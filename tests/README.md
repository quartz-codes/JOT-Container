# 테스트 실행

`cargo test`는 소스의 단위 테스트와 `tests/` 디렉터리의 통합 테스트를 모두 실행한다. 이 디렉터리에는 실행 파일을 띄우고 외부에서 동작을 확인하는 통합 테스트를 둔다. 새 통합 테스트를 추가하면 아래 목록도 갱신한다.

## 실행 방법

`cargo test`를 실행하기 전에 같은 셸에서 `GRPC_ADDR`, `CLANG_PATH`, `CLANGPP_PATH`를 반드시 export해야 한다. 설정 로더는 단위 테스트에서도 모든 필수 환경변수를 검증하므로, 컴파일러 테스트만 실행해도 `GRPC_ADDR`가 필요하다. `.env` 파일은 자동으로 읽지 않는다.

```sh
export GRPC_ADDR='127.0.0.1:50051'
export CLANG_PATH='/path/to/clang'
export CLANGPP_PATH='/path/to/clang++'
export CLANG_ADDITIONAL_FLAGS='--target=wasm32-wasip1 --sysroot=/path/to/wasi-sysroot'
export CLANGPP_ADDITIONAL_FLAGS='--target=wasm32-wasip1 --sysroot=/path/to/wasi-sysroot'
cargo test
```

경로와 추가 플래그는 로컬에 설치된 WASI 컴파일러 환경에 맞게 바꾼다. 추가 플래그 변수는 설정상 선택 사항이지만, 컴파일러가 기본적으로 Wasm을 생성하지 않는다면 테스트에 필요한 값을 지정해야 한다. Docker 테스트 이미지에서는 이 환경변수들이 미리 설정된다.

## 통합 테스트 목록

| 파일 | 확인하는 동작 |
| --- | --- |
| [`health.rs`](health.rs) | 서버를 실행한 뒤 gRPC Health `Check` 요청을 보내 빈 서비스 이름에 대해 `SERVING`을 반환하는지 확인한다. |
| [`judge_protocol.rs`](judge_protocol.rs) | 실제 서버에 `Judge` 요청을 보내 요청 ID, 서브문제별 정답 여부, 최종 결과 수신 및 스트림의 정상 종료를 확인한다. |

## 채점 프로토콜 테스트

```sh
cargo test --test judge_protocol
```

위 테스트도 앞서 설명한 필수 환경변수를 설정한 셸에서 실행한다.

