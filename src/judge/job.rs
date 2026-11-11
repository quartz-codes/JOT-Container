use std::sync::Arc;

use crate::judge::{
    Problem,
    checker::lcmp_checker::LcmpChecker,
    compiler,
    grader::{self, Grader},
    testcase::{TestcaseFetcher, Testcases},
};
use jot_proto::judge::v1::{self as proto, JudgeEvent, judge_result::Verdict};

pub type Error = Box<dyn std::error::Error + Send + Sync>;

// JudgeRequest currently identifies only a problem.
const DEFAULT_SUBPROBLEM_ID: i64 = 1;

async fn fetch_testcases(problem_id: i64) -> Result<Arc<Testcases>, String> {
    let fetcher = crate::config::testcase_fetcher();
    let mut stream = fetcher
        .get_subproblem_test_cases(problem_id, DEFAULT_SUBPROBLEM_ID)
        .await;
    let mut cases = Vec::new();
    while let Some(item) = stream.recv().await {
        cases.push(item.map_err(|error| error.to_string())?);
    }
    if cases.is_empty() {
        return Err("Testcases not found".into());
    }
    Ok(Arc::new(cases))
}

pub struct JudgeJob {
    request: jot_proto::judge::v1::JudgeRequest,
    response_channel: tokio::sync::mpsc::Sender<jot_proto::judge::v1::JudgeEvent>,
}

