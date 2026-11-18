use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};

use futures_util::{TryStreamExt, stream::FuturesUnordered};

use crate::judge::{
    ProblemRuntimeLimits,
    checker::lcmp_checker::LcmpChecker,
    compiler,
    executor::Executor,
    grader::{GradeStreamData, Grader, SubproblemVerdict},
    testcase::{Testcases, TestcasesFetcher},
};
use jot_proto::judge::v1::{self as jot_proto};

pub type Error = Box<dyn std::error::Error + Send + Sync>;

async fn fetch_subproblem_testcases(
    problem_id: i64,
    subproblem_id: i64,
) -> Result<(i64, Testcases), String> {
    let fetcher = crate::config::testcase_fetcher();

    let testcases = fetcher
        .get_subproblem_testcases(problem_id, subproblem_id)
        .await
        .map_err(|e| {
            format!("Failed to fetch testcases for problem {problem_id}, subproblem {subproblem_id}: {e}")
        })?;
    if testcases.is_empty() {
        return Err(format!(
            "No testcases found for problem {problem_id}, subproblem {subproblem_id}"
        ));
    }
    Ok((subproblem_id, testcases))
}

pub struct JudgeJob {
    request: jot_proto::JudgeRequest,
    response_channel: tokio::sync::mpsc::Sender<jot_proto::JudgeEvent>,
}

impl JudgeJob {
    pub async fn new(
        request: jot_proto::JudgeRequest,
        response_channel: tokio::sync::mpsc::Sender<jot_proto::JudgeEvent>,
    ) -> Result<Self, String> {
        // send RECEIVED event to the response channel
        let event = jot_proto::JudgeEvent {
            request_id: request.request_id,
            payload: Some(jot_proto::judge_event::Payload::Received(
                jot_proto::JudgeReceived {},
            )),
        };
        response_channel
            .send(event)
            .await
            .map_err(|e| format!("Failed to send JudgePhase::RECEIVED event: {}", e))?;

        Ok(Self {
            request,
            response_channel,
        })
    }

    pub async fn run(self) -> Result<(), Error> {
        // 컴파일 시작 이벤트 발송
        self.response_channel
            .send(jot_proto::JudgeEvent {
                request_id: self.request.request_id,
                payload: Some(jot_proto::judge_event::Payload::Compiling(
                    jot_proto::JudgeCompiling {},
                )),
            })
            .await?;

        // 컴파일 진행
        let compiler = compiler::get_compiler(self.request.language());
        // Compile the source code

        let compiler_result = match compiler
            .compile(
                &self.request.source_code,
                compiler::CompilerResourceLimits {
                    time_limit_ms: self.request.compile_time_ms_limit,
                    build_artifact_size_limit_byte: self.request.build_artifact_bytes_limit,
                },
            )
            .await
        {
            Ok(compiler_result) => compiler_result,
            Err(e) => {
                let verdict = match e {
                    compiler::CompilerError::CompilationError(msg) => {
                        Some(jot_proto::judge_result::Verdict::CompileError(
                            jot_proto::JudgeCompileError {
                                compiler_diagnostics: msg,
                            },
                        ))
                    }
                    compiler::CompilerError::TimeLimitExceeded(limit) => Some(
                        jot_proto::judge_result::Verdict::CompileTle(jot_proto::JudgeCompileTle {
                            max_compile_time_ms: limit,
                        }),
                    ),
                    compiler::CompilerError::BuildArtifactSizeLimitExceeded(limit) => Some(
                        jot_proto::judge_result::Verdict::CompileBle(jot_proto::JudgeCompileBle {
                            max_build_artifact_bytes: limit,
                        }),
                    ),
                    compiler::CompilerError::RuntimeError(msg) => {
                        Some(jot_proto::judge_result::Verdict::SystemError(
                            jot_proto::JudgeSystemError { error_message: msg },
                        ))
                    }
                };
                let judge_result = jot_proto::JudgeResult { verdict };
                let event = jot_proto::JudgeEvent {
                    request_id: self.request.request_id,
                    payload: Some(jot_proto::judge_event::Payload::Result(judge_result)),
                };
                self.response_channel.send(event).await?;
                return Ok(());
            }
        };

        // Testcase 다운로드 시작
        self.response_channel
            .send(jot_proto::JudgeEvent {
                request_id: self.request.request_id,
                payload: Some(jot_proto::judge_event::Payload::TestcasesDownloading(
                    jot_proto::JudgeTestcasesDownloading {},
                )),
            })
            .await?;
        let problem_id = self.request.problem_id;
        let subproblem_ids: HashSet<_> = self.request.subproblem_ids.iter().copied().collect();
        let downloads = subproblem_ids
            .into_iter()
            .map(|subproblem_id| fetch_subproblem_testcases(problem_id, subproblem_id))
            .collect::<FuturesUnordered<_>>()
            .try_collect::<HashMap<i64, Testcases>>()
            .await;
        let mut testcases_by_subproblem_id = match downloads {
            Ok(testcases) => testcases,
            Err(error_message) => {
                return self
                    .send_result(jot_proto::judge_result::Verdict::SystemError(
                        jot_proto::JudgeSystemError { error_message },
                    ))
                    .await;
            }
        };

        // Running 이벤트 발송
        let executor: Arc<dyn Executor> = Arc::from(compiler_result.executor);
        let total_cases = testcases_by_subproblem_id
            .values()
            .map(Vec::len)
            .sum::<usize>() as u32;
        let mut complete = jot_proto::JudgeComplete::default();
        let mut completed_cases = 0;
        self.send_running(completed_cases, total_cases).await?;

        // 각 subproblem에 대해 grading 수행
        for &subproblem_id in &self.request.subproblem_ids {
            let Some(testcases) = testcases_by_subproblem_id.remove(&subproblem_id) else {
                continue;
            };
            let grader = Grader::new(LcmpChecker::new(), Arc::clone(&executor));
            let mut stream = grader.grade_stream(
                testcases,
                ProblemRuntimeLimits {
                    memory_byte_limit: self.request.memory_bytes_limit,
                    instruction_count_limit: self.request.instruction_count_limit,
                },
            );

            let mut subproblem_completed_cases = 0;
            let result = loop {
                match stream.recv().await {
                    Some(GradeStreamData::Progress {
                        completed_cases: count,
                    }) => {
                        subproblem_completed_cases = count;
                        // completed_cases와 subproblem_completed_cases를 합쳐서 전체 진행 상황을 업데이트
                        self.send_running(completed_cases + count, total_cases)
                            .await?;
                    }
                    Some(GradeStreamData::Result(result)) => break result,
                    None => break Err("Grading stream closed without a result".into()),
                }
            };

            completed_cases += subproblem_completed_cases;
            let verdict = match result {
                Ok(verdict) => verdict,
                Err(error) => {
                    return self
                        .send_result(jot_proto::judge_result::Verdict::SystemError(
                            jot_proto::JudgeSystemError {
                                error_message: format!("Subproblem {subproblem_id}: {error}"),
                            },
                        ))
                        .await;
                }
            };
            complete
                .subproblem_verdicts
                .insert(subproblem_id, verdict.into());
        }

        // 모든 subproblem에 대한 grading이 완료되었으므로 최종 결과를 전송
        self.send_result(jot_proto::judge_result::Verdict::Complete(complete))
            .await
    }

