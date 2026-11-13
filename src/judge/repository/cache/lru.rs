use std::{num::NonZeroUsize, sync::Mutex};

use ::lru::LruCache;

use super::Cache;

// A fixed-capacity cache that evicts the least recently used entry.
pub struct Lru<T> {
    /*
    A cache hit updates the global recency order. The same lock keeps
    insertion and eviction consistent with that order and the capacity limit.
    A concurrent map like scc::HashMap synchronizes entry access. The global
    recency order and capacity limit would still need coordination.
    */
    entries: Mutex<LruCache<String, T>>,
}

impl<T> Lru<T> {
    pub fn new(capacity: NonZeroUsize) -> Self {
        Self {
            entries: Mutex::new(LruCache::new(capacity)),
        }
    }
}

impl<T: Clone + Send> Cache<T> for Lru<T> {
    fn get(&self, key: &str) -> Option<T> {
        self.entries.lock().unwrap().get(key).cloned()
    }

    fn insert(&self, key: String, value: T) {
        self.entries.lock().unwrap().put(key, value);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accessing_an_entry_protects_it_from_the_next_eviction() {
        let cache = Lru::new(NonZeroUsize::new(2).unwrap());
        cache.insert("first".into(), 1);
        cache.insert("second".into(), 2);
        assert_eq!(cache.get("first"), Some(1));
        cache.insert("third".into(), 3);

        assert_eq!(cache.get("second"), None);
        assert_eq!(cache.get("first"), Some(1));
        assert_eq!(cache.get("third"), Some(3));
    }
}
