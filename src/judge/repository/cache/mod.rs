use std::sync::Arc;

use async_trait::async_trait;

use super::{HttpRepositoryRequestError, Repository, in_flight::InFlight};

pub mod lru;
pub use lru::Lru;

/// Stores decoded objects by (problem_id, subproblem_id), independently of the URL.
pub trait Cache<T>: Send + Sync {
    fn get(&self, key: &(i64, i64)) -> Option<T>;
    fn insert(&self, key: (i64, i64), value: T);
}

type FetchResult<T, E> = Result<T, HttpRepositoryRequestError<E>>;

/// Wraps a repository with a cache. Cache keys are scoped to the wrapped
/// repository, so each repository should have its own cache instance.
pub struct CachedRepository<R, C, T, E> {
    repository: Arc<R>,
    cache: Arc<C>,
    requests: InFlight<(i64, i64), FetchResult<T, E>>,
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
    async fn fetch(&self, key: (i64, i64), url: &str) -> FetchResult<T, E> {
        if let Some(value) = self.cache.get(&key) {
            return Ok(value);
        }

        let repository = self.repository.clone();
        let cache = self.cache.clone();
        let url = url.to_owned();
        self.requests
            .run(key, async move {
                // A previous request may have populated the cache before registration.
                if let Some(value) = cache.get(&key) {
                    return Ok(value);
                }
                let value = repository.fetch(key, &url).await?;
                cache.insert(key, value.clone());
                Ok(value)
            })
            .await
            .map_err(HttpRepositoryRequestError::TaskFailed)?
            .as_ref()
            .clone()
    }
}

#[cfg(test)]
mod tests {
    use std::{
        num::NonZeroUsize,
        sync::atomic::{AtomicUsize, Ordering},
    };

    use super::*;

    #[derive(Default)]
    struct CountingRepository {
        calls: AtomicUsize,
    }

    #[async_trait]
    impl Repository<String, String> for CountingRepository {
        async fn fetch(&self, _key: (i64, i64), url: &str) -> FetchResult<String, String> {
            self.calls.fetch_add(1, Ordering::Relaxed);
            tokio::task::yield_now().await;
            match url {
                "failed-download" => Err(HttpRepositoryRequestError::RepositoryError(
                    "download failed".into(),
                )),
                "failed-decode" => Err(HttpRepositoryRequestError::DecodeError(
                    "decode failed".into(),
                )),
                _ => Ok(url.to_owned()),
            }
        }
    }

    fn repository() -> CachedRepository<CountingRepository, Lru<String>, String, String> {
        CachedRepository::new(
            CountingRepository::default(),
            Lru::new(NonZeroUsize::new(4).unwrap()),
        )
    }

    #[tokio::test]
    async fn reuses_the_same_id_pair_after_the_url_changes() {
        let repository = repository();
        assert_eq!(
            repository.fetch((1, 7), "original-url").await.unwrap(),
            "original-url"
        );
        assert_eq!(
            repository.fetch((1, 7), "renewed-url").await.unwrap(),
            "original-url"
        );
        assert_eq!(repository.repository.calls.load(Ordering::Relaxed), 1);
    }

    #[tokio::test]
    async fn different_problem_or_subproblem_ids_have_separate_entries() {
        let repository = repository();
        for key in [(1, 7), (2, 7), (1, 8)] {
            assert_eq!(
                repository.fetch(key, "shared-url").await.unwrap(),
                "shared-url"
            );
        }
        assert_eq!(repository.repository.calls.load(Ordering::Relaxed), 3);
    }

    #[tokio::test]
    async fn concurrent_requests_with_different_urls_share_work_for_the_same_pair() {
        let repository = repository();
        let requests = ["first-url", "second-url", "third-url"]
            .into_iter()
            .map(|url| repository.fetch((1, 7), url));
        let results = futures_util::future::join_all(requests).await;
        let first = results[0].as_ref().unwrap();
        assert!(
            results
                .iter()
                .all(|result| result.as_ref().unwrap() == first)
        );
        assert_eq!(repository.repository.calls.load(Ordering::Relaxed), 1);
    }

    #[tokio::test]
    async fn failed_downloads_and_decodes_allow_retry_with_a_new_url() {
        for failed_url in ["failed-download", "failed-decode"] {
            let repository = repository();
            assert!(repository.fetch((1, 7), failed_url).await.is_err());
            assert_eq!(
                repository.fetch((1, 7), "working-url").await.unwrap(),
                "working-url"
            );
            assert_eq!(
                repository.fetch((1, 7), "renewed-url").await.unwrap(),
                "working-url"
            );
            assert_eq!(repository.repository.calls.load(Ordering::Relaxed), 2);
        }
    }
}
