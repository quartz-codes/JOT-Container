use std::sync::Arc;

use async_trait::async_trait;
use aws_sdk_s3::Client;
use bytes::Bytes;
use tokio::sync::mpsc::{self, UnboundedReceiver};

use super::{ObjectVersion, Repository, S3RepositoryRequestError, in_flight::InFlight};
use S3RepositoryRequestError::{DecodeError, RepositoryError, TaskFailed};

type DownloadResult<T, E> = Result<T, S3RepositoryRequestError<E>>;

pub struct S3Repository<T, E> {
    client: Client,
    bucket: Arc<str>,
    downloads: InFlight<ObjectVersion, DownloadResult<T, E>>,
    decode_fn: fn(UnboundedReceiver<Bytes>) -> Result<T, E>,
}

impl<T, E> S3Repository<T, E> {
    pub fn new(
        client: Client,
        bucket: String,
        decode_fn: fn(UnboundedReceiver<Bytes>) -> Result<T, E>,
    ) -> Self {
        Self {
            client,
            bucket: bucket.into(),
            downloads: InFlight::default(),
            decode_fn,
        }
    }
}

impl<T: Send + 'static, E: Send + 'static> S3Repository<T, E> {
    async fn download(
        client: Client,
        bucket: Arc<str>,
        version: ObjectVersion,
        decode: fn(UnboundedReceiver<Bytes>) -> Result<T, E>,
    ) -> DownloadResult<T, E> {
        let object_key = &version.object_key;
        let response = client
            .get_object()
            .bucket(bucket.as_ref())
            .key(object_key)
            .if_match(&version.etag)
            .send()
            .await
            .map_err(|error| {
                RepositoryError(format!("GET {bucket}/{object_key} failed: {error}"))
            })?;
        if response.e_tag() != Some(version.etag.as_str()) {
            return Err(RepositoryError(format!(
                "GET {bucket}/{object_key} returned an unexpected ETag"
            )));
        }
        let mut body = response.body;

        let (chunks, receiver) = mpsc::unbounded_channel();
        let decoder = tokio::task::spawn_blocking(move || decode(receiver));

        loop {
            let chunk = body.try_next().await.map_err(|error| {
                RepositoryError(format!(
                    "GET {bucket}/{object_key} body read failed: {error}"
                ))
            })?;
            let Some(chunk) = chunk else {
                break;
            };
            if chunks.send(chunk).is_err() {
                break;
            }
        }

        drop(chunks);
        decoder
            .await
            .map_err(|error| RepositoryError(format!("S3 object decoder task failed: {error}")))?
            .map_err(DecodeError)
    }
}

#[async_trait]
impl<T, E> Repository<T, E> for S3Repository<T, E>
where
    T: Clone + Send + Sync + 'static,
    E: Clone + Send + Sync + 'static,
{
    async fn get_object_version(&self, object_key: &str) -> Result<ObjectVersion, String> {
        let response = self
            .client
            .head_object()
            .bucket(self.bucket.as_ref())
            .key(object_key)
            .send()
            .await
            .map_err(|error| format!("HEAD {}/{object_key} failed: {error}", self.bucket))?;

        let etag = response
            .e_tag
            .ok_or_else(|| format!("HEAD {}/{object_key} returned no ETag", self.bucket))?;
        Ok(ObjectVersion {
            object_key: object_key.to_owned(),
            etag,
        })
    }

    async fn fetch_version(&self, version: &ObjectVersion) -> DownloadResult<T, E> {
        self.downloads
            .run(
                version.clone(),
                Self::download(
                    self.client.clone(),
                    self.bucket.clone(),
                    version.clone(),
                    self.decode_fn,
                ),
            )
            .await
            .map_err(TaskFailed)?
            .as_ref()
            .clone()
    }
}
