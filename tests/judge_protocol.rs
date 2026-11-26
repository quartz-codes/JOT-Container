use std::{net::TcpListener, time::Duration};

use jot_proto::judge::v1::{
    Checker, JudgeEvent, JudgeRequest, JudgeResult, Language, Subproblem, TestcaseOrigin,
    judge_event::Payload, judge_result::Verdict, judge_service_client::JudgeServiceClient,
    subproblem_verdict::SubproblemVerdict,
};
use tokio::process::{Child, Command};
use tonic::transport::{Channel, Endpoint};

#[path = "support/mock_http.rs"]
mod mock_http;

use mock_http::MockHttp;

fn subproblems(http: &MockHttp, ids: &[i64]) -> std::collections::HashMap<i64, Subproblem> {
    ids.iter()
        .map(|&id| {
            (
                id,
                Subproblem {
                    subproblem_id: id,
                    testcase_origin: TestcaseOrigin::Https as i32,
                    testcase_url: http.url(id),
                    checker: Checker::Lcmp as i32,
                },
            )
        })
        .collect()
}

fn request(request_id: i64, http: &MockHttp) -> JudgeRequest {
    JudgeRequest {
        request_id,
        submission_id: request_id,
        problem_id: 1,
        subproblems: subproblems(http, &[7]),
        language: Language::Cpp as i32,
        source_code: r#"#include <iostream>
int main() {
    long long a, b;
    std::cin >> a >> b;
    std::cout << a + b << '\n';
}
"#
        .into(),
        compile_time_ms_limit: 60_000,
        instruction_count_limit: 100_000_000,
        memory_bytes_limit: 64 * 1024 * 1024,
        build_artifact_bytes_limit: 16 * 1024 * 1024,
        output_bytes_limit_multiplier: 200.0,
    }
}

async fn start_server() -> (Child, JudgeServiceClient<Channel>, MockHttp) {
    let http = MockHttp::start().await;
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    drop(listener);

    let mut command = Command::new(env!("CARGO_BIN_EXE_JOT-Container"));
    let mut server = command
        .env("GRPC_ADDR", address.to_string())
        .kill_on_drop(true)
        .spawn()
        .expect("failed to spawn gRPC server");
    let endpoint = Endpoint::from_shared(format!("http://{address}"))
        .unwrap()
        .connect_timeout(Duration::from_secs(1));
    let channel = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            assert!(
                server.try_wait().unwrap().is_none(),
                "gRPC server exited before accepting connections"
            );
            match endpoint.connect().await {
                Ok(channel) => break channel,
                Err(_) => tokio::time::sleep(Duration::from_millis(50)).await,
            }
        }
    })
    .await
    .expect("gRPC server did not start");
    (server, JudgeServiceClient::new(channel), http)
}

async fn judge_events(
    client: &mut JudgeServiceClient<Channel>,
    request: JudgeRequest,
) -> Vec<JudgeEvent> {
    tokio::time::timeout(Duration::from_secs(120), async {
        let mut stream = client
            .judge(request)
            .await
            .expect("Judge RPC failed")
            .into_inner();
        let mut events = Vec::new();
        while let Some(event) = stream.message().await.expect("Judge stream failed") {
            events.push(event);
        }
        events
    })
    .await
    .expect("Judge stream did not finish")
}

fn terminal_result(events: &[JudgeEvent]) -> &JudgeResult {
    let (last, progress) = events.split_last().expect("Judge stream was empty");
    for event in progress {
        assert!(
            matches!(
                &event.payload,
                Some(
                    Payload::Received(_)
                        | Payload::Compiling(_)
                        | Payload::TestcasesDownloading(_)
                        | Payload::Running(_)
                )
            ),
            "only progress events may precede the terminal result: {event:?}"
        );
    }
    match &last.payload {
        Some(Payload::Result(result)) => result,
        payload => panic!("expected a terminal result, got {payload:?}"),
    }
}

fn assert_compile_progress(events: &[JudgeEvent]) {
    assert_eq!(
        events.len(),
        3,
        "expected received, compiling, result: {events:?}"
    );
    assert!(matches!(events[0].payload, Some(Payload::Received(_))));
    assert!(matches!(events[1].payload, Some(Payload::Compiling(_))));
}

