# JOT (Judge Operation Toolkit)

JOT은 컨테이너에서 동작하는 Rust 기반 채점 서버입니다. 상위 도메인인 `judge`가 채점 요청을 조율하고, 독립적인 `compile`, `execute`, `grade` 도메인이 각각 제출 코드를 WebAssembly(Wasm)로 컴파일하고, Wasm 런타임에서 실행하고, 테스트 케이스를 기준으로 결과를 판정합니다.

## 도메인 분리

`judge`는 채점 요청의 전체 과정을 다루는 상위 도메인입니다. 요청을 받아 필요한 단계를 조율하고, 각 단계의 결과를 모아 최종 채점 결과를 반환합니다.

| 하위 도메인 | 책임 |
| --- | --- |
| `compile` | 제출 코드를 대상 언어의 컴파일러로 빌드하고, 실행 가능한 산출물 또는 컴파일 오류를 반환합니다. |
| `execute` | 실행 가능한 산출물을 주어진 입력과 자원 제한에 따라 WASM 런타임에서 실행하고, 출력과 실행 상태를 반환합니다. |
| `grade` | 실행 결과를 테스트 케이스의 기대 결과와 비교하고, 판정과 성능 평가치를 산출합니다. |

세 하위 도메인은 서로의 구현에 직접 종속되지 않도록 개발합니다. 각 도메인은 자신의 입력과 출력 계약만 정의하며, `judge`가 이 계약을 통해 결과를 다음 단계로 전달하고 전체 흐름을 조율합니다. 따라서 한 도메인의 구현을 변경하거나 교체해도 다른 하위 도메인의 구현을 수정할 필요가 없도록 합니다.

## 동작 흐름

1. Rust 프로세스가 gRPC 서버를 열고 백엔드의 채점 요청을 받습니다.
2. 컨테이너에 설치된 해당 언어의 컴파일러를 호출해 제출 코드를 Wasm 타겟으로 빌드합니다.
3. 외부 S3 호환 스토리지에서 요청에 필요한 테스트 케이스를 다운로드합니다.
4. `wasmtime-wasi`로 빌드된 코드를 각 테스트 케이스에 대해 실행합니다.
5. 실행 결과의 출력을 검증하고 채점 결과를 gRPC로 백엔드에 반환합니다.

## 진행 상황 보고

채점 요청을 처리하는 동안 컴파일, 테스트 케이스 준비, 실행, 검증 등의 진행 과정과 진행률을 gRPC 스트림으로 백엔드에 전달합니다. 백엔드는 최종 결과가 나오기 전에도 채점 상태를 확인할 수 있습니다.

## 테스트 케이스 캐시

Rust 프로세스는 자주 사용되는 테스트 케이스를 메모리에 캐시해 반복 다운로드를 줄입니다. 프로세스 종료에 대비해 캐시를 비동기적으로 디스크에 백업합니다.

## 로컬 테스트

로컬에서 `cargo test`를 실행하려면 먼저 필수 환경변수를 export해야 합니다. 설정값은 테스트에서도 자동으로 제공되지 않습니다. 필요한 변수와 실행 예시는 [테스트 안내](tests/README.md)를 참고하세요.

## WASI 및 언어 도구 체인

빌드와 테스트는 기본적으로 `debian:trixie-slim`을 사용합니다.
최종 이미지는 `gcr.io/distroless/cc-debian13:nonroot`를 기반으로 합니다.
SDK 버전은 `WASI_SDK_VERSION`(기본 `34.0`), 다운로드할 아키텍처는 `WASI_SDK_ARCH`로 지정합니다.
아키텍처를 생략하면 Docker의 `TARGETARCH`를 사용하며, `amd64`는 릴리스 파일명의 `x86_64`로 변환합니다.
WASI SDK 체크섬은 별도로 검증하지 않습니다.

```sh
docker build --build-arg WASI_SDK_VERSION=34.0 --build-arg WASI_SDK_ARCH=arm64 -t jot:trixie .
```

