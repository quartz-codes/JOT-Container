use std::{fmt, sync::Arc};

use async_trait::async_trait;
use tokio::task::JoinError;

pub mod cache;
pub mod http_repository;
mod in_flight;

#[async_trait]
pub trait Repository<T, E>: Sync {
    async fn fetch(&self, key: (i64, i64), url: &str) -> Result<T, HttpRepositoryRequestError<E>>;
}

#[derive(Clone, Debug)]
pub enum HttpRepositoryRequestError<E> {
    DecodeError(E),
    RepositoryError(String),
    TaskFailed(Arc<JoinError>),
}

impl<E: fmt::Display> fmt::Display for HttpRepositoryRequestError<E> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DecodeError(error) => write!(f, "HTTP response decoding failed: {error}"),
            Self::RepositoryError(error) => f.write_str(error),
            Self::TaskFailed(error) => write!(f, "Repository task failed: {error}"),
        }
    }
}
