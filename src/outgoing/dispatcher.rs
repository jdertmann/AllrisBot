use std::collections::{BTreeSet, HashMap, VecDeque};
use std::fmt::Debug;
use std::future::Future;
use std::hash::Hash;
use std::ops::DerefMut;
use std::pin::*;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tokio::sync::{mpsc, Notify, OwnedSemaphorePermit, Semaphore};
use tokio::task::JoinHandle;
use tokio::time::{interval, timeout_at, MissedTickBehavior};
use tokio_stream::{Stream, StreamExt};

const GROUP_MESSAGES_PER_MINUTE: usize = 20;
const CHAT_MESSAGES_PER_SECOND: usize = 1;
const BROADCASTS_PER_SECOND: usize = 30;

#[derive(Clone)]
pub struct SemaphoreRateLimiter {
    period: Duration,
    semaphore: Arc<Semaphore>,
}

impl SemaphoreRateLimiter {
    pub fn new(max_requests: usize, period: Duration) -> Self {
        SemaphoreRateLimiter {
            period,
            semaphore: Arc::new(Semaphore::new(max_requests)),
        }
    }

    pub async fn acquire(&self) -> RateLimiterPermit {
        let permit = Arc::clone(&self.semaphore).acquire_owned().await.unwrap();
        RateLimiterPermit {
            period: self.period,
            permit,
        }
    }
}

pub struct RateLimiterPermit {
    period: Duration,
    permit: OwnedSemaphorePermit,
}

impl RateLimiterPermit {
    pub fn sent(self) {
        let sleep = tokio::time::sleep(self.period);
        tokio::spawn(async move {
            sleep.await;
            drop(self.permit)
        });
    }
}

struct SlidingWindowRateLimiter {
    period: Duration,
    max_requests: usize,
    history: VecDeque<Instant>,
}

impl SlidingWindowRateLimiter {
    fn new(period: Duration, max_requests: usize) -> Self {
        assert!(max_requests > 0, "max_requests must be at least 1");
        Self {
            period,
            max_requests,
            history: VecDeque::with_capacity(max_requests),
        }
    }

    fn prune(&mut self) {
        while self
            .history
            .front()
            .map_or(false, |&t| t.elapsed() > self.period)
        {
            self.history.pop_front().unwrap();
        }
    }

    fn can_send(&mut self) -> bool {
        self.prune();
        self.history.len() < self.max_requests
    }

    async fn wait_until_available(&mut self) {
        while !self.can_send() {
            let index = self.history.len() - self.max_requests; // usually equals 0
            let next = self.history[index];
            tokio::time::sleep_until((next + self.period).into()).await;
        }
    }

    fn register_send(&mut self) {
        self.prune();
        self.history.push_back(Instant::now());
    }
}

pub trait QueueEntry: Ord + Send + Sized + 'static {
    type Params: Send;
    type Chat: Send + Copy + Hash + Eq + Debug;

    fn is_reply(&self) -> bool;

    fn get_chat(&self) -> Self::Chat;

    fn process(
        self,
        p: &Self::Params,
    ) -> impl Future<Output = Result<(), QueueEntryError<Self>>> + Send;

    fn delete(self, p: &Self::Params) -> impl Future<Output = ()> + Send;

    fn get_all(p: &Self::Params) -> impl Future<Output = Vec<Self>> + Send;
}

pub enum QueueEntryError<Q: QueueEntry> {
    Retry { entry: Q, retry_after: Duration },
    ChatInvalid,
}

pub struct Queue<E: QueueEntry> {
    queue: Mutex<BTreeSet<E>>,
    element_added: Notify,
    invalidated: AtomicBool,
}

impl<E: QueueEntry> Queue<E> {
    fn new() -> Self {
        Self {
            queue: Default::default(),
            element_added: Notify::new(),
            invalidated: AtomicBool::new(false),
        }
    }