`rustup-stage`는 rustup과 HTTPS 다운로드에 필요한 인증서만 준비합니다.
`wasi-language-stage`는 `RUST_VERSION`(기본 `1.96.1`)의 Rust와 `wasm32-wasip1`을 설치하고,
배포에 필요한 컴파일러·WASI 표준 라이브러리만 `/opt/rust`에 남깁니다.
`project-rust-stage`는 `PROJECT_RUST`(기본 `1.99.1`)의 호스트용 Rust·Cargo와
프로젝트 빌드에 필요한 `gcc`, `libc6-dev`를 준비합니다. 도구 체인은 `/opt/project-rust`에 설치하며,
WASI 타깃은 추가하지 않습니다. 두 Rust 버전은 별도 빌드 인자로 지정할 수 있습니다.
Trixie 기본 Rust는 1.85.1로 서버의 최소 요구 버전(1.96.0)보다 낮고,
backports의 컴파일러(1.96.1)와 WASI 표준 라이브러리(1.94.1)도 버전이 달라
Rust 툴체인은 rustup으로 버전을 맞춥니다.
`wasi-language-stage`에서는 `GO_VERSION`(기본 `1.27.3`)의 공식 Go Linux 압축파일을 다운로드해 `/opt/go`에 설치합니다.
배포 파일의 아키텍처는 Docker의 `TARGETARCH`를 사용합니다.
`GOROOT=/opt/go`, `GO_PATH=/opt/go/bin/go`를 사용합니다.
`wasi-language-stage`는 WASI SDK와 Python 빌드 단계를 상속하지 않습니다.

```text
wasi-sdk-stage → build-wasi-python-template
rustup-stage
├── wasi-language-stage → RUST_VERSION의 WASI용 Rust + Go
└── project-rust-stage → PROJECT_RUST의 호스트용 Rust·Cargo + 빌드 의존성
    ├── chef → planner / build
    └── test에 프로젝트용 Rust·Cargo 복사

debian:trixie-slim → test-runtime ← SDK + WASI용 Rust·Go + Python 템플릿
                    └── test: 프로젝트용 Rust·Cargo, 빌드 도구, 소스 추가 → cargo test
cc-debian13:nonroot → final ← SDK + WASI용 Rust·Go + Python 템플릿 + libtinfo + 서버
```

```sh
docker build --target wasi-language-stage -t jot-wasi-language .
docker build --target test -t jot-test .
```

C++은 SDK 34의 예외 처리 라이브러리와 표준 Wasm 예외 처리를 사용합니다.
SDK는 채점에 사용하는 WASI P1 sysroot만 보존합니다.
`runtime-wasi-sdk-stage`는 `wasi-sdk-stage`를 상속해 분석·디버거·아카이브·바이너리 후처리 도구,
libclang C API·LLDB 공유 라이브러리와 LTO 라이브러리를 제거합니다.
최종 실행 환경은 이 SDK를 복사하며, Python 템플릿 빌드는 정리 전 `wasi-sdk-stage`를 사용합니다.
Clang·LLD가 사용하는 LLVM·libclang-cpp·libedit와 진단용 llvm-symbolizer는 유지합니다.
`wasi-language-stage`에서는 Rust 호스트 표준 라이브러리, Cargo, rustdoc, 디버거 보조 도구를 제거합니다.
Rust 라이선스·저작권 고지, 컴파일러 공유 라이브러리, `rust-lld`, WASI 표준 라이브러리와
Go 표준 라이브러리 소스는 유지합니다. 서버 빌드는 `project-rust-stage`의 도구 체인을 사용합니다.
`test-runtime`은 새 Debian 이미지에 `libc6`, `libgcc-s1`, `libstdc++6`, `libtinfo6`, `zlib1g`를 확보한 후
각 설치 단계에서 SDK, 정리된 Rust, Go, Python 템플릿을 복사합니다.
`final`은 동일한 도구 체인과 서버 실행 파일을 복사하고, `runtime-libtinfo`에서
WASI SDK에 필요한 `libtinfo.so.6`을 가져옵니다. 서버는 비루트 사용자로 실행되며
기본 `GRPC_ADDR`는 `0.0.0.0:50051`입니다.
`test`는 `test-runtime`에서 시작하며, `project-rust-stage`의 Rust·Cargo를 `/opt/project-rust`에 복사합니다.
테스트는 배포용 `/opt/rust`와 동일한 SDK·Go·Python 환경을 사용합니다.
테스트는 `cargo-chef`와 `recipe.json`을 복사해 소스 복사 전에 의존성을 빌드합니다.
Cargo 레지스트리와 `target`은 일반 Docker 레이어에 남겨 기존 GHA `mode=max` 캐시로 재사용합니다.
프로젝트 소스만 바뀌면 의존성 빌드 레이어를 재사용하고 `cargo test`를 다시 실행합니다.
APT 목록은 설치한 동일한 `RUN`에서 삭제하며,
다운로드 도구·rustup·호스트 빌드 도구의 설치 레이어는 최종 이미지에 포함하지 않습니다.
기본 빌드 대상은 `final`입니다.

