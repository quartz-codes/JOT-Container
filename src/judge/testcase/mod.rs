use async_trait::async_trait;

#[derive(Clone, prost::Message)]
pub struct Testcase {
    #[prost(bytes = "bytes", tag = "1")]
    pub input: bytes::Bytes,
    #[prost(bytes = "bytes", tag = "2")]
    pub answer: bytes::Bytes,
}

pub type Testcases = Vec<Testcase>;

#[async_trait]
pub trait TestcasesFetcher {
    async fn get_subproblem_testcases(
        &self,
        problem_id: i64,
        subproblem_id: i64,
        testcase_url: &str,
    ) -> Result<Testcases, TestcaseFetchError>;
}

pub type TestcaseFetchError = crate::judge::repository::HttpRepositoryRequestError<String>;

pub mod http_fetcher;
