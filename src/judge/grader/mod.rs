use tokio::sync::mpsc;

use crate::judge::ProblemRuntimeLimits;
use crate::judge::testcase::Testcases;

use super::checker::Checker;
use super::executor::{ExecuteError, ExecuteResult, Executor};

pub struct Grader<C: Checker, E: Executor> {
    pub checker: C,
    pub executor: E,
}

pub enum SubproblemVerdict {
    Correct(UsedResources),
    WrongAnswer,
    RuntimeError(String),
    TimeLimitExceeded(u64),
    MemoryLimitExceeded(u64),
    OutputLimitExceeded(usize),
}

pub type GraderResult = Result<SubproblemVerdict, String>;

pub struct UsedResources {
    pub memory_used: u64,
    pub instruction_count: u64,
}

pub enum GradeStreamData {
    Result(GraderResult),
    Progress { completed_cases: u32 },
}

impl<C: Checker + 'static, E: Executor + 'static> Grader<C, E> {
    pub fn new(checker: C, executor: E) -> Self {
        Self { checker, executor }
    }
    pub fn grade_stream(
        self,
        testcases: Testcases,
        problem_limits: ProblemRuntimeLimits,
    ) -> mpsc::Receiver<GradeStreamData> {
        let checker = self.checker;
        let executor = self.executor;
        let (tx, rx) = mpsc::channel(8);

        tokio::spawn(async move {
            let result = 'grading: {
                let mut used_resources = UsedResources {
                    memory_used: 0,
                    instruction_count: 0,
                };
                for (idx, testcase) in testcases.iter().enumerate() {
                    let (Ok(input), Ok(answer)) = (
                        std::str::from_utf8(&testcase.input),
                        std::str::from_utf8(&testcase.answer),
                    ) else {
                        break 'grading Err(format!("Testcase {} contains invalid UTF-8", idx + 1));
                    };
                    let execution = executor
                        .execute(
                            input,
                            problem_limits.memory_byte_limit,
                            problem_limits.instruction_count_limit,
                            Some(
                                (answer.len() as f64
                                    * f64::from(problem_limits.output_bytes_limit_multiplier))
                                    as usize,
                            ),
                        )
                        .await;
                    let ExecuteResult {
                        output,
                        memory_used,
                        instruction_count,
                    } = match execution {
                        Ok(result) => result,
                        Err(ExecuteError::TimeLimitExceeded(limit)) => {
                            break 'grading Ok(SubproblemVerdict::TimeLimitExceeded(limit));
                        }
                        Err(ExecuteError::MemoryLimitExceeded(limit)) => {
                            break 'grading Ok(SubproblemVerdict::MemoryLimitExceeded(limit));
                        }
                        Err(ExecuteError::OutputLimitExceeded(limit)) => {
                            break 'grading Ok(SubproblemVerdict::OutputLimitExceeded(limit));
                        }
                        Err(ExecuteError::RuntimeError { reason }) => {
                            let message = reason.unwrap_or_else(|| "Unknown runtime error".into());
                            break 'grading Ok(SubproblemVerdict::RuntimeError(message));
                        }
                    };

                    match checker.check(&output, answer).await {
                        Ok(true) => {}
                        Ok(false) => break 'grading Ok(SubproblemVerdict::WrongAnswer),
                        Err(error) => {
                            break 'grading Err(format!(
                                "Checker failed for testcase {}: {error}",
                                idx + 1
                            ));
                        }
                    }

                    used_resources.memory_used = used_resources.memory_used.max(memory_used);
                    used_resources.instruction_count =
                        used_resources.instruction_count.max(instruction_count);
                    if tx
                        .send(GradeStreamData::Progress {
                            completed_cases: (idx + 1) as u32,
                        })
                        .await
                        .is_err()
                    {
                        return;
                    }
                }
                Ok(SubproblemVerdict::Correct(used_resources))
            };
            let _ = tx.send(GradeStreamData::Result(result)).await;
        });

        rx
    }
}
