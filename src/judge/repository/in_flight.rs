use std::{future::Future, hash::Hash, panic::AssertUnwindSafe, sync::Arc};

use futures_util::future::{BoxFuture, FutureExt, Shared};
use scc::{HashMap, hash_map::Entry};

type Job<V> = Shared<BoxFuture<'static, Option<Arc<V>>>>;
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
    pub(super) async fn run<F>(&self, key: K, start: impl FnOnce() -> F) -> Option<Arc<V>>
    where
        F: Future<Output = V> + Send + 'static,
    {
        let (shared, is_new) = match self.jobs.entry_async(key.clone()).await {
            Entry::Occupied(job) => (job.get().clone(), false),
            Entry::Vacant(job) => {
                let work = start();
                // Resolve all handles consistently even if the work panics.
                let shared = async move {
                    AssertUnwindSafe(work)
                        .catch_unwind()
                        .await
                        .ok()
                        .map(Arc::new)
                }
                .boxed()
                .shared();
                job.insert_entry(shared.clone());
                (shared, true)
            }
        };

        if is_new {
            let registration = Registration {
                jobs: Arc::clone(&self.jobs),
                key,
            };
            let worker = shared.clone();
            // Keep driving the job even when every caller drops its handle.
            tokio::spawn(async move {
                let _registration = registration;
                worker.await;
            });
        }

        shared.await
    }
}

struct Registration<K: Eq + Hash, V> {
    jobs: Jobs<K, V>,
    key: K,
}

impl<K: Eq + Hash, V> Drop for Registration<K, V> {
    fn drop(&mut self) {
        // Drop also runs if the worker unwinds, so cleanup must be synchronous.
        self.jobs.remove_sync(&self.key);
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
        let mut first = Box::pin(jobs.run("version", || async move { wait.await.unwrap() }));
        let mut second = Box::pin(jobs.run("version", || async { panic!("duplicate work") }));
        let mut third = Box::pin(jobs.run("version", || async { panic!("duplicate work") }));

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
        wait_for_cleanup(&jobs).await;
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
        let mut caller = Box::pin(jobs.run("version", || async move { wait.await.unwrap() }));
        poll_fn(|cx| {
            assert!(caller.as_mut().poll(cx).is_pending());
            Poll::Ready(())
        })
        .await;

        drop(caller);
        finish.send(42).unwrap();
        wait_for_cleanup(&jobs).await;
        assert_eq!(*jobs.run("version", || async { 43 }).await.unwrap(), 43);
    }
}
