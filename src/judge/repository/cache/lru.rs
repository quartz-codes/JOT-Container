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
    entries: Mutex<LruCache<(i64, i64), T>>,
}

impl<T> Lru<T> {
    pub fn new(capacity: NonZeroUsize) -> Self {
        Self {
            entries: Mutex::new(LruCache::new(capacity)),
        }
    }
}

impl<T: Clone + Send> Cache<T> for Lru<T> {
    fn get(&self, key: &(i64, i64)) -> Option<T> {
        self.entries.lock().unwrap().get(key).cloned()
    }

    fn insert(&self, key: (i64, i64), value: T) {
        self.entries.lock().unwrap().put(key, value);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accessing_an_entry_protects_it_from_the_next_eviction() {
        let cache = Lru::new(NonZeroUsize::new(2).unwrap());
        cache.insert((1, 7), 1);
        cache.insert((1, 12), 2);
        assert_eq!(cache.get(&(1, 7)), Some(1));
        cache.insert((2, 7), 3);

        assert_eq!(cache.get(&(1, 12)), None);
        assert_eq!(cache.get(&(1, 7)), Some(1));
        assert_eq!(cache.get(&(2, 7)), Some(3));
    }
}
