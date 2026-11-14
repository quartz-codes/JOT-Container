use std::{future::Future, hash::Hash, sync::Arc};

use futures_util::future::{BoxFuture, FutureExt, Shared};
use scc::{HashMap, hash_map::Entry};
use tokio::task::JoinError;

type JobResult<V> = Result<Arc<V>, Arc<JoinError>>;
type Job<V> = Shared<BoxFuture<'static, JobResult<V>>>;
type Jobs<K, V> = Arc<HashMap<K, Job<V>>>;

pub(super) struct InFlight<K, V> {
    jobs: Jobs<K, V>,
}

impl<K, V> Default for InFlight<K, V> {
    fn default() -> Self {
        Self {
            jobs: Arc::default(),
        }
    }
}

impl<K, V> InFlight<K, V>
where
    K: Clone + Eq + Hash + Send + Sync + 'static,
    V: Send + Sync + 'static,
{
    pub(super) async fn run(
        &self,
        key: K,
        work: impl Future<Output = V> + Send + 'static,
    ) -> JobResult<V> {
        let job = match self.jobs.entry_async(key.clone()).await {
            Entry::Occupied(entry) => entry.get().clone(),
            Entry::Vacant(entry) => {
                let worker = tokio::spawn(work);
                let jobs = Arc::clone(&self.jobs);
                // The completion task owns cleanup independently of the callers.
                let completion = tokio::spawn(async move {
                    let result = worker.await;
                    jobs.remove_async(&key).await;
                    result
                });

                // Waiters share the outcome only after the key has been removed.
                let job = async move {
                    completion
                        .await
                        .and_then(|result| result)
                        .map(Arc::new)
                        .map_err(Arc::new)
                }
                .boxed()
                .shared();
                entry.insert_entry(job.clone());
                job
            }
        };

        job.await
    }
}

#[cfg(test)]
mod tests {
    use std::{future::poll_fn, task::Poll};

    use tokio::sync::oneshot;

    use super::*;

    #[tokio::test]
    async fn subscribers_share_the_result_after_the_first_caller_drops() {
        let jobs = InFlight::default();
        let (finish, wait) = oneshot::channel();
        let mut first = Box::pin(jobs.run("version", async move { wait.await.unwrap() }));
        let mut second = Box::pin(jobs.run("version", async { panic!("duplicate work") }));
        let mut third = Box::pin(jobs.run("version", async { panic!("duplicate work") }));

        // Register all three subscribers before allowing the worker to finish.
        poll_fn(|cx| {
            assert!(first.as_mut().poll(cx).is_pending());
            assert!(second.as_mut().poll(cx).is_pending());
            assert!(third.as_mut().poll(cx).is_pending());
            Poll::Ready(())
        })
        .await;

        drop(first);
        finish.send(42).unwrap();
        let second = second.await.unwrap();
        let third = third.await.unwrap();
        assert_eq!(*second, 42);
        assert!(Arc::ptr_eq(&second, &third));
        assert!(jobs.jobs.is_empty());
    }

    async fn wait_for_cleanup(jobs: &InFlight<&str, i32>) {
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            while !jobs.jobs.is_empty() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("completed job should leave the map");
    }

    #[tokio::test]
    async fn finishes_and_cleans_up_after_all_callers_drop() {
        let jobs = InFlight::default();
        let (finish, wait) = oneshot::channel();
        let mut caller = Box::pin(jobs.run("version", async move { wait.await.unwrap() }));
        poll_fn(|cx| {
            assert!(caller.as_mut().poll(cx).is_pending());
            Poll::Ready(())
        })
        .await;

        drop(caller);
        finish.send(42).unwrap();
        wait_for_cleanup(&jobs).await;
        assert_eq!(*jobs.run("version", async { 43 }).await.unwrap(), 43);
    }

    #[tokio::test]
    async fn shares_task_panics_and_allows_an_immediate_retry() {
        let jobs = InFlight::default();
        let (finish, wait) = oneshot::channel();
        let mut first = Box::pin(jobs.run("version", async move {
            wait.await.unwrap();
            panic!("worker panicked");
        }));
        let mut second = Box::pin(jobs.run("version", async { 99 }));
        poll_fn(|cx| {
            assert!(first.as_mut().poll(cx).is_pending());
            assert!(second.as_mut().poll(cx).is_pending());
            Poll::Ready(())
        })
        .await;

        finish.send(()).unwrap();
        let (first, second) = tokio::join!(first, second);
        let first = first.unwrap_err();
        let second = second.unwrap_err();
        assert!(first.is_panic());
        assert!(Arc::ptr_eq(&first, &second));
        assert_eq!(*jobs.run("version", async { 42 }).await.unwrap(), 42);
    }
}
