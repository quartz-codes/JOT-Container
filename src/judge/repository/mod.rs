use std::{fmt, sync::Arc};

use async_trait::async_trait;
use tokio::task::JoinError;

pub mod cache;
mod in_flight;
pub mod s3_repository;

/// An object and the ETag observed when looking up its current version.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct ObjectVersion {
    pub object_key: String,
    pub etag: String,
}

#[async_trait]
pub trait Repository<T, E>: Sync {
    /// Fetches the current version of an object.
    async fn fetch(&self, object_key: &str) -> Result<T, S3RepositoryRequestError<E>> {
        let version = self
            .get_object_version(object_key)
            .await
            .map_err(S3RepositoryRequestError::RepositoryError)?;
        self.fetch_version(&version).await
    }

    async fn get_object_version(&self, object_key: &str) -> Result<ObjectVersion, String>;

    /// Fetches the specified version or reports that it could not be fetched.
    async fn fetch_version(
        &self,
        version: &ObjectVersion,
    ) -> Result<T, S3RepositoryRequestError<E>>;
}

#[derive(Clone, Debug)]
pub enum S3RepositoryRequestError<E> {
    DecodeError(E),
    RepositoryError(String),
    TaskFailed(Arc<JoinError>),
}

impl<E: fmt::Display> fmt::Display for S3RepositoryRequestError<E> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DecodeError(error) => write!(f, "S3 object decoding failed: {error}"),
            Self::RepositoryError(error) => f.write_str(error),
            Self::TaskFailed(error) => write!(f, "Repository task failed: {error}"),
        }
    }
}
