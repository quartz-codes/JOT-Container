pub mod lcmp_checker;
pub mod testlib_checker;

use async_trait::async_trait;

#[async_trait]
pub trait Checker: Send + Sync {
    async fn check(
        &self,
        input: &str,
        output: &str,
        answer: &str,
    ) -> Result<bool, Box<dyn std::error::Error>>;
}
