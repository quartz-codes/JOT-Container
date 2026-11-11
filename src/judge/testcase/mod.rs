use async_trait::async_trait;
use tokio::sync::mpsc::UnboundedReceiver;
#[derive(Clone, prost::Message)]
pub struct Testcase {
    #[prost(bytes = "bytes", tag = "1")]
    pub input: bytes::Bytes,
    #[prost(bytes = "bytes", tag = "2")]
    pub answer: bytes::Bytes,
}

pub type Testcases = Vec<Testcase>;

#[async_trait]
pub trait TestcaseFetcher {
    async fn get_subproblem_test_cases(
        &self,
        problem_id: i64,
        subproblem_id: i64,
    ) -> UnboundedReceiver<Result<Testcase, TestcaseFetchError>>;
}

pub type TestcaseFetchError = crate::judge::s3_repository::CachedS3Error<String>;

pub mod cached_fetcher;
