use bytes::Bytes;
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};
use tokio::sync::mpsc;

mod cached;

pub use cached::{CachedS3Error, CachedS3Repository};

pub struct S3Repository {
    s3_client: aws_sdk_s3::Client,
    bucket_name: String,
    in_downloading_jobs: Mutex<HashMap<FileKey, mpsc::UnboundedSender<Subscriber>>>,
}

impl S3Repository {
    pub fn new(s3_client: aws_sdk_s3::Client, bucket_name: String) -> Self {
        Self {
            s3_client,
            bucket_name,
            in_downloading_jobs: Mutex::new(HashMap::new()),
        }
    }

    pub async fn get_etag(&self, object_key: &str) -> Result<String, S3RepositoryRequestError> {
        let response = self
            .s3_client
            .head_object()
            .bucket(&self.bucket_name)
            .key(object_key)
            .send()
            .await
            .map_err(|error| {
                S3RepositoryRequestError::S3RequestFailure(format!(
                    "HEAD {}/{} failed: {error}",
                    self.bucket_name, object_key
                ))
            })?;

        response.e_tag.ok_or_else(|| {
            S3RepositoryRequestError::S3RequestFailure(format!(
                "HEAD {}/{} returned no ETag",
                self.bucket_name, object_key
            ))
        })
    }

    pub(crate) fn start_or_subscribe(
        self: &Arc<Self>,
        key: FileKey,
    ) -> mpsc::UnboundedReceiver<Result<Bytes, S3RepositoryRequestError>> {
        let (tx, rx) = mpsc::unbounded_channel();
        let mut jobs = self.in_downloading_jobs.lock().unwrap();
        let tx = match jobs.get(&key) {
            Some(subscribe_tx) => match subscribe_tx.send(tx) {
                Ok(()) => return rx,
                Err(error) => error.0,
            },
            None => tx,
        };

        let (subscribe_tx, subscribe_rx) = mpsc::unbounded_channel();
        jobs.insert(key.clone(), subscribe_tx);
        drop(jobs);

        let job = DownloadJob {
            subscribe_rx,
            downloaded: Vec::new(),
            subscribers: vec![tx],
        };
        tokio::spawn(job.run(Arc::clone(self), key));
        rx
    }
}

type Subscriber = mpsc::UnboundedSender<Result<Bytes, S3RepositoryRequestError>>;

#[derive(Clone, Debug)]
pub enum S3RepositoryRequestError {
    S3RequestFailure(String),
}

#[derive(Clone, PartialEq, Eq, Hash)]
pub(crate) struct FileKey {
    pub object_key: String,
    pub etag: String,
}

struct DownloadJob {
    subscribe_rx: mpsc::UnboundedReceiver<Subscriber>,
    downloaded: Vec<Bytes>,
    subscribers: Vec<Subscriber>,
}

impl DownloadJob {
    async fn run(mut self, repository: Arc<S3Repository>, key: FileKey) {
        let response = repository
            .s3_client
            .get_object()
            .bucket(&repository.bucket_name)
            .key(&key.object_key)
            .if_match(&key.etag)
            .send()
            .await;

        let result = match response {
            Ok(response) => {
                let mut body = response.body;
                loop {
                    tokio::select! {
                        Some(subscriber) = self.subscribe_rx.recv() => {
                            self.add_subscriber(subscriber);
                        }
                        chunk = body.next() => match chunk {
                            Some(Ok(chunk)) => {
                                self.subscribers.retain(|subscriber| {
                                    subscriber.send(Ok(chunk.clone())).is_ok()
                                });
                                self.downloaded.push(chunk);
                            }
                            Some(Err(error)) => break Err(format!("body read failed: {error}")),
                            None => break Ok(()),
                        }
                    }
                }
            }
            Err(error) => Err(format!("request failed: {error}")),
        };

        {
            let mut jobs = repository.in_downloading_jobs.lock().unwrap();
            self.subscribe_rx.close();
            jobs.remove(&key);
        }

        while let Some(subscriber) = self.subscribe_rx.recv().await {
            self.add_subscriber(subscriber);
        }

        if let Err(error) = result {
            let error = S3RepositoryRequestError::S3RequestFailure(format!(
                "GET {}/{}: {error}",
                repository.bucket_name, key.object_key
            ));
            for subscriber in self.subscribers {
                let _ = subscriber.send(Err(error.clone()));
            }
        }
    }

    fn add_subscriber(&mut self, subscriber: Subscriber) {
        for chunk in &self.downloaded {
            if subscriber.send(Ok(chunk.clone())).is_err() {
                return;
            }
        }
        self.subscribers.push(subscriber);
    }
}