fn assert_complete(events: &[JudgeEvent], subproblem_ids: &[i64]) {
    assert!(matches!(events[0].payload, Some(Payload::Received(_))));
    assert!(matches!(events[1].payload, Some(Payload::Compiling(_))));
    assert!(matches!(
        events[2].payload,
        Some(Payload::TestcasesDownloading(_))
    ));
    let total_cases = subproblem_ids.len() as u32 * 2;
    let progress: Vec<_> = events
        .iter()
        .filter_map(|event| match &event.payload {
            Some(Payload::Running(running)) => {
                assert_eq!(running.total_cases, total_cases);
                Some(running.completed_cases)
            }
            _ => None,
        })
        .collect();
    assert_eq!(progress, (0..=total_cases).collect::<Vec<_>>());
    match &terminal_result(events).verdict {
        Some(Verdict::Complete(complete)) => {
            assert_eq!(complete.subproblem_verdicts.len(), subproblem_ids.len());
            for id in subproblem_ids {
                assert!(matches!(
                    &complete.subproblem_verdicts[id].subproblem_verdict,
                    Some(SubproblemVerdict::Correct(resources))
                        if resources.max_instruction_count_used > 0
                            && resources.max_memory_byte_used > 0
                ));
            }
        }
        verdict => panic!("expected complete verdict, got {verdict:?}"),
    }
}

#[tokio::test]
async fn judge_rpc_stream_ends_with_a_terminal_result() {
    let (mut server, mut client, http) = start_server().await;
    for request_id in [1, 2] {
        let mut request = request(request_id, &http);
        if request_id == 2 {
            // A renewed URL must reuse the testcases cached by problem/subproblem ID.
            request
                .subproblems
                .get_mut(&7)
                .unwrap()
                .testcase_url
                .push_str("&renewed=1");
        }
        let events = judge_events(&mut client, request).await;
        assert!(events.iter().all(|event| event.request_id == request_id));
        assert_complete(&events, &[7]);
    }
    assert_eq!(
        http.downloads(),
        1,
        "requests for the same problem/subproblem pair should reuse the cached testcases"
    );
    server.kill().await.unwrap();
}

#[tokio::test]
async fn judge_rpc_reports_incorrect_subproblems_in_the_complete_result() {
    let (mut server, mut client, http) = start_server().await;
    let mut request = request(7, &http);
    request.subproblems = subproblems(&http, &[7, 12, 7]);
    request.source_code = "#include <iostream>\nint main() { std::cout << \"wrong\\n\"; }".into();

    let events = judge_events(&mut client, request).await;
    match &terminal_result(&events).verdict {
        Some(Verdict::Complete(complete)) => {
            assert_eq!(complete.subproblem_verdicts.len(), 2);
            for id in [7, 12] {
                assert!(matches!(
                    complete.subproblem_verdicts[&id].subproblem_verdict,
                    Some(SubproblemVerdict::WrongAnswer(_))
                ));
            }
        }
        verdict => panic!("expected complete verdict, got {verdict:?}"),
    }
    server.kill().await.unwrap();
}

#[tokio::test]
async fn judge_rpc_reports_time_limit_for_each_subproblem() {
    let (mut server, mut client, http) = start_server().await;
    let mut request = request(8, &http);
    request.subproblems = subproblems(&http, &[7, 12]);
    request.language = Language::C as i32;
    request.source_code =
        "int main(void) { volatile unsigned count = 0; for (;;) { ++count; } }".into();
    let instruction_limit = 100_000;
    request.instruction_count_limit = instruction_limit;

    let events = judge_events(&mut client, request).await;
    match &terminal_result(&events).verdict {
        Some(Verdict::Complete(complete)) => {
            assert_eq!(complete.subproblem_verdicts.len(), 2);
            for id in [7, 12] {
                let verdict = &complete.subproblem_verdicts[&id].subproblem_verdict;
                assert!(
                    matches!(
                        verdict,
                        Some(SubproblemVerdict::TimeLimitExceeded(limit))
                            if limit.max_instruction_count == instruction_limit
                    ),
                    "unexpected verdict for subproblem {id}: {verdict:?}"
                );
            }
        }
        verdict => panic!("expected complete verdict, got {verdict:?}"),
    }
    server.kill().await.unwrap();
}

