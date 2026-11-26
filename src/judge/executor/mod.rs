pub mod wasm_executor;

use std::sync::Arc;

use async_trait::async_trait;

#[derive(Debug)]
pub enum ExecuteError {
    TimeLimitExceeded(u64),
    MemoryLimitExceeded(u64),
    OutputLimitExceeded(usize),
    RuntimeError { reason: Option<String> },
}
#[derive(Debug)]
pub struct ExecuteResult {
    pub output: String,
    pub memory_used: u64,
    pub instruction_count: u64,
}

#[async_trait]
pub trait Executor: Send + Sync {
    async fn execute(
        &self,
        input: &str,
        memory_limit: u64,
        time_limit: u64,
        output_limit: usize,
    ) -> Result<ExecuteResult, ExecuteError>;
}

#[async_trait]
impl<T: Executor + ?Sized> Executor for Arc<T> {
    async fn execute(
        &self,
        input: &str,
        memory_limit: u64,
        time_limit: u64,
        output_limit: usize,
    ) -> Result<ExecuteResult, ExecuteError> {
        (**self)
            .execute(input, memory_limit, time_limit, output_limit)
            .await
    }
}

#[cfg(test)]
pub(crate) mod aplusb_executor;