    fn lock_queue(&self) -> impl DerefMut<Target = BTreeSet<E>> + '_ {
        self.queue.lock().expect("Mutex should not be poisoned")
    }

    fn invalidate(&self) {
        self.invalidated.store(true, Ordering::Relaxed)
    }

    pub fn invalidated(&self) -> bool {
        self.invalidated.load(Ordering::Relaxed)
    }

    pub fn add_item(&self, item: E) {
        self.lock_queue().insert(item);
        self.element_added.notify_one();
    }

    pub fn add_items(&self, items: impl IntoIterator<Item = E>) -> usize {
        let added_items;
        {
            let mut queue = self.lock_queue();
            let old_size = queue.len();
            queue.extend(items);
            added_items = queue.len() - old_size;
        }
        self.element_added.notify_one();
        added_items
    }

    pub async fn get_next_entry(&self, only_replies: bool) -> E {
        loop {
            let item;

            {
                let mut queue_ref = self.lock_queue();
                if !only_replies || queue_ref.first().map_or(false, E::is_reply) {
                    item = queue_ref.pop_first();
                } else {
                    item = None;
                }
            }

            if let Some(item) = item {
                return item;
            }

            self.element_added.notified().await
        }
    }

    fn entry_stream(
        &self,
        broadcast_limiter: SemaphoreRateLimiter,
    ) -> impl Stream<Item = (Option<RateLimiterPermit>, E)> + '_ {
        let entries = async_stream::stream! {
            loop {
                let next_entry = self.get_next_entry(false).await;
                if next_entry.is_reply() {
                    yield (None, next_entry);
                    continue;
                }

                let mut acquire = pin!(broadcast_limiter.acquire());
                loop {
                    tokio::select! {
                        next_reply = self.get_next_entry(true) => {
                            yield (None, next_reply);
                        },
                        permit = &mut acquire => {
                            yield (Some(permit), next_entry);
                            break;
                        },
                    }
                }
            }
        };

        let entries = throttle(entries, Duration::from_secs(1), CHAT_MESSAGES_PER_SECOND);

        let mut interval = 1. / CHAT_MESSAGES_PER_SECOND as f32;

        let entries: Pin<Box<dyn Stream<Item = _> + Send>> = if true {
            interval = f32::max(interval, 60. / GROUP_MESSAGES_PER_MINUTE as f32);
            let entries = throttle(entries, Duration::from_secs(60), GROUP_MESSAGES_PER_MINUTE);
            Box::pin(entries)
        } else {
            Box::pin(entries)
        };

        let mut tick_interval = tokio::time::interval(Duration::from_secs_f32(interval));

        let entries = async_stream::stream! {
            tick_interval.tick().await;
            for await entry in entries {
                yield entry;
                if self.invalidated() {
                    break
                }
                tick_interval.tick().await;
            }
        };

        entries
    }
}

fn throttle<T>(
    s: impl Stream<Item = T>,
    period: Duration,
    max_requests: usize,
) -> impl Stream<Item = T> {
    async_stream::stream! {
        let mut limiter = SlidingWindowRateLimiter::new(period, max_requests);
        for await entry in s {
            limiter.wait_until_available().await;
            yield entry;
            limiter.register_send();
        }
    }
}
enum Msg {
    Shutdown { hard: bool },
}

async fn chat_task<E: QueueEntry>(
    params: E::Params,
    queue: Arc<Queue<E>>,
    broadcast_limiter: SemaphoreRateLimiter,
    on_exit: impl FnOnce(),
    mut rx: mpsc::UnboundedReceiver<Msg>,
) {
    let mut entries = pin!(queue.entry_stream(broadcast_limiter));

    let mut current_entry = None;
    let mut retry_timer = pin!(tokio::time::sleep(Duration::ZERO));
    let mut soft_shutdown = false;

    loop {
        if soft_shutdown && current_entry.is_none() && queue.lock_queue().is_empty() {
            break;
        }

        tokio::select! {
            biased;
            msg = rx.recv() => {
                match msg {
                    None | Some(Msg::Shutdown { hard: true }) => break,
                    Some(Msg::Shutdown { hard: false }) => {
                        soft_shutdown = true;
                    }
                }
            }
            entry = entries.next(), if current_entry.is_none() => {
                if entry.is_some() {
                    current_entry = entry;
                    retry_timer.set(tokio::time::sleep(Duration::ZERO));
                }
            }
            () = &mut retry_timer, if current_entry.is_some() => {
                let (permit, entry) = current_entry.take().unwrap();
                match entry.process(&params).await {
                    Ok(()) => {
                        if let Some(permit) = permit {
                            permit.sent();
                        }                     }
                    Err(QueueEntryError::Retry { entry, retry_after }) => {
                        current_entry = Some((permit, entry));
                        retry_timer.set(tokio::time::sleep(retry_after));
                    },
                    Err(QueueEntryError::ChatInvalid) => {
                        if let Some(permit) = permit {
                            permit.sent();
                        }
                        queue.invalidate();
                        break;
                    }
                }
            },
        }
    }

    if queue.invalidated() {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(60);
        while let Ok(entry) = timeout_at(deadline, queue.get_next_entry(false)).await {
            entry.delete(&params).await;
        }
    }

    on_exit()
}

pub struct ChatTask<E: QueueEntry> {
    tx: mpsc::UnboundedSender<Msg>,
    queue: Arc<Queue<E>>,
}

impl<E: QueueEntry> ChatTask<E> {
    pub fn new(
        params: E::Params,
        broadcast_limiter: SemaphoreRateLimiter,
        on_exit: impl FnOnce() + Send + 'static,
    ) -> Self {
        let (tx, rx) = mpsc::unbounded_channel();
        let queue: Arc<Queue<E>> = Arc::new(Queue::new());
        tokio::spawn(chat_task(
            params,
            queue.clone(),
            broadcast_limiter,
            on_exit,
            rx,
        ));
        ChatTask { tx, queue }
    }