    async fn send_result(&self, verdict: jot_proto::judge_result::Verdict) -> Result<(), Error> {
        let event = jot_proto::JudgeEvent {
            request_id: self.request.request_id,
            payload: Some(jot_proto::judge_event::Payload::Result(
                jot_proto::JudgeResult {
                    verdict: Some(verdict),
                },
            )),
        };
        self.response_channel.send(event).await?;
        Ok(())
    }

    async fn send_running(&self, completed_cases: u32, total_cases: u32) -> Result<(), Error> {
        self.response_channel
            .send(jot_proto::JudgeEvent {
                request_id: self.request.request_id,
                payload: Some(jot_proto::judge_event::Payload::Running(
                    jot_proto::JudgeRunning {
                        completed_cases,
                        total_cases,
                    },
                )),
            })
            .await?;
        Ok(())
    }
}

impl From<SubproblemVerdict> for jot_proto::SubproblemVerdict {
    fn from(verdict: SubproblemVerdict) -> Self {
        match verdict {
            SubproblemVerdict::Correct(resources) => jot_proto::SubproblemVerdict {
                subproblem_verdict: Some(
                    jot_proto::subproblem_verdict::SubproblemVerdict::Correct(
                        jot_proto::SubproblemCorrect {
                            max_memory_byte_used: resources.memory_used,
                            max_instruction_count_used: resources.instruction_count,
                        },
                    ),
                ),
            },
            SubproblemVerdict::WrongAnswer => jot_proto::SubproblemVerdict {
                subproblem_verdict: Some(
                    jot_proto::subproblem_verdict::SubproblemVerdict::WrongAnswer(
                        jot_proto::SubproblemWrongAnswer {},
                    ),
                ),
            },
            SubproblemVerdict::TimeLimitExceeded(max_instruction_count) => {
                jot_proto::SubproblemVerdict {
                    subproblem_verdict: Some(
                        jot_proto::subproblem_verdict::SubproblemVerdict::TimeLimitExceeded(
                            jot_proto::SubproblemTle {
                                max_instruction_count,
                            },
                        ),
                    ),
                }
            }
            SubproblemVerdict::MemoryLimitExceeded(max_memory_bytes) => {
                jot_proto::SubproblemVerdict {
                    subproblem_verdict: Some(
                        jot_proto::subproblem_verdict::SubproblemVerdict::MemoryLimitExceeded(
                            jot_proto::SubproblemMle { max_memory_bytes },
                        ),
                    ),
                }
            }
            SubproblemVerdict::RuntimeError(error_message) => jot_proto::SubproblemVerdict {
                subproblem_verdict: Some(
                    jot_proto::subproblem_verdict::SubproblemVerdict::RuntimeError(
                        jot_proto::SubproblemRuntimeError { error_message },
                    ),
                ),
            },
        }
    }
}
