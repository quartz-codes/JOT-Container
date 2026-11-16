use tokio::sync::mpsc;

use crate::judge::ProblemRuntimeLimits;
use crate::judge::testcase::Testcases;

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
        // total_cases: u32,
    },
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
            let mut grade_result = GradeResult {
                verdict: Verdict::SystemError(Some("Unknown error".to_string())),
                memory_used: 0,
                instruction_count: 0,
            };
            for (idx, testcase) in testcases.iter().enumerate() {
                let (Ok(input), Ok(answer)) = (
                    std::str::from_utf8(&testcase.input),
                    std::str::from_utf8(&testcase.answer),
                ) else {
                    grade_result.verdict = Verdict::SystemError(Some(format!(
                        "Testcase {} contains invalid UTF-8",
                        idx + 1
                    )));
                    let _ = tx.send(GradeStreamData::Result(grade_result)).await;
                    return;
                };
                let res = executor
                    .execute(
                        input,
                        problem_limits.memory_byte_limit,
                        problem_limits.instruction_count_limit,
                    )
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
                            grade_result.verdict = Verdict::WrongAnswer;
                            let _ = tx.send(GradeStreamData::Result(grade_result)).await;
                            return;
                        }
                        let _ = tx
                            .send(GradeStreamData::Progress {
                                completed_cases: (idx + 1) as u32,
                                // total_cases: testcases.len() as u32,
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

// #[tokio::test]
// async fn test_grader() {
//     use super::checker::lcmp_checker::LcmpChecker;
//     use super::executor::aplusb_executor::APlusBExecutor;
//     use crate::judge::testcase::Testcase;
//     use std::sync::Arc;

//     let problem = Subproblem {
//         id: 1,
//         memory_limit: 1024 * 1024,
//         instruction_limit: 100000000,
//         testcases: Arc::new(vec![
//             Testcase {
//                 input: "1 2".into(),
//                 answer: "3".into(),
//             },
//             Testcase {
//                 input: "100 200".into(),
//                 answer: "300".into(),
//             },
//         ]),
//         memory_limit: 1024 * 1024,
//         instruction_limit: 100000000,
//     };

//     let grader = Grader::new(LcmpChecker, APlusBExecutor);
//     let result = grader.grade(problem).await;

//     assert_eq!(matches!(result.verdict, Verdict::Accepted), true);
// }

// #[tokio::test]
// async fn test_grader_instruction_limit() {
//     use super::checker::lcmp_checker::LcmpChecker;
//     use super::executor::aplusb_executor::APlusBExecutor;
//     use crate::judge::testcase::Testcase;
//     use std::sync::Arc;

//     let problem = Subproblem {
//         tests: Arc::new(vec![
//             Testcase {
//                 input: "1 2".into(),
//                 answer: "3".into(),
//             },
//             Testcase {
//                 input: "100 200".into(),
//                 answer: "300".into(),
//             },
//         ]),
//         memory_limit: 1024 * 1024,
//         instruction_limit: 1,
//     };

//     let grader = Grader::new(LcmpChecker, APlusBExecutor);
//     let result = grader.grade(problem).await;

//     if let Verdict::TimeLimitExceeded(_) = result.verdict {
//         return;
//     } else {
//         panic!("Expected TimeLimitExceeded, got {:?}", result.verdict);
//     }
// }
