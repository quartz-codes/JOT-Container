use std::{
    collections::HashMap,
    num::NonZeroUsize,
    sync::{Arc, Mutex},
};

use aws_sdk_s3::Client;
use lru::LruCache;
use tokio::sync::mpsc;

#[derive(Clone, Debug)]
pub enum CachedS3Error<E> {
    S3RequestFailure(String),
    DecodeFailure(E),
    DecoderTaskFailed(String),
}

impl<E: std::fmt::Display> std::fmt::Display for CachedS3Error<E> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::S3RequestFailure(message) => {
                write!(f, "S3 download failed: {message}")
            }
            Self::DecodeFailure(error) => write!(f, "S3 object decoding failed: {error}"),
            Self::DecoderTaskFailed(error) => write!(f, "S3 object decoder task failed: {error}"),
        }
    }
}

type Decoder<T, E> = dyn Fn(&[u8]) -> Result<Vec<T>, E> + Send + Sync;
type Subscriber<T, E> = mpsc::UnboundedSender<Result<T, CachedS3Error<E>>>;
type DownloadJobs<T, E> = HashMap<FileKey, mpsc::UnboundedSender<Subscriber<T, E>>>;
type ObjectCache<T> = LruCache<FileKey, Arc<Vec<T>>>;

pub struct CachedS3Repository<T, E> {
    client: Client,
    bucket: String,
    decoder: Box<Decoder<T, E>>,
    cache: Mutex<ObjectCache<T>>,
    downloads: Mutex<DownloadJobs<T, E>>,
}

impl<T, E> CachedS3Repository<T, E>
where
    T: Clone + Send + Sync + 'static,
    E: Clone + Send + 'static,
{
    pub fn new(
        client: Client,
        bucket: String,
        capacity: NonZeroUsize,
        decoder: impl Fn(&[u8]) -> Result<Vec<T>, E> + Send + Sync + 'static,
    ) -> Self {
        Self {
            client,
            bucket,
            decoder: Box::new(decoder),
            cache: Mutex::new(LruCache::new(capacity)),
            downloads: Mutex::new(HashMap::new()),
        }
    }

    pub async fn fetch(
        self: &Arc<Self>,
        object_key: String,
    ) -> mpsc::UnboundedReceiver<Result<T, CachedS3Error<E>>> {
        let etag = match self.get_etag(&object_key).await {
            Ok(etag) => etag,
            Err(error) => {
                let (tx, rx) = mpsc::unbounded_channel();
                let _ = tx.send(Err(error));
                return rx;
            }
        };
        self.start_or_subscribe(FileKey { object_key, etag })
    }

    fn start_or_subscribe(
        self: &Arc<Self>,
        key: FileKey,
    ) -> mpsc::UnboundedReceiver<Result<T, CachedS3Error<E>>> {
        let (tx, rx) = mpsc::unbounded_channel();
        let mut jobs = self.downloads.lock().unwrap();
        let cached_items = self.cache.lock().unwrap().get(&key).cloned();
        if let Some(items) = cached_items {
            drop(jobs);
            send_items(&tx, &items);
            return rx;
        }
        let tx = match jobs.get(&key) {
            Some(subscribe_tx) => match subscribe_tx.send(tx) {
                Ok(()) => return rx,
                Err(error) => error.0,
            },
            None => tx,
        };

        let (subscribe_tx, subscribe_rx) = mpsc::unbounded_channel();
        subscribe_tx
            .send(tx)
            .expect("new subscription channel is open");
        jobs.insert(key.clone(), subscribe_tx);
        drop(jobs);

        let job = DownloadJob { subscribe_rx };
        tokio::spawn(job.run(Arc::clone(self), key));
        rx
    }

    async fn get_etag(&self, object_key: &str) -> Result<String, CachedS3Error<E>> {
        let response = self
            .client
            .head_object()
            .bucket(&self.bucket)
            .key(object_key)
            .send()
            .await
            .map_err(|error| {
                CachedS3Error::S3RequestFailure(format!(
                    "HEAD {}/{object_key} failed: {error}",
                    self.bucket
                ))
            })?;

        response.e_tag.ok_or_else(|| {
            CachedS3Error::S3RequestFailure(format!(
                "HEAD {}/{object_key} returned no ETag",
                self.bucket
            ))
        })
    }
}