impl JudgeJob {
    pub async fn new(
        request: jot_proto::judge::v1::JudgeRequest,
        response_channel: tokio::sync::mpsc::Sender<jot_proto::judge::v1::JudgeEvent>,
    ) -> Result<Self, String> {
        // send RECEIVED event to the response channel
        let event = JudgeEvent {
            request_id: request.request_id,
            payload: Some(jot_proto::judge::v1::judge_event::Payload::Received(
                jot_proto::judge::v1::JudgeReceived {},
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
        // preload를 여기서 호출할 수 있음
        // PROBLEM_FETCHER.preload(self.request.problem_id);

        // 컴파일 시작 이벤트 발송
        self.response_channel
            .send(JudgeEvent {
                request_id: self.request.request_id,
                payload: Some(jot_proto::judge::v1::judge_event::Payload::Compiling(
                    jot_proto::judge::v1::JudgeCompiling {},
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
                        Some(jot_proto::judge::v1::judge_result::Verdict::CompileError(
                            jot_proto::judge::v1::JudgeCompileError {
                                compiler_diagnostics: msg,
                            },
                        ))
                    }
                    compiler::CompilerError::TimeLimitExceeded(limit) => {
                        Some(jot_proto::judge::v1::judge_result::Verdict::CompileTle(
                            jot_proto::judge::v1::JudgeCompileTle {
                                max_compile_time_ms: limit,
                            },
                        ))
                    }
                    compiler::CompilerError::BuildArtifactSizeLimitExceeded(limit) => {
                        Some(jot_proto::judge::v1::judge_result::Verdict::CompileBle(
                            jot_proto::judge::v1::JudgeCompileBle {
                                max_build_artifact_bytes: limit,
                            },
                        ))
                    }
                    compiler::CompilerError::RuntimeError(msg) => {
                        Some(jot_proto::judge::v1::judge_result::Verdict::SystemError(
                            jot_proto::judge::v1::JudgeSystemError { error_message: msg },
                        ))
                    }
                };
                let judge_result = jot_proto::judge::v1::JudgeResult { verdict };
                let event = JudgeEvent {
                    request_id: self.request.request_id,
                    payload: Some(jot_proto::judge::v1::judge_event::Payload::Result(
                        judge_result,
                    )),
                };
                self.response_channel.send(event).await?;
                return Ok(());
            }
        };

        // 테스트 케이스 다운로드 시작 이벤트 발송
        self.response_channel
            .send(JudgeEvent {
                request_id: self.request.request_id,
                payload: Some(
                    jot_proto::judge::v1::judge_event::Payload::FetchingTestCasesMetadata(
                        proto::JudgeFetchingTestCasesMetadata {},
                    ),
                ),
            })
            .await?;

        let testcases = match fetch_testcases(self.request.problem_id).await {
            Ok(testcases) => testcases,
            Err(error_message) => {
                self.response_channel
                    .send(JudgeEvent {
                        request_id: self.request.request_id,
                        payload: Some(proto::judge_event::Payload::Result(proto::JudgeResult {
                            verdict: Some(Verdict::SystemError(proto::JudgeSystemError {
                                error_message,
                            })),
                        })),
                    })
                    .await?;
                return Ok(());
            }
        };

        let total_cases = testcases.len() as u32;

        // 실행 중 이벤트 발송
        self.response_channel
            .send(JudgeEvent {
                request_id: self.request.request_id,
                payload: Some(jot_proto::judge::v1::judge_event::Payload::Running(
                    proto::JudgeRunning {
                        completed_cases: 0,
                        total_cases,
                    },
                )),
            })
            .await?;

        let grader = Grader::new(LcmpChecker::new(), compiler_result.executor);

        let mut stream = grader.grade_stream(Problem {
            memory_limit: self.request.memory_bytes_limit,
            instruction_limit: self.request.instruction_count_limit,
            tests: testcases,
        });
        let grade_result = loop {
            let Some(data) = stream.recv().await else {
                self.response_channel
                    .send(JudgeEvent {
                        request_id: self.request.request_id,
                        payload: Some(jot_proto::judge::v1::judge_event::Payload::Result(
                            jot_proto::judge::v1::JudgeResult {
                                verdict: Some(Verdict::SystemError(proto::JudgeSystemError {
                                    error_message: "Grader stream closed unexpectedly".into(),
                                })),
                                ..Default::default()
                            },
                        )),
                    })
                    .await?;
                return Ok(());
            };
            match data {
                grader::GradeStreamData::Progress {
                    completed_cases,
                    total_cases,
                } => {
                    let event = JudgeEvent {
                        request_id: self.request.request_id,
                        payload: Some(jot_proto::judge::v1::judge_event::Payload::Running(
                            proto::JudgeRunning {
                                completed_cases,
                                total_cases,
                            },
                        )),
                    };
                    self.response_channel.send(event).await?;
                }
                grader::GradeStreamData::Result(grade_result) => {
                    break grade_result;
                }
            }
        };

        let verdict = match grade_result.verdict {
            // 현재 subproblem이 지원되지 않기 때문에 맞은 subproblem 개수는 1
            grader::Verdict::Accepted => Verdict::Complete(proto::JudgeComplete {
                correct_subproblems: 1,
                total_subproblems: 1,
                instruction_count: grade_result.instruction_count,
                memory_bytes: grade_result.memory_used,
            }),
            grader::Verdict::WrongAnswer => Verdict::WrongAnswer(proto::JudgeWrongAnswer {
                message: "Wrong answer".into(),
            }),
            grader::Verdict::TimeLimitExceeded(limit) => {
                Verdict::TimeLimitExceeded(proto::JudgeTle {
                    max_instruction_count: limit,
                })
            }
            grader::Verdict::MemoryLimitExceeded(limit) => {
                Verdict::MemoryLimitExceeded(proto::JudgeMle {
                    max_memory_bytes: limit,
                })
            }
            grader::Verdict::RuntimeError(error) | grader::Verdict::SystemError(error) => {
                Verdict::SystemError(proto::JudgeSystemError {
                    error_message: error.unwrap_or_else(|| "Unknown execution error".into()),
                })
            }
        };

        let judge_result = proto::JudgeResult {
            verdict: Some(verdict),
        };
        let event = JudgeEvent {
            request_id: self.request.request_id,
            payload: Some(jot_proto::judge::v1::judge_event::Payload::Result(
                judge_result,
            )),
        };
        self.response_channel.send(event).await?;
        Ok(())
    }
}
