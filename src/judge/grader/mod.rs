use tokio::sync::mpsc;

use crate::judge::Problem;

use super::checker::Checker;
use super::executor::{ExecuteError, ExecuteResult, Executor};

pub struct Grader<C: Checker, E: Executor> {
    pub checker: C,
    pub executor: E,
}

#[derive(Debug)]
pub enum Verdict {
    Accepted,
    WrongAnswer,
    RuntimeError(Option<String>),
    TimeLimitExceeded(u64),
    MemoryLimitExceeded(u64),
    SystemError(Option<String>),
}
#[derive(Debug)]
pub struct GradeResult {
    pub verdict: Verdict,
    pub memory_used: u64,
    pub instruction_count: u64,
}

#[derive(Debug)]
pub enum GradeStreamData {
    Result(GradeResult),
    Progress {
        completed_cases: u32,
        total_cases: u32,
    },
}

impl<C: Checker + 'static, E: Executor + 'static> Grader<C, E> {
    pub fn new(checker: C, executor: E) -> Self {
        Self { checker, executor }
    }
    // Grader::grade() is now useless and it is only used for testing
    // Now it is implemented to call Grader::grade_stream() and collect the result from the stream
    // to test the Grader::grade_stream() function
    #[cfg(test)]
    pub async fn grade(self, problem: Problem) -> GradeResult {
        let mut stream = self.grade_stream(problem);
        let mut result = None;
        while let Some(data) = stream.recv().await {
            match data {
                GradeStreamData::Result(r) => {
                    result = Some(r);
                }
                GradeStreamData::Progress { .. } => {}
            }
        }
        result.unwrap_or(GradeResult {
            verdict: Verdict::SystemError(Some("Unknown error".to_string())),
            memory_used: 0,
            instruction_count: 0,
        })
    }
    pub fn grade_stream(self, problem: Problem) -> mpsc::Receiver<GradeStreamData> {
        let checker = self.checker;
        let executor = self.executor;
        let (tx, rx) = mpsc::channel(8);

        tokio::spawn(async move {
            let mut grade_result = GradeResult {
                verdict: Verdict::SystemError(Some("Unknown error".to_string())),
                memory_used: 0,
                instruction_count: 0,
            };
            for (idx, test) in problem.tests.iter().enumerate() {
                let (Ok(input), Ok(answer)) = (
                    std::str::from_utf8(&test.input),
                    std::str::from_utf8(&test.answer),
                ) else {
                    grade_result.verdict = Verdict::SystemError(Some(format!(
                        "Testcase {} contains invalid UTF-8",
                        idx + 1
                    )));
                    let _ = tx.send(GradeStreamData::Result(grade_result)).await;
                    return;
                };
                let res = executor
                    .execute(input, problem.memory_limit, problem.instruction_limit)
                    .await;
                match res {
                    Ok(ExecuteResult {
                        output,
                        memory_used,
                        instruction_count,
                    }) => {
                        grade_result.memory_used = grade_result.memory_used.max(memory_used);
                        grade_result.instruction_count =
                            grade_result.instruction_count.max(instruction_count);

                        if !checker.check(&output, answer).await.unwrap_or(false) {
                            let _ = tx
                                .send(GradeStreamData::Result(GradeResult {
                                    verdict: Verdict::WrongAnswer,
                                    memory_used,
                                    instruction_count,
                                }))
                                .await;
                            return;
                        }
                        let _ = tx
                            .send(GradeStreamData::Progress {
                                completed_cases: (idx + 1) as u32,
                                total_cases: problem.tests.len() as u32,
                            })
                            .await;
                    }
                    Err(ExecuteError::TimeLimitExceeded(time)) => {
                        grade_result.verdict = Verdict::TimeLimitExceeded(time);
                        let _ = tx.send(GradeStreamData::Result(grade_result)).await;
                        return;
                    }
                    Err(ExecuteError::MemoryLimitExceeded(memory)) => {
                        grade_result.verdict = Verdict::MemoryLimitExceeded(memory);
                        let _ = tx.send(GradeStreamData::Result(grade_result)).await;
                        return;
                    }
                    Err(ExecuteError::RuntimeError { reason }) => {
                        grade_result.verdict = Verdict::RuntimeError(reason);
                        let _ = tx.send(GradeStreamData::Result(grade_result)).await;
                        return;
                    }
                }
            }

            grade_result.verdict = Verdict::Accepted;
            tx.send(GradeStreamData::Result(grade_result))
                .await
                .unwrap();
        });

        rx
    }
}

#[tokio::test]
async fn test_grader() {
    use super::checker::lcmp_checker::LcmpChecker;
    use super::executor::aplusb_executor::APlusBExecutor;
    use crate::judge::testcase::Testcase;
    use std::sync::Arc;

    let problem = Problem {
        tests: Arc::new(vec![
            Testcase {
                input: "1 2".into(),
                answer: "3".into(),
            },
            Testcase {
                input: "100 200".into(),
                answer: "300".into(),
            },
        ]),
        memory_limit: 1024 * 1024,
        instruction_limit: 100000000,
    };

    let grader = Grader::new(LcmpChecker, APlusBExecutor);
    let result = grader.grade(problem).await;

    assert_eq!(matches!(result.verdict, Verdict::Accepted), true);
}

#[tokio::test]
async fn test_grader_instruction_limit() {
    use super::checker::lcmp_checker::LcmpChecker;
    use super::executor::aplusb_executor::APlusBExecutor;
    use crate::judge::testcase::Testcase;
    use std::sync::Arc;

    let problem = Problem {
        tests: Arc::new(vec![
            Testcase {
                input: "1 2".into(),
                answer: "3".into(),
            },
            Testcase {
                input: "100 200".into(),
                answer: "300".into(),
            },
        ]),
        memory_limit: 1024 * 1024,
        instruction_limit: 1,
    };

    let grader = Grader::new(LcmpChecker, APlusBExecutor);
    let result = grader.grade(problem).await;

    if let Verdict::TimeLimitExceeded(_) = result.verdict {
        return;
    } else {
        panic!("Expected TimeLimitExceeded, got {:?}", result.verdict);
    }
}
