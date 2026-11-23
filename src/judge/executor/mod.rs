pub mod wasm_executor;

use std::{fmt, sync::Arc};

use async_trait::async_trait;

#[derive(Debug, Clone)]
pub enum ExecuteError {
    TimeLimitExceeded(u64),
    MemoryLimitExceeded(u64),
    RuntimeError { reason: Option<String> },
}
impl fmt::Display for ExecuteError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ExecuteError::TimeLimitExceeded(limit) => write!(f, "Time limit exceeded: {}", limit),
            ExecuteError::MemoryLimitExceeded(limit) => {
                write!(f, "Memory limit exceeded: {}", limit)
            }
            ExecuteError::RuntimeError { reason } => {
                if let Some(r) = reason {
                    write!(f, "Runtime error: {}", r)
                } else {
                    write!(f, "Runtime error")
                }
            }
        }
    }
}
impl std::error::Error for ExecuteError {}
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
    ) -> Result<ExecuteResult, ExecuteError>;
}

#[async_trait]
impl<T: Executor + ?Sized> Executor for Arc<T> {
    async fn execute(
        &self,
        input: &str,
        memory_limit: u64,
        time_limit: u64,
    ) -> Result<ExecuteResult, ExecuteError> {
        (**self).execute(input, memory_limit, time_limit).await
    }
}

#[cfg(test)]
pub(crate) mod aplusb_executor;