#[tokio::test]
async fn judge_rpc_returns_system_error_when_testcase_download_fails() {
    let (mut server, mut client, http) = start_server().await;
    let mut request = request(5, &http);
    request.subproblems = subproblems(&http, &[7, 99]);

    let events = judge_events(&mut client, request).await;
    assert!(events.iter().all(|event| event.request_id == 5));
    assert_eq!(events.len(), 4);
    assert!(matches!(
        events[2].payload,
        Some(Payload::TestcasesDownloading(_))
    ));
    match &terminal_result(&events).verdict {
        Some(Verdict::SystemError(error)) => {
            assert!(error.error_message.contains("problem 1, subproblem 99"));
        }
        verdict => panic!("expected system error, got {verdict:?}"),
    }
    server.kill().await.unwrap();
}

#[tokio::test]
async fn judge_rpc_returns_system_error_for_an_empty_testcase_list() {
    let (mut server, mut client, http) = start_server().await;
    let mut request = request(6, &http);
    request.subproblems = subproblems(&http, &[7, 13]);

    let events = judge_events(&mut client, request).await;
    assert_eq!(events.len(), 4);
    match &terminal_result(&events).verdict {
        Some(Verdict::SystemError(error)) => assert_eq!(
            error.error_message,
            "No testcases found for problem 1, subproblem 13"
        ),
        verdict => panic!("expected system error, got {verdict:?}"),
    }
    server.kill().await.unwrap();
}

#[tokio::test]
async fn judge_rpc_stream_returns_compilation_error_as_terminal_result() {
    let (mut server, mut client, http) = start_server().await;
    let mut invalid_request = request(2, &http);
    invalid_request.source_code = "this is not valid C++".into();

    let events = judge_events(&mut client, invalid_request).await;
    assert!(events.iter().all(|event| event.request_id == 2));
    assert_compile_progress(&events);
    match &terminal_result(&events).verdict {
        Some(Verdict::CompileError(error)) => assert!(!error.compiler_diagnostics.is_empty()),
        verdict => panic!("expected compile error, got {verdict:?}"),
    }
    server.kill().await.unwrap();
}

#[tokio::test]
async fn judge_queue_streams_concurrent_requests_with_independent_terminal_results() {
    let (mut server, client, http) = start_server().await;
    let mut tasks = tokio::task::JoinSet::new();

    for request_id in 10..13 {
        let mut client = client.clone();
        let mut request = request(request_id, &http);
        request.subproblems = subproblems(&http, &[12, 7, 12]);
        tasks.spawn(async move { (request_id, judge_events(&mut client, request).await) });
    }

    while let Some(task) = tasks.join_next().await {
        let (request_id, events) = task.expect("judge request task failed");
        assert!(events.iter().all(|event| event.request_id == request_id));
        assert_complete(&events, &[7, 12]);
    }
    assert_eq!(
        http.downloads(),
        2,
        "expected one GET per distinct problem/subproblem pair"
    );
    server.kill().await.unwrap();
}

#[tokio::test]
async fn judge_rpc_returns_compile_time_limit_with_requested_limit() {
    let (mut server, mut client, http) = start_server().await;
    let mut request = request(3, &http);
    request.compile_time_ms_limit = 0;
    let events = judge_events(&mut client, request).await;
    assert!(events.iter().all(|event| event.request_id == 3));
    assert_compile_progress(&events);
    match &terminal_result(&events).verdict {
        Some(Verdict::CompileTle(error)) => assert_eq!(error.max_compile_time_ms, 0),
        verdict => panic!("expected compile time limit, got {verdict:?}"),
    }
    server.kill().await.unwrap();
}

#[tokio::test]
async fn judge_rpc_returns_compile_artifact_limit_with_requested_limit() {
    let (mut server, mut client, http) = start_server().await;
    let mut request = request(4, &http);
    request.build_artifact_bytes_limit = 1;
    let events = judge_events(&mut client, request).await;
    assert!(events.iter().all(|event| event.request_id == 4));
    assert_compile_progress(&events);
    match &terminal_result(&events).verdict {
        Some(Verdict::CompileBle(error)) => assert_eq!(error.max_build_artifact_bytes, 1),
        verdict => panic!("expected compile artifact limit, got {verdict:?}"),
    }
    server.kill().await.unwrap();
}
