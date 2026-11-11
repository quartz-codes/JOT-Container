use std::{
    collections::{HashMap, hash_map::Entry},
    num::NonZeroUsize,
    sync::{Arc, Mutex},
};

use lru::LruCache;
use tokio::sync::mpsc;

use super::{FileKey, S3Repository, S3RepositoryRequestError};

#[derive(Clone, Debug)]
pub enum CachedS3Error<E> {
    S3RequestFailure(S3RepositoryRequestError),
    DecodeFailure(E),
    DecoderTaskFailed(String),
}

impl<E: std::fmt::Display> std::fmt::Display for CachedS3Error<E> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::S3RequestFailure(S3RepositoryRequestError::S3RequestFailure(message)) => {
                write!(f, "S3 download failed: {message}")
            }
            Self::DecodeFailure(error) => write!(f, "S3 object decoding failed: {error}"),
            Self::DecoderTaskFailed(error) => write!(f, "S3 object decoder task failed: {error}"),
        }
    }
}

type Decoder<T, E> = dyn Fn(&[u8]) -> Result<Vec<T>, E> + Send + Sync;

pub struct CachedS3Repository<T, E> {
    repository: Arc<S3Repository>,
    decoder: Arc<Decoder<T, E>>,
    cache: Arc<Mutex<CacheState<T, E>>>,
}

impl<T, E> CachedS3Repository<T, E>
where
    T: Clone + Send + Sync + 'static,
    E: Clone + Send + 'static,
{
    pub fn new(
        repository: Arc<S3Repository>,
        capacity: NonZeroUsize,
        decoder: impl Fn(&[u8]) -> Result<Vec<T>, E> + Send + Sync + 'static,
    ) -> Self {
        Self {
            repository,
            decoder: Arc::new(decoder),
            cache: Arc::new(Mutex::new(CacheState::new(capacity))),
        }
    }

    pub async fn fetch(
        &self,
        object_key: String,
    ) -> mpsc::UnboundedReceiver<Result<T, CachedS3Error<E>>> {
        let etag = match self.repository.get_etag(&object_key).await {
            Ok(etag) => etag,
            Err(error) => {
                let (tx, rx) = mpsc::unbounded_channel();
                let _ = tx.send(Err(CachedS3Error::S3RequestFailure(error)));
                return rx;
            }
        };
        let key = FileKey { object_key, etag };
        let repository = Arc::clone(&self.repository);
        let decoder = Arc::clone(&self.decoder);
        get_or_load(&self.cache, key.clone(), async move {
            let mut chunks = repository.start_or_subscribe(key);
            let mut bytes = Vec::new();
            while let Some(chunk) = chunks.recv().await {
                bytes.extend_from_slice(&chunk.map_err(CachedS3Error::S3RequestFailure)?);
            }
            tokio::task::spawn_blocking(move || decoder(&bytes))
                .await
                .map_err(|error| CachedS3Error::DecoderTaskFailed(error.to_string()))?
                .map_err(CachedS3Error::DecodeFailure)
        })
    }
}

struct CacheState<T, E> {
    ready: LruCache<FileKey, Arc<Vec<T>>>,
    waiting: HashMap<FileKey, Vec<mpsc::UnboundedSender<Result<T, CachedS3Error<E>>>>>,
}

impl<T, E> CacheState<T, E> {
    fn new(capacity: NonZeroUsize) -> Self {
        Self {
            ready: LruCache::new(capacity),
            waiting: HashMap::new(),
        }
    }
}

fn get_or_load<T, E>(
    cache: &Arc<Mutex<CacheState<T, E>>>,
    key: FileKey,
    load: impl Future<Output = Result<Vec<T>, CachedS3Error<E>>> + Send + 'static,
) -> mpsc::UnboundedReceiver<Result<T, CachedS3Error<E>>>
where
    T: Clone + Send + Sync + 'static,
    E: Clone + Send + 'static,
{
    let (tx, rx) = mpsc::unbounded_channel();
    let mut state = cache.lock().unwrap();
    if let Some(items) = state.ready.get(&key).cloned() {
        drop(state);
        send_items(&tx, &items);
        return rx;
    }
    match state.waiting.entry(key.clone()) {
        Entry::Occupied(mut entry) => {
            entry.get_mut().push(tx);
            return rx;
        }
        Entry::Vacant(entry) => {
            entry.insert(vec![tx]);
        }
    }
    drop(state);

    let cache = Arc::clone(cache);
    tokio::spawn(async move {
        let result = load.await.map(Arc::new);
        let subscribers = {
            let mut cache = cache.lock().unwrap();
            if let Ok(items) = &result {
                cache.ready.put(key.clone(), Arc::clone(items));
            }
            cache.waiting.remove(&key).unwrap()
        };

        for subscriber in subscribers {
            match &result {
                Ok(items) => send_items(&subscriber, items),
                Err(error) => {
                    let _ = subscriber.send(Err(error.clone()));
                }
            }
        }
    });

    rx
}

fn send_items<T: Clone, E>(tx: &mpsc::UnboundedSender<Result<T, CachedS3Error<E>>>, items: &[T]) {
    for item in items {
        if tx.send(Ok(item.clone())).is_err() {
            break;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn shares_pending_loads_and_evicts_the_least_recently_used_version() {
        let cache = Arc::new(Mutex::new(CacheState::new(NonZeroUsize::new(2).unwrap())));
        let key = |etag: &str| FileKey {
            object_key: "42/1.pb.gz".into(),
            etag: etag.into(),
        };
        let cases = |item: &'static str| Ok(vec![item, "last"]);
        let unexpected_load = || async { Err(CachedS3Error::DecodeFailure("unexpected load")) };
        let (release, wait) = tokio::sync::oneshot::channel();
        let mut first = get_or_load(&cache, key("v1"), async move {
            wait.await.unwrap();
            cases("first")
        });
        let mut subscriber = get_or_load(&cache, key("v1"), unexpected_load());
        release.send(()).unwrap();
        assert_eq!(first.recv().await.unwrap().unwrap(), "first");
        assert_eq!(subscriber.recv().await.unwrap().unwrap(), "first");

        let mut second = get_or_load(&cache, key("v2"), async move { cases("second") });
        assert_eq!(second.recv().await.unwrap().unwrap(), "second");
        let mut hit = get_or_load(&cache, key("v1"), unexpected_load());
        assert_eq!(hit.try_recv().unwrap().unwrap(), "first");
        assert_eq!(hit.try_recv().unwrap().unwrap(), "last");
        assert!(matches!(
            hit.try_recv(),
            Err(mpsc::error::TryRecvError::Disconnected)
        ));
        let mut third = get_or_load(&cache, key("v3"), async move { cases("third") });
        assert_eq!(third.recv().await.unwrap().unwrap(), "third");

        let mut hit = get_or_load(&cache, key("v1"), unexpected_load());
        assert_eq!(hit.recv().await.unwrap().unwrap(), "first");
        let mut evicted = get_or_load(&cache, key("v2"), async move { cases("reloaded") });
        assert_eq!(evicted.recv().await.unwrap().unwrap(), "reloaded");
    }
}
