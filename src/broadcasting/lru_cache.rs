use std::collections::{HashMap, VecDeque};
use std::hash::Hash;
use std::ops::Deref;
use std::sync::Arc;

use tokio::sync::{Mutex, OnceCell};

pub trait EvictionStrategy<K> {
    fn insert(&mut self, key: K, is_present: bool) -> Option<K>;

    fn initial_capacity(&self) -> usize {
        0
    }
}

pub struct Lru<K> {
    capacity: usize,
    lru: VecDeque<K>,
}

impl<K> Lru<K> {
    pub fn new(capacity: usize) -> Self {
        assert_ne!(capacity, 0);

        Self {
            capacity,
            lru: VecDeque::with_capacity(capacity + 1),
        }
    }
}
/*
pub struct NoEviction;

impl<K> EvictionStrategy<K> for NoEviction {
    fn insert(&mut self, _key: K, is_present: bool) -> Option<K> {
        None
    }
}*/

impl<K: Eq> EvictionStrategy<K> for Lru<K> {
    fn insert(&mut self, key: K, is_present: bool) -> Option<K> {
        if is_present {
            if let Some(index) = self.lru.iter().position(|k| *k == key) {
                self.lru.remove(index);
            }

            self.lru.push_front(key);

            None
        } else {
            self.lru.push_front(key);

            if self.lru.len() > self.capacity {
                self.lru.pop_back()
            } else {
                None
            }
        }
    }

    fn initial_capacity(&self) -> usize {
        self.capacity
    }
}

struct CacheInner<K, V, E> {
    cache: HashMap<K, Arc<OnceCell<V>>>,
    eviction_strategy: E,
}

impl<K: Eq + Hash + Copy, V, E: EvictionStrategy<K>> CacheInner<K, V, E> {
    fn new(eviction_strategy: E) -> Self {
        CacheInner {
            cache: HashMap::with_capacity(eviction_strategy.initial_capacity() + 1),
            eviction_strategy,
        }
    }

    fn get(&mut self, key: K) -> Arc<OnceCell<V>> {
        if let Some(evict) = self
            .eviction_strategy
            .insert(key, self.cache.contains_key(&key))
        {
            self.cache.remove(&evict);
        }

        self.cache.entry(key).or_default().clone()
    }
}

pub struct CacheItem<V>(Arc<OnceCell<V>>);

impl<V> Deref for CacheItem<V> {
    type Target = V;

    fn deref(&self) -> &Self::Target {
        self.0.get().expect("invariant: cell is initialized")
    }
}
pub struct Cache<K, V, E> {
    inner: Mutex<CacheInner<K, V, E>>,
}

impl<K: Eq + Hash + Copy, V, E: EvictionStrategy<K>> Cache<K, V, E> {
    pub fn new(eviction_strategy: E) -> Self {
        Self {
            inner: Mutex::new(CacheInner::new(eviction_strategy)),
        }
    }

    pub async fn get<Err, Fut: Future<Output = Result<V, Err>>>(
        &self,
        key: K,
        init: impl FnOnce() -> Fut,
    ) -> Result<CacheItem<V>, Err> {
        let cell = self.inner.lock().await.get(key);

        cell.get_or_try_init(init).await?;

        Ok(CacheItem(cell))
    }

    pub async fn get_some<Err, Fut: Future<Output = Result<Option<V>, Err>>>(
        &self,
        key: K,
        init: impl FnOnce() -> Fut,
    ) -> Result<Option<CacheItem<V>>, Err> {
        let init2 = || async {
            match init().await {
                Ok(Some(r)) => Ok(r),
                Ok(None) => Err(None),
                Err(e) => Err(Some(e)),
            }
        };

        match self.get(key, init2).await {
            Ok(next) => Ok(Some(next)),
            Err(None) => Ok(None),
            Err(Some(e)) => Err(e),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use futures_util::FutureExt;
    use tokio::sync::Barrier;

    use super::*;

    macro_rules! ok {
        ($i:expr) => {
            || async { Ok::<_, ()>($i) }
        };
    }

    #[tokio::test]
    async fn test_cache_insert_and_get() {
        let cache = Cache::new(Lru::new(2));

        let value1 = cache.get(1, ok!(10)).await.unwrap();
        assert_eq!(*value1, 10);

        let value2 = cache.get(2, ok!(20)).await.unwrap();
        assert_eq!(*value2, 20);

        let value1_again = cache.get(1, ok!(100)).await.unwrap();
        assert_eq!(*value1_again, 10); // Should not replace existing value
    }

    #[tokio::test]
    async fn test_cache_eviction() {
        let cache = Cache::new(Lru::new(2));

        cache.get(1, ok!(10)).await.unwrap();
        cache.get(2, ok!(20)).await.unwrap();
        cache.get(3, ok!(30)).await.unwrap(); // Evicts key 1

        let value = cache.get(1, ok!(100)).await.unwrap();
        assert_eq!(*value, 100); // Key 1 was evicted, so it should be re-initialized

        let value2 = cache.get(3, ok!(300)).await.unwrap();
        assert_eq!(*value2, 30); // Key 3 should still be present
    }

    #[tokio::test]
    async fn test_lru_eviction_order() {
        let mut lru = Lru::new(2);

        assert_eq!(lru.insert(1, false), None);
        assert_eq!(lru.insert(2, false), None);
        assert_eq!(lru.insert(3, false), Some(1)); // Evicts 1

        lru.insert(2, true); // Move 2 to the front
        assert_eq!(lru.insert(4, false), Some(3)); // Evicts 3, as 2 was accessed recently
    }

    #[tokio::test]
    async fn test_concurrent_cache_access() {
        let cache = Arc::new(Cache::new(Lru::new(3)));
        let barrier = Arc::new(Barrier::new(5));

        let mut handles = vec![];
        for i in 0..5 {
            let cache = Arc::clone(&cache);
            let barrier = Arc::clone(&barrier);
            handles.push(tokio::spawn(async move {
                barrier.wait().await;
                cache
                    .get((), || async {
                        tokio::time::sleep(Duration::from_millis(200 + 5 * i)).await;
                        Ok::<_, ()>(i * 10)
                    })
                    .await
                    .unwrap()
            }));
        }

        for handle in handles {
            let result = handle.await.unwrap();
            let expected = cache
                .get((), || async { Err(()) })
                .now_or_never()
                .unwrap()
                .unwrap();
            assert!(*result == *expected);
        }
    }
}
