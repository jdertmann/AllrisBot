use std::collections::hash_map::Entry;
use std::collections::{HashMap, VecDeque};
use std::hash::Hash;
use std::ops::Deref;
use std::sync::Arc;

use tokio::sync::{Mutex, OnceCell};

struct CacheInner<K, V> {
    capacity: usize,
    cache: HashMap<K, Arc<OnceCell<V>>>,
    lru: VecDeque<K>,
}

impl<K: Eq + Hash + Copy, V> CacheInner<K, V> {
    fn new(capacity: usize) -> CacheInner<K, V> {
        assert_ne!(capacity, 0);

        CacheInner {
            cache: HashMap::with_capacity(capacity),
            lru: VecDeque::with_capacity(capacity),
            capacity,
        }
    }

    fn get(&mut self, key: K) -> Arc<OnceCell<V>> {
        match self.cache.entry(key) {
            Entry::Occupied(entry) => {
                let cell = Arc::clone(entry.get());
                if let Some(index) = self.lru.iter().position(|k| *k == key) {
                    self.lru.remove(index);
                }
                self.lru.push_front(key);
                cell
            }
            Entry::Vacant(entry) => {
                let cell = Arc::new(OnceCell::new());
                entry.insert(cell.clone());
                if self.lru.len() >= self.capacity {
                    let oldest_item = self.lru.pop_back().unwrap();
                    self.cache.remove(&oldest_item);
                }
                self.lru.push_front(key);
                cell
            }
        }
    }
}

pub struct CacheItem<V>(Arc<OnceCell<V>>);

impl<V> Deref for CacheItem<V> {
    type Target = V;

    fn deref(&self) -> &Self::Target {
        self.0.get().expect("invariant: cell is initialized")
    }
}
pub struct Cache<K, V> {
    inner: Mutex<CacheInner<K, V>>,
}

impl<K: Eq + Hash + Copy, V> Cache<K, V> {
    pub fn new(capacity: usize) -> Self {
        Self {
            inner: Mutex::new(CacheInner::new(capacity)),
        }
    }

    pub async fn get<E, Fut: Future<Output = Result<V, E>>>(
        &self,
        key: K,
        init: impl FnOnce() -> Fut,
    ) -> Result<CacheItem<V>, E> {
        let cell = self.inner.lock().await.get(key);

        cell.get_or_try_init(init).await?;

        Ok(CacheItem(cell))
    }
}