#[derive(Clone, PartialEq, Eq, Hash)]
struct FileKey {
    object_key: String,
    etag: String,
}

struct DownloadJob<T, E> {
    subscribe_rx: mpsc::UnboundedReceiver<Subscriber<T, E>>,
}

impl<T, E> DownloadJob<T, E>
where
    T: Clone + Send + Sync + 'static,
    E: Clone + Send + 'static,
{
    async fn run(self, repository: Arc<CachedS3Repository<T, E>>, key: FileKey) {
        let result = async {
            let response = repository
                .client
                .get_object()
                .bucket(&repository.bucket)
                .key(&key.object_key)
                .if_match(&key.etag)
                .send()
                .await
                .map_err(|error| {
                    CachedS3Error::S3RequestFailure(format!(
                        "GET {}/{} failed: {error}",
                        repository.bucket, key.object_key
                    ))
                })?;
            let body = response.body.collect().await.map_err(|error| {
                CachedS3Error::S3RequestFailure(format!(
                    "GET {}/{} body read failed: {error}",
                    repository.bucket, key.object_key
                ))
            })?;
            let repository = Arc::clone(&repository);
            tokio::task::spawn_blocking(move || (repository.decoder)(&body.into_bytes()))
                .await
                .map_err(|error| CachedS3Error::DecoderTaskFailed(error.to_string()))?
                .map_err(CachedS3Error::DecodeFailure)
        }
        .await;
        self.finish(&repository.cache, &repository.downloads, key, result);
    }

    fn finish(
        mut self,
        cache: &Mutex<ObjectCache<T>>,
        downloads: &Mutex<DownloadJobs<T, E>>,
        key: FileKey,
        result: Result<Vec<T>, CachedS3Error<E>>,
    ) {
        let result = result.map(Arc::new);
        {
            // Publish the cache entry before closing registration for this version.
            let mut jobs = downloads.lock().unwrap();
            if let Ok(items) = &result {
                cache.lock().unwrap().put(key.clone(), Arc::clone(items));
            }
            self.subscribe_rx.close();
            jobs.remove(&key);
        }

        while let Ok(subscriber) = self.subscribe_rx.try_recv() {
            match &result {
                Ok(items) => send_items(&subscriber, items),
                Err(error) => {
                    let _ = subscriber.send(Err(error.clone()));
                }
            }
        }
    }
}

fn send_items<T: Clone, E>(tx: &Subscriber<T, E>, items: &[T]) {
    for item in items {
        if tx.send(Ok(item.clone())).is_err() {
            break;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn completed_job_caches_the_result_and_notifies_all_subscribers() {
        let cache = Mutex::new(LruCache::new(NonZeroUsize::new(2).unwrap()));
        let downloads: Mutex<DownloadJobs<&str, &str>> = Mutex::new(HashMap::new());
        let key = FileKey {
            object_key: "42/1.pb.gz".into(),
            etag: "v1".into(),
        };
        let (subscribe_tx, subscribe_rx) = mpsc::unbounded_channel();
        let (first_tx, mut first) = mpsc::unbounded_channel();
        let (second_tx, mut second) = mpsc::unbounded_channel();
        subscribe_tx.send(first_tx).unwrap();
        subscribe_tx.send(second_tx).unwrap();
        downloads.lock().unwrap().insert(key.clone(), subscribe_tx);

        assert!(cache.lock().unwrap().is_empty());
        DownloadJob { subscribe_rx }.finish(
            &cache,
            &downloads,
            key.clone(),
            Ok(vec!["first", "last"]),
        );

        assert!(downloads.lock().unwrap().is_empty());
        assert_eq!(
            &**cache.lock().unwrap().get(&key).unwrap(),
            &["first", "last"]
        );
        for subscriber in [&mut first, &mut second] {
            assert_eq!(subscriber.try_recv().unwrap().unwrap(), "first");
            assert_eq!(subscriber.try_recv().unwrap().unwrap(), "last");
            assert!(matches!(
                subscriber.try_recv(),
                Err(mpsc::error::TryRecvError::Disconnected)
            ));
        }
    }
}
