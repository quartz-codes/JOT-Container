use async_trait::async_trait;
use bytes::Bytes;
use reqwest::Client;
use tokio::sync::mpsc::{self, UnboundedReceiver};

use super::{HttpRepositoryRequestError, Repository};
use HttpRepositoryRequestError::{DecodeError, RepositoryError};

pub struct HttpRepository<T, E> {
    client: Client,
    decode_fn: fn(UnboundedReceiver<Bytes>) -> Result<T, E>,
}

impl<T, E> HttpRepository<T, E> {
    pub fn new(client: Client, decode_fn: fn(UnboundedReceiver<Bytes>) -> Result<T, E>) -> Self {
        Self { client, decode_fn }
    }
}

#[async_trait]
impl<T, E> Repository<T, E> for HttpRepository<T, E>
where
    T: Send + 'static,
    E: Send + 'static,
{
    async fn fetch(&self, _key: (i64, i64), url: &str) -> Result<T, HttpRepositoryRequestError<E>> {
        let mut response = self
            .client
            .get(url)
            .send()
            .await
            .and_then(reqwest::Response::error_for_status)
            .map_err(|error| {
                RepositoryError(format!("Testcase GET failed: {}", error.without_url()))
            })?;

        let (chunks, receiver) = mpsc::unbounded_channel();
        let decode = self.decode_fn;
        let decoder = tokio::task::spawn_blocking(move || decode(receiver));
        while let Some(chunk) = response.chunk().await.map_err(|error| {
            RepositoryError(format!(
                "Testcase body read failed: {}",
                error.without_url()
            ))
        })? {
            if chunks.send(chunk).is_err() {
                break;
            }
        }
        drop(chunks);
        decoder
            .await
            .map_err(|error| {
                RepositoryError(format!("HTTP response decoder task failed: {error}"))
            })?
            .map_err(DecodeError)
    }
}
