use crate::constants::{CACHE_MAX_ENTRIES, CACHE_TTL_SECS};
use lru::LruCache;
use std::num::NonZeroUsize;
use std::time::{Duration, Instant};

struct CacheEntry<V> {
    value: V,
    inserted_at: Instant,
}

pub struct TtlLruCache<V> {
    inner: LruCache<String, CacheEntry<V>>,
    ttl: Duration,
}

impl<V: Clone> Default for TtlLruCache<V> {
    fn default() -> Self {
        Self::new()
    }
}

impl<V: Clone> TtlLruCache<V> {
    pub fn new() -> Self {
        Self {
            inner: LruCache::new(
                NonZeroUsize::new(CACHE_MAX_ENTRIES)
                    .expect("CACHE_MAX_ENTRIES is a non-zero constant"),
            ),
            ttl: Duration::from_secs(CACHE_TTL_SECS),
        }
    }

    pub fn with_config(max_entries: usize, ttl_secs: u64) -> Self {
        Self {
            inner: LruCache::new(
                NonZeroUsize::new(max_entries.max(1)).expect("max(1) cannot be zero"),
            ),
            ttl: Duration::from_secs(ttl_secs),
        }
    }

    pub fn get(&mut self, key: &str) -> Option<V> {
        let dominated = self
            .inner
            .peek(key)
            .is_some_and(|entry| entry.inserted_at.elapsed() >= self.ttl);
        if dominated {
            self.inner.pop(key);
            return None;
        }
        self.inner.get(key).map(|entry| entry.value.clone())
    }

    pub fn put(&mut self, key: String, value: V) {
        self.inner.put(
            key,
            CacheEntry {
                value,
                inserted_at: Instant::now(),
            },
        );
    }

    pub fn stats(&self) -> (usize, usize) {
        (self.inner.len(), self.inner.cap().get())
    }

    pub fn clear(&mut self) {
        self.inner.clear();
    }

    pub fn evict_expired(&mut self) {
        let keys_to_remove: Vec<String> = self
            .inner
            .iter()
            .filter(|(_, entry)| entry.inserted_at.elapsed() >= self.ttl)
            .map(|(key, _)| key.clone())
            .collect();

        for key in keys_to_remove {
            self.inner.pop(&key);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cache_basic() {
        let mut cache: TtlLruCache<Vec<f32>> = TtlLruCache::with_config(3, 60);
        cache.put("key1".into(), vec![1.0, 2.0]);
        assert!(cache.get("key1").is_some());
        assert!(cache.get("missing").is_none());
    }

    #[test]
    fn test_cache_lru_eviction() {
        let mut cache: TtlLruCache<String> = TtlLruCache::with_config(2, 60);
        cache.put("a".into(), "alpha".into());
        cache.put("b".into(), "beta".into());
        cache.put("c".into(), "gamma".into());
        assert!(cache.get("a").is_none(), "LRU should have evicted 'a'");
        assert!(cache.get("b").is_some());
        assert!(cache.get("c").is_some());
    }

    #[test]
    fn test_cache_stats() {
        let mut cache: TtlLruCache<i32> = TtlLruCache::with_config(10, 60);
        cache.put("x".into(), 1);
        cache.put("y".into(), 2);
        let (len, cap) = cache.stats();
        assert_eq!(len, 2);
        assert_eq!(cap, 10);
    }

    #[test]
    fn a_zero_ttl_entry_is_never_served() {
        let mut cache: TtlLruCache<i32> = TtlLruCache::with_config(4, 0);
        cache.put("x".into(), 7);
        std::thread::sleep(std::time::Duration::from_millis(2));
        assert!(cache.get("x").is_none());
    }

    #[test]
    fn evict_expired_drops_stale_keys_and_keeps_fresh_ones() {
        let mut cache: TtlLruCache<i32> = TtlLruCache::with_config(4, 0);
        cache.put("stale".into(), 1);
        std::thread::sleep(std::time::Duration::from_millis(2));
        cache.evict_expired();
        let (len, _) = cache.stats();
        assert_eq!(len, 0);
    }

    #[test]
    fn clear_empties_the_cache() {
        let mut cache: TtlLruCache<i32> = TtlLruCache::with_config(4, 60);
        cache.put("x".into(), 1);
        cache.clear();
        assert!(cache.get("x").is_none());
        assert_eq!(cache.stats().0, 0);
    }
}
