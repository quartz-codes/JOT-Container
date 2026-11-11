use std::sync::Arc;

use crate::judge::{job::JudgeJob, testcase::Testcases};

pub mod checker;
pub mod compiler;
pub mod executor;
pub mod grader;
pub mod job;
pub mod s3_repository;
pub mod testcase;

pub struct Problem {
    pub memory_limit: u64,
    pub instruction_limit: u64,
    pub tests: Arc<Testcases>,
}

pub struct JudgeQueue {
    job_queue_sender: tokio::sync::mpsc::Sender<JudgeJob>,
}

impl JudgeQueue {
    pub fn new(max_concurrency: usize) -> Self {
        let (tx, mut rx) = tokio::sync::mpsc::channel::<JudgeJob>(100);
        tokio::spawn(async move {
            let semaphore =
                std::sync::Arc::new(tokio::sync::Semaphore::new(max_concurrency.max(1)));

            while let Some(job) = rx.recv().await {
                let permit = match semaphore.clone().acquire_owned().await {
                    Ok(permit) => permit,
                    Err(_) => break,
                };
                tokio::spawn(async move {
                    let _permit = permit;
                    if let Err(e) = job.run().await {
                        eprintln!("Failed to run JudgeJob: {}", e);
                    }
                });
            }
        });
        Self {
            job_queue_sender: tx,
        }
    }

    pub async fn submit(
        &self,
        request: jot_proto::judge::v1::JudgeRequest,
    ) -> Result<tokio::sync::mpsc::Receiver<jot_proto::judge::v1::JudgeEvent>, String> {
        let (tx, rx) = tokio::sync::mpsc::channel(100);
        let job = JudgeJob::new(request, tx)
            .await
            .map_err(|e| format!("Failed to create JudgeJob: {}", e))?;
        self.submit_job(job).await?;
        Ok(rx)
    }

    async fn submit_job(&self, job: JudgeJob) -> Result<(), String> {
        self.job_queue_sender
            .send(job)
            .await
            .map_err(|_| "Judge queue is closed".to_string())
    }
}
