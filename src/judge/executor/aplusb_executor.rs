// APlusBExecutor: a simple implementation example of Executor
// It returns the sum of given two integers

use super::Executor;
use crate::judge::executor::{ExecuteError, ExecuteResult};
use async_trait::async_trait;

pub struct APlusBExecutor;

impl APlusBExecutor {
    pub fn new() -> Self {
        Self {}
    }
}

#[async_trait]
impl Executor for APlusBExecutor {
    async fn execute(
        &self,
        input: &str,
        memory_limit: u64,
        time_limit: u64,
        output_limit: usize,
    ) -> Result<ExecuteResult, ExecuteError> {
        if time_limit < 100 {
            return Err(ExecuteError::TimeLimitExceeded(time_limit));
        }
        if memory_limit < 1024 {
            return Err(ExecuteError::MemoryLimitExceeded(memory_limit));
        }

        let sp = input
            .split(' ')
            .map(|x| x.trim().parse::<i32>().unwrap_or(0))
            .collect::<Vec<_>>();
        let (a, b) = (sp[0], sp[1]);
        let res = a + b;
        if res.to_string().len() > output_limit {
            return Err(ExecuteError::OutputLimitExceeded(output_limit));
        }

        Ok(ExecuteResult {
            output: res.to_string(),
            memory_used: 1024, // Just a dummy value for memory used
            instruction_count: 100,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_1() {
        let executor: Box<dyn Executor> = Box::new(APlusBExecutor::new());
        let res = executor
            .execute("1 2", 1048576, 100000, usize::MAX)
            .await
            .expect("Runtime error");
        assert_eq!(res.output, "3");
    }
    #[tokio::test]
    async fn test_2() {
        let executor: Box<dyn Executor> = Box::new(APlusBExecutor::new());
        let res = executor
            .execute("100 200", 1048576, 100000, usize::MAX)
            .await
            .expect("Runtime error");
        assert_eq!(res.output, "300");
    }
    #[tokio::test]
    async fn test_3() {
        let executor: Box<dyn Executor> = Box::new(APlusBExecutor::new());
        let res = executor
            .execute("123 45", 1048576, 100000, usize::MAX)
            .await
            .expect("Runtime error");
        assert_eq!(res.output, "168");
    }
}