SDK 다운로드 캐시와 Cargo Chef 의존성 레이어를 재사용하고,
CI/CD도 `test`·`release` 캐시를 서로 읽습니다.

## Python Wasm 템플릿 빌드

`build-wasi-python-template`은 Debian 기본 `perl-base`의 Perl로 `scripts/build_python_template.pl`을 실행합니다.
이 스크립트가 `PYTHON_VERSION`(기본 `3.14.7`) 소스 다운로드·압축 해제부터 빌드 보조용 네이티브 Python의 O1 빌드,
WASI SDK를 사용하는 WASI Preview 1용 Python의 O3 교차 빌드, 최종 템플릿 생성까지 수행합니다.
표준 라이브러리와 1MiB의 제출 코드 저장 공간을 포함하는 `/out/python-template.wasm`을 생성합니다.
템플릿 생성은 Python 인터프리터 대신 같은 소스에서 빌드한 네이티브 `Programs/_freeze_module`로
표준 라이브러리를 동결하고, Perl로 C 진입점과 Wasm 메타데이터를 생성합니다.
버전과 ABI가 반영된 정적 라이브러리 이름은 CPython Makefile에서 자동으로 읽습니다.
CPython 3.11 이상을 대상으로 하며, 네이티브·WASI 빌드는 지정한 동일 소스를 사용해야 합니다.
교차 빌드에 필요한 보조 Python은 Perl 스크립트 내부에서 빌드합니다.
기본 출력은 단계별 진행 상황으로 제한하고, 전체 컴파일 출력과 경고는 `/build/python/<버전>/logs/`에 보관합니다.
반복되는 WASI HACL 헤더의 매크로 재정의 경고는 해당 경고 옵션만 비활성화합니다.
실패 시 마지막 60줄과 전체 로그 경로를 출력하며, `--verbose`를 지정하면 각 단계가 끝날 때 전체 로그를 출력합니다.

```sh
docker build --target build-wasi-python-template -t jot-python-template .
```

SDK와 빌드 도구가 준비된 Debian 환경에서는 스크립트만으로도 전체 과정을 실행할 수 있습니다.
`--jobs`로 병렬 빌드 수를, `--log-dir`로 로그 경로를 지정할 수 있습니다.
다운로드는 `--download-dir`에 캐시하고, 소스와 빌드 디렉터리는 버전별로 분리합니다.

```sh
perl scripts/build_python_template.pl --python-version 3.14.7 \
    --work-dir /build/python --download-dir /downloads \
    --wasi-sdk /opt/wasi-sdk --output /out/python-template.wasm
```

이미 빌드한 CPython을 다시 링크할 때는 `--python-version` 대신 기존의
`--source-dir`, `--build-dir`, `--native-build-dir` 옵션을 사용할 수 있습니다.

`test-runtime`과 `final`은 템플릿을 `/opt/jot/python/python-template.wasm`으로 복사하고
`PYTHON_TEMPLATE_PATH`로 경로를 제공합니다. CPython 소스와 네이티브 빌드 도구는
최종 이미지에 복사하지 않습니다.