    pub fn queue(&self) -> &Queue<E> {
        &self.queue
    }

    pub fn shutdown(&self, hard: bool) {
        let _ = self.tx.send(Msg::Shutdown { hard });
    }
}

enum DispatcherMsg<E: QueueEntry> {
    Enqueue(E),
    Shutdown { hard: bool },
    DropChatTask(E::Chat),
    CheckOrphaned,
}

pub struct DispatcherTask<E: QueueEntry> {
    tx: mpsc::UnboundedSender<DispatcherMsg<E>>,
    handle: JoinHandle<()>,
}

struct DispatcherTaskInner<E: QueueEntry> {
    params: E::Params,
    limiter: SemaphoreRateLimiter,
    tx: mpsc::UnboundedSender<DispatcherMsg<E>>,
}

impl<E: QueueEntry> DispatcherTaskInner<E>
where
    E::Params: Clone,
{
    fn create_chat(&self, chat: E::Chat) -> ChatTask<E> {
        let tx = self.tx.clone();
        let on_exit = move || {
            let _ = tx.send(DispatcherMsg::DropChatTask(chat));
        };
        ChatTask::new(self.params.clone(), self.limiter.clone(), on_exit)
    }
}

impl<E: QueueEntry> DispatcherTask<E>
where
    E::Params: Clone,
{
    pub fn new(params: E::Params) -> Self {
        let (tx, mut rx) = mpsc::unbounded_channel::<DispatcherMsg<E>>();

        let tx2 = tx.clone();

        let handle = tokio::spawn(async move {
            let inner = DispatcherTaskInner {
                limiter: SemaphoreRateLimiter::new(BROADCASTS_PER_SECOND, Duration::from_secs(1)),
                params,
                tx: tx2,
            };

            let mut tasks = HashMap::new();
            let mut orphaned: HashMap<_, Vec<_>> = HashMap::new();
            let mut shutdown = false;

            let mut check_orphaned_interval = interval(Duration::from_secs(120));
            check_orphaned_interval.set_missed_tick_behavior(MissedTickBehavior::Delay);

            loop {
                let msg = tokio::select! {
                    biased;
                    _ = check_orphaned_interval.tick() => DispatcherMsg::CheckOrphaned,
                    msg = rx.recv() => match msg {
                        Some(msg) => msg,
                        None => break
                    },
                };

                match msg {
                    DispatcherMsg::Enqueue(e) => {
                        if shutdown {
                            continue;
                        }

                        let chat = e.get_chat();
                        let task = tasks.entry(chat).or_insert_with(|| inner.create_chat(chat));
                        task.queue().add_item(e);
                    }
                    DispatcherMsg::DropChatTask(chat) => {
                        tasks.remove(&chat);
                    }
                    DispatcherMsg::Shutdown { hard } => {
                        shutdown = true;
                        for task in tasks.values() {
                            task.shutdown(hard);
                        }
                    }
                    DispatcherMsg::CheckOrphaned => {
                        let keys = E::get_all(&inner.params).await;
                        for key in keys {
                            let key: E = key;
                            let chat = key.get_chat();
                            orphaned.entry(chat).or_default().push(key);
                        }

                        tasks.retain(|chat, task| {
                            if task.tx.is_closed() {
                                log::warn!("Task for chat {chat:#?} was found dead");
                                return false;
                            }

                            if let Some(keys) = orphaned.remove(chat) {
                                let added = task.queue().add_items(keys);
                                log::info!("Found {added} orphaned messages for chat {chat:#?}");
                            }

                            true
                        });

                        for (chat, keys) in orphaned.drain() {
                            let task = tasks.entry(chat).or_insert_with(|| inner.create_chat(chat));
                            let added = task.queue().add_items(keys);
                            log::info!("Found {added} orphaned messages for chat {chat:#?}");
                        }
                    }
                }

                if shutdown && tasks.is_empty() {
                    break;
                }
            }
        });

        Self { tx, handle }
    }

    pub fn sender(&self) -> MessageDispatcher<E> {
        MessageDispatcher {
            tx: self.tx.clone(),
        }
    }

    pub fn shutdown(&self, hard: bool) {
        let _ = self.tx.send(DispatcherMsg::Shutdown { hard });
    }

    pub fn join_handle(&mut self) -> &mut JoinHandle<()> {
        &mut self.handle
    }
}

pub struct MessageDispatcher<E: QueueEntry> {
    tx: mpsc::UnboundedSender<DispatcherMsg<E>>,
}

impl<E: QueueEntry> MessageDispatcher<E> {
    pub fn dispatch(&self, entry: E) {
        let _ = self.tx.send(DispatcherMsg::Enqueue(entry));
    }
}

impl<E: QueueEntry> Clone for MessageDispatcher<E> {
    fn clone(&self) -> Self {
        Self {
            tx: self.tx.clone(),
        }
    }
}
