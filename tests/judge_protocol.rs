use std::{net::TcpListener, time::Duration};

use jot_proto::judge::v1::{
    JudgeEvent, JudgeRequest, JudgeResult, Language, judge_event::Payload, judge_result::Verdict,
    judge_service_client::JudgeServiceClient,
};
use tokio::process::{Child, Command};
use tonic::transport::{Channel, Endpoint};

#[path = "support/mock_s3.rs"]
mod mock_s3;

use mock_s3::MockS3;

fn request(request_id: i64) -> JudgeRequest {
    JudgeRequest {
        request_id,
        problem_id: 1,
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
    }
}

async fn start_server() -> (Child, JudgeServiceClient<Channel>, MockS3) {
    let s3 = MockS3::start().await;
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    drop(listener);

    let mut command = Command::new(env!("CARGO_BIN_EXE_JOT-Container"));
    s3.configure(&mut command);
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
    (server, JudgeServiceClient::new(channel), s3)
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
                        | Payload::FetchingTestCasesMetadata(_)
                        | Payload::Running(_)
                        | Payload::Finishing(_)
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

fn assert_complete(events: &[JudgeEvent]) {
    // 기존에는 events.len()이 5인 것이 맞지만 스트리밍 기능이 추가되면서
    // 이벤트 수가 항상 5가 아니게 되어 주석처리함
    // assert_eq!(events.len(), 5, "unexpected event sequence: {events:?}");
    assert!(matches!(events[0].payload, Some(Payload::Received(_))));
    assert!(matches!(events[1].payload, Some(Payload::Compiling(_))));
    assert!(matches!(
        events[2].payload,
        Some(Payload::FetchingTestCasesMetadata(_))
    ));
    match &events[3].payload {
        Some(Payload::Running(running)) => {
            assert_eq!(running.completed_cases, 0);
            assert_eq!(running.total_cases, 2);
        }
        payload => panic!("expected running event, got {payload:?}"),
    }
    match &terminal_result(events).verdict {
        Some(Verdict::Complete(complete)) => {
            assert_eq!(complete.correct_subproblems, 1);
            assert_eq!(complete.total_subproblems, 1);
            assert!(complete.instruction_count > 0);
            assert!(complete.memory_bytes > 0);
        }
        verdict => panic!("expected complete verdict, got {verdict:?}"),
    }
}

#[tokio::test]
async fn judge_rpc_stream_ends_with_a_terminal_result() {
    let (mut server, mut client, _s3) = start_server().await;
    let events = judge_events(&mut client, request(1)).await;
    assert!(events.iter().all(|event| event.request_id == 1));
    assert_complete(&events);
    server.kill().await.unwrap();
}

#[tokio::test]
async fn judge_rpc_stream_returns_compilation_error_as_terminal_result() {
    let (mut server, mut client, _s3) = start_server().await;
    let mut invalid_request = request(2);
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
    let (mut server, client, s3) = start_server().await;
    let mut tasks = tokio::task::JoinSet::new();

    for request_id in 10..13 {
        let mut client = client.clone();
        tasks.spawn(async move {
            (
                request_id,
                judge_events(&mut client, request(request_id)).await,
            )
        });
    }

    while let Some(task) = tasks.join_next().await {
        let (request_id, events) = task.expect("judge request task failed");
        assert!(events.iter().all(|event| event.request_id == request_id));
        assert_complete(&events);
    }
    assert_eq!(
        s3.downloads(),
        1,
        "expected one GET for the same object version"
    );
    server.kill().await.unwrap();
}

#[tokio::test]
async fn judge_rpc_returns_compile_time_limit_with_requested_limit() {
    let (mut server, mut client, _s3) = start_server().await;
    let mut request = request(3);
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
    let (mut server, mut client, _s3) = start_server().await;
    let mut request = request(4);
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
