use std::collections::{BTreeSet, HashMap, VecDeque};
use std::future::Future;
use std::hash::Hash;
use std::ops::DerefMut;
use std::pin::*;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tokio::sync::{mpsc, Notify, OwnedSemaphorePermit, Semaphore};
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

pub trait QueueEntry: Ord + Send + 'static {
    type Params: Send;
    type Chat: Send + Copy + Hash + Eq;

    fn is_reply(&self) -> bool;

    fn get_chat(&self) -> Self::Chat;

    fn process(self, p: &Self::Params) -> impl Future<Output = ()> + Send;
}

pub struct Queue<E: QueueEntry> {
    queue: Mutex<BTreeSet<E>>,
    element_added: Notify,
}

impl<E: QueueEntry> Queue<E> {
    fn new() -> Self {
        Self {
            queue: Default::default(),
            element_added: Notify::new(),
        }
    }

    fn lock_queue(&self) -> impl DerefMut<Target = BTreeSet<E>> + '_ {
        self.queue.lock().expect("Mutex should not be poisoned")
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
    Shutdown,
}

async fn chat_task<E: QueueEntry>(
    params: E::Params,
    queue: Arc<Queue<E>>,
    broadcast_limiter: SemaphoreRateLimiter,
    mut rx: mpsc::UnboundedReceiver<Msg>,
) {
    let entries = async_stream::stream! {
        loop {
            let next_entry = queue.get_next_entry(false).await;
            if next_entry.is_reply() {
                yield (None, next_entry);
                continue;
            }
            let mut acquire = pin!(broadcast_limiter.acquire());
            loop {
                tokio::select! {
                    next_reply = queue.get_next_entry(true) => {
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

    let mut entries = pin!(async_stream::stream! {
        tick_interval.tick().await;
        for await entry in entries {
            yield entry;
            tick_interval.tick().await;
        }
    });

    loop {
        tokio::select! {
            x = entries.next() => {
                let (permit, entry) = x.expect("Stream will never end");
                entry.process(&params).await;
                permit.map(|x| x.sent());
            },
            msg = rx.recv() => match msg {
                None | Some(Msg::Shutdown) => return,
            }
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

    pub async fn shutdown(self) {
        self.tx.send(Msg::Shutdown).unwrap();
        let _ = self.handle.await;
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
