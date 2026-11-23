use crate::judge::{checker::Checker, executor::Executor};
use async_trait::async_trait;
use base64::prelude::*;

pub struct TestlibChecker {
    executor: Box<dyn Executor>,
}

impl TestlibChecker {
    pub fn new(executor: Box<dyn Executor>) -> Self {
        Self { executor }
    }
}

#[async_trait]
impl Checker for TestlibChecker {
    async fn check(
        &self,
        input: &str,
        output: &str,
        answer: &str,
    ) -> Result<bool, Box<dyn std::error::Error>> {
        let executor_input = format!(
            "{}\n{}\n{}",
            BASE64_STANDARD.encode(input),
            BASE64_STANDARD.encode(output),
            BASE64_STANDARD.encode(answer)
        );
        match self
            .executor
            .execute(
                &executor_input,
                1024 * 1024 * 1024,
                1024 * 1024 * 1024 * 1024,
            )
            .await
        {
            Ok(_) => {
                return Ok(true);
            }
            Err(e) => {
                println!("testlib checker returned error: {}", e);
                return Ok(false);
            }
        }
    }
}
