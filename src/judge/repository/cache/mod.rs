use std::sync::Arc;

use async_trait::async_trait;

use super::{ObjectVersion, Repository, S3RepositoryRequestError, in_flight::InFlight};

pub mod lru;
pub use lru::Lru;

/// Stores decoded objects by their repository-provided version key.
pub trait Cache<T>: Send + Sync {
    fn get(&self, key: &str) -> Option<T>;
    fn insert(&self, key: String, value: T);
}

type FetchResult<T, E> = Result<T, S3RepositoryRequestError<E>>;

/// Wraps a repository with a cache. Cache keys are scoped to the wrapped
/// repository, so each repository should have its own cache instance.
pub struct CachedRepository<R, C, T, E> {
    repository: Arc<R>,
    cache: Arc<C>,
    requests: InFlight<ObjectVersion, FetchResult<T, E>>,
}

impl<R, C, T, E> CachedRepository<R, C, T, E> {
    pub fn new(repository: R, cache: C) -> Self {
        Self {
            repository: Arc::new(repository),
            cache: Arc::new(cache),
            requests: InFlight::default(),
        }
    }
}

#[async_trait]
impl<T, E, R, C> Repository<T, E> for CachedRepository<R, C, T, E>
where
    T: Clone + Send + Sync + 'static,
    E: Clone + Send + Sync + 'static,
    R: Repository<T, E> + Send + 'static,
    C: Cache<T> + 'static,
{
    async fn get_object_version(&self, object_key: &str) -> Result<ObjectVersion, String> {
        self.repository.get_object_version(object_key).await
    }

    async fn fetch_version(&self, version: &ObjectVersion) -> FetchResult<T, E> {
        let object_key = &version.object_key;
        let key = format!("{}:{object_key}:{}", object_key.len(), version.etag);
        if let Some(value) = self.cache.get(&key) {
            return Ok(value);
        }

        let repository = Arc::clone(&self.repository);
        let cache = Arc::clone(&self.cache);
        let version = version.clone();
        self.requests
            .run(version.clone(), async move {
                let value = repository.fetch_version(&version).await?;
                cache.insert(key, value.clone());
                Ok(value)
            })
            .await
            .map_err(S3RepositoryRequestError::TaskFailed)?
            .as_ref()
            .clone()
    }
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroUsize;

    use super::*;

    struct VersionedRepository;

    #[async_trait]
    impl Repository<String, ()> for VersionedRepository {
        async fn fetch_version(&self, version: &ObjectVersion) -> FetchResult<String, ()> {
            Ok(version.etag.clone())
        }

        async fn get_object_version(&self, object_key: &str) -> Result<ObjectVersion, String> {
            Ok(ObjectVersion {
                object_key: object_key.into(),
                etag: "v2".into(),
            })
        }
    }

    #[tokio::test]
    async fn fetches_and_caches_the_current_version() {
        let cache = Lru::new(NonZeroUsize::new(2).unwrap());
        cache.insert("6:object:v1".into(), "stale".into());
        let repository = CachedRepository::new(VersionedRepository, cache);

        assert_eq!(repository.fetch("object").await.unwrap(), "v2");
        assert_eq!(repository.cache.get("6:object:v2").as_deref(), Some("v2"));
    }

    #[tokio::test]
    async fn returns_the_cached_value_for_the_current_version() {
        let cache = Lru::new(NonZeroUsize::new(1).unwrap());
        cache.insert("6:object:v2".into(), "cached".into());
        let repository = CachedRepository::new(VersionedRepository, cache);

        assert_eq!(repository.fetch("object").await.unwrap(), "cached");
    }
}
