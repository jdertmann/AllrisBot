use std::collections::{BTreeSet, HashMap, VecDeque};
use std::future::Future;
use std::hash::Hash;
use std::ops::DerefMut;
use std::pin::*;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tokio::sync::{mpsc, Notify, OwnedSemaphorePermit, Semaphore};
use tokio::time::timeout_at;
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
        let _ = tokio::spawn(async move {
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
    type Chat: Send + Copy + Hash + Eq;

    fn is_reply(&self) -> bool;

    fn get_chat(&self) -> Self::Chat;

    fn process(
        self,
        p: &Self::Params,
    ) -> impl Future<Output = Result<(), QueueEntryError<Self>>> + Send;

    fn delete(self, p: &Self::Params) -> impl Future<Output = ()> + Send;
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
    HardShutdown,
    SoftShutdown,
}

async fn chat_task<E: QueueEntry>(
    params: E::Params,
    queue: Arc<Queue<E>>,
    broadcast_limiter: SemaphoreRateLimiter,
    mut rx: mpsc::UnboundedReceiver<Msg>,
) {
    let mut entries = pin!(queue.entry_stream(broadcast_limiter));

    let mut current_entry = None;
    let mut retry_timer = pin!(tokio::time::sleep(Duration::ZERO));
    let mut soft_shutdown = false;

    loop {
        tokio::select! {
            biased;
            msg = rx.recv() => {
                match msg {
                    None | Some(Msg::HardShutdown) => break,
                    Some(Msg::SoftShutdown) => {
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
                        permit.map(|x| x.sent());
                    }
                    Err(QueueEntryError::Retry { entry, retry_after }) => {
                        current_entry = Some((permit, entry));
                        retry_timer.set(tokio::time::sleep(retry_after));
                    },
                    Err(QueueEntryError::ChatInvalid) => {
                        permit.map(|x| x.sent()); // don't know if necessary?
                        queue.invalidate();
                        break;
                    }
                }
            },
            () = std::future::ready(()), if soft_shutdown && current_entry.is_none() => {
                // This will be called only if
                // - soft_shutdown is set to true
                // - there is no entry currently being processed
                // - none of the above futures finish immediately (due to `biased`), in particular the queue is empty
                break
            }
        }
    }

    if queue.invalidated() {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(60);
        while let Ok(entry) = timeout_at(deadline, queue.get_next_entry(false)).await {
            entry.delete(&params).await;
        }
    }
}

pub struct ChatTask<E: QueueEntry> {
    tx: mpsc::UnboundedSender<Msg>,
    queue: Arc<Queue<E>>,
    handle: tokio::task::JoinHandle<()>,
}

impl<E: QueueEntry> ChatTask<E> {
    pub fn new(params: E::Params, broadcast_limiter: SemaphoreRateLimiter) -> Self {
        let (tx, rx) = mpsc::unbounded_channel();
        let queue: Arc<Queue<E>> = Arc::new(Queue::new());
        let handle = tokio::spawn(chat_task(params, queue.clone(), broadcast_limiter, rx));
        ChatTask { tx, queue, handle }
    }

    pub fn queue(&self) -> &Queue<E> {
        &self.queue
    }

    // Stop as soon as the queue is empty
    pub fn soft_shutdown(&self) {
        let _ = self.tx.send(Msg::SoftShutdown);
    }

    /// Stop as soon as possible. Messages that are not sent are persisted
    pub fn hard_shutdown(&self) {
        let _ = self.tx.send(Msg::HardShutdown);
    }

    pub fn join_handle(&mut self) -> &mut tokio::task::JoinHandle<()> {
        &mut self.handle
    }
}

pub struct MessageDispatcher<E: QueueEntry> {
    params: E::Params,
    broadcast_limiter: SemaphoreRateLimiter,
    tasks: Mutex<HashMap<E::Chat, ChatTask<E>>>,
}

impl<E: QueueEntry> MessageDispatcher<E>
where
    E::Params: Clone,
{
    pub fn new(params: E::Params) -> Self {
        let broadcast_limiter =
            SemaphoreRateLimiter::new(BROADCASTS_PER_SECOND, Duration::from_secs(1));
        let tasks = Mutex::new(HashMap::new());

        MessageDispatcher {
            params,
            broadcast_limiter,
            tasks,
        }
    }

    pub fn dispatch(&self, entry: E) {
        let mut tasks = self.tasks.lock().expect("shouldn't be poisoned");
        let task = tasks
            .entry(entry.get_chat())
            .or_insert_with(|| ChatTask::new(self.params.clone(), self.broadcast_limiter.clone()));

        task.queue().add_item(entry);
    }
}
