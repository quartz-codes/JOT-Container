use super::{ExecuteError, ExecuteResult, Executor};
use async_trait::async_trait;
use lazy_static::lazy_static;
use std::sync::Arc;
use wasmtime::{Config, Engine, Linker, Module, ResourceLimiter, Store, Trap};
use wasmtime_wasi::{HostMonotonicClock, HostWallClock, I32Exit};
use wasmtime_wasi::{
    WasiCtx, WasiCtxView, WasiView,
    p1::WasiP1Ctx,
    p2::pipe::{MemoryInputPipe, MemoryOutputPipe},
};

lazy_static! {
    static ref WASMTIME_ENGINE: Engine = Engine::new(
        Config::new()
            .max_wasm_stack(1048576)
            .wasm_threads(false)
            .consume_fuel(true)
            .wasm_exceptions(true)
    )
    .expect("Failed to initialize Wasmtime engine");
}

// this clock always returns same value to prevent non-deterministic codes depends on system clock
// unfortunately this custom clock doesn't prevent sleep()
// but it can prevent something like non-deterministic hashing based on clock
struct DeniedClock;
impl HostWallClock for DeniedClock {
    fn now(&self) -> std::time::Duration {
        std::time::Duration::from_secs(1)
    }
    fn resolution(&self) -> std::time::Duration {
        std::time::Duration::from_nanos(1)
    }
}
struct DeniedMonotonicClock;
impl HostMonotonicClock for DeniedMonotonicClock {
    fn now(&self) -> u64 {
        1
    }
    fn resolution(&self) -> u64 {
        1
    }
}

pub struct WasmExecutor {
    engine: Engine,
    wasm: Vec<u8>,
}

impl WasmExecutor {
    pub fn new(wasm: Vec<u8>) -> Self {
        Self {
            engine: WASMTIME_ENGINE.clone(),
            wasm,
        }
    }
}

struct MemoryCheckState {
    ctx: WasiP1Ctx,
    limit: MemoryLimiter,
}
impl WasiView for MemoryCheckState {
    fn ctx(&mut self) -> WasiCtxView<'_> {
        self.ctx.ctx()
    }
}

struct MemoryLimiter {
    memory_limit: usize,
    memory_limit_exceeded: bool,
    memory_used: usize,
    table_used: usize,
}
impl MemoryLimiter {
    fn new(memory_limit: usize) -> Self {
        Self {
            memory_limit,
            memory_limit_exceeded: false,
            memory_used: 0,
            table_used: 0,
        }
    }
}
impl ResourceLimiter for MemoryLimiter {
    fn memory_growing(
        &mut self,
        _current: usize,
        desired: usize,
        _maximum: Option<usize>,
    ) -> wasmtime::Result<bool> {
        // self.memory_used = current;
        if self.table_used + desired > self.memory_limit {
            self.memory_limit_exceeded = true;
            return Err(wasmtime::Error::msg("Memory limit exceeded"));
        }
        self.memory_used = desired;
        Ok(true)
    }

    fn table_growing(
        &mut self,
        _current: usize,
        desired: usize,
        _maximum: Option<usize>,
    ) -> wasmtime::Result<bool> {
        // self.table_used = current * 4;
        let desired_table_used = desired * 4;
        if self.memory_used + desired_table_used > self.memory_limit {
            self.memory_limit_exceeded = true;
            return Err(wasmtime::Error::msg("Memory limit exceeded"));
        }
        self.table_used = desired_table_used;
        Ok(true)
    }

    fn instances(&self) -> usize {
        1
    }

    fn memories(&self) -> usize {
        1
    }
}

#[async_trait]
impl Executor for WasmExecutor {
    async fn execute(
        &self,
        input: &str,
        memory_limit: u64,
        time_limit: u64,
    ) -> Result<ExecuteResult, ExecuteError> {
        let mut linker = Linker::<MemoryCheckState>::new(&self.engine);
        if let Err(_) = wasmtime_wasi::p1::add_to_linker_sync(&mut linker, |s| &mut s.ctx) {
            return Err(ExecuteError::RuntimeError {
                reason: Some("add_to_linker_sync() failed".into()),
            });
        }

        // shadow implementation of `poll_oneoff` to preventing sleeping
        // it immediately traps the instance if it tries to call poll_oneoff
        // it can prevent something like sleep() on <unistd.h>
        // but it may have side effects about input/output
        // currently our input stream immediately returns some value or EOF, so this works fine
        linker.allow_shadowing(true);

        linker
            .func_wrap(
                "wasi_snapshot_preview1",
                "poll_oneoff",
                |_in_ptr: i32,
                 _out_ptr: i32,
                 _nsubscriptions: i32,
                 _nevents_ptr: i32|
                 -> wasmtime::Result<i32> {
                    Err(wasmtime::Error::msg("poll_oneoff is disabled"))
                },
            )
            .map_err(|_| ExecuteError::RuntimeError {
                reason: Some("linker.func_wrap() failed".into()),
            })?;

        let input = String::from(input);

        let memory_input = MemoryInputPipe::new(input);
        let memory_output = Arc::new(MemoryOutputPipe::new(1048576));
        let memory_error_output = Arc::new(MemoryOutputPipe::new(1048576));

        let wasi = WasiCtx::builder()
            //.inherit_stdout()
            .stdout(memory_output.clone())
            .stderr(memory_error_output.clone())
            .stdin(memory_input)
            .wall_clock(DeniedClock)
            .monotonic_clock(DeniedMonotonicClock)
            .arg("StringExecutor")
            .build_p1();

        let mut store = Store::new(
            &self.engine,
            MemoryCheckState {
                ctx: wasi,
                limit: MemoryLimiter::new(memory_limit as usize),
            },
        );
        store.limiter(|store| &mut store.limit);
        let initial_fuel = time_limit;

        store
            .set_fuel(initial_fuel)
            .map_err(|_| ExecuteError::RuntimeError {
                reason: Some("set_fuel() failed".into()),
            })?;

        let module =
            Module::new(&self.engine, &self.wasm).map_err(|_| ExecuteError::RuntimeError {
                reason: Some("Failed to load wasm binary".into()),
            })?;
        linker
            .module(&mut store, "", &module)
            .map_err(|_| ExecuteError::RuntimeError {
                reason: Some("linker.module() failed".into()),
            })?;

        let instance = match linker.instantiate(&mut store, &module) {
            Ok(instance) => instance,
            Err(_) if store.data().limit.memory_limit_exceeded => {
                return Err(ExecuteError::MemoryLimitExceeded(memory_limit));
            }
            Err(e) => {
                return Err(ExecuteError::RuntimeError {
                    reason: Some(format!("linker.instantiate() failed: {}", e)),
                });
            }
        };

        let default =
            instance
                .get_func(&mut store, "_start")
                .ok_or(ExecuteError::RuntimeError {
                    reason: Some("instance.get_func() failed".into()),
                })?;
        let default = default
            .typed::<(), ()>(&store)
            .map_err(|_| ExecuteError::RuntimeError {
                reason: Some("default.typed() failed".into()),
            })?;

        let (result, store) =
            tokio::task::spawn_blocking(move || (default.call(&mut store, ()), store))
                .await
                .map_err(|_| ExecuteError::RuntimeError {
                    reason: Some("tokio::task::spawn_blocking() failed".into()),
                })?;

        // WASM runtime returns exit code 0 as successful execution, but it is returned as an error in wasmtime.
        // So we need to check if the error is I32Exit(0) and treat it as a successful execution.
        // See: https://github.com/judge-master/JOT-Container/pull/22#issuecomment-6572822702
        let result = match result {
            Err(e) if matches!(e.downcast_ref::<I32Exit>(), Some(I32Exit(0))) => Ok(()),
            other => other,
        };

        match result {
            Ok(()) => Ok(ExecuteResult {
                output: String::from_utf8_lossy(memory_output.contents().iter().as_slice()).into(),
                memory_used: (store.data().limit.memory_used + store.data().limit.table_used)
                    as u64,
                instruction_count: initial_fuel - store.get_fuel().unwrap(),
            }),
            Err(_) if store.data().limit.memory_limit_exceeded => {
                return Err(ExecuteError::MemoryLimitExceeded(memory_limit));
            }
            Err(e) => {
                if e.root_cause().downcast_ref::<Trap>() == Some(&Trap::OutOfFuel) {
                    Err(ExecuteError::TimeLimitExceeded(time_limit))
                } else {
                    Err(ExecuteError::RuntimeError {
                        reason: Some(e.to_string()),
                    })
                }
            }
        }
    }
}

#[tokio::test]
async fn test_wasm() {
    let executor = WasmExecutor::new(
        include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/fixtures/aplusb.wasm")).to_vec(),
    );
    let res = executor
        .execute("1 2", 1048576, 100000)
        .await
        .expect("Runtime error");
    assert_eq!(res.output.trim(), "3");
}
#[tokio::test]
async fn test_wasm_tle() {
    let executor = WasmExecutor::new(
        include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/fixtures/aplusb.wasm")).to_vec(),
    );
    let result = executor.execute("1 2", 1048576, 10).await;
    assert!(matches!(result, Err(ExecuteError::TimeLimitExceeded(_))));
}
#[tokio::test]
async fn test_wasm_mle() {
    let executor = WasmExecutor::new(
        include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/fixtures/aplusb.wasm")).to_vec(),
    );
    let result = executor.execute("1 2", 1000, 100000).await;
    assert!(matches!(result, Err(ExecuteError::MemoryLimitExceeded(_))));
}

// poll_oneoff 를 disable 했기 때문에 이 fixture는 런타임 에러가 나야 함
#[tokio::test]
#[should_panic = "Runtime error"]
async fn test_wasm_sleep() {
    let executor = WasmExecutor::new(
        include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/fixtures/sleep.wasm")).to_vec(),
    );
    executor
        .execute("", 100000000, 100000000)
        .await
        .expect("Runtime error");
}

#[cfg(test)]
fn generate_long_input() -> String {
    let n = 100_000;
    let mut input = format!("{}\n", n);
    for i in 0..n {
        input.push_str(&format!("{} ", i));
    }
    input
}

#[tokio::test]
async fn test_long_input_scanf() {
    let executor = WasmExecutor::new(
        include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/fixtures/long_input_scanf_strip.wasm"
        ))
        .to_vec(),
    );
    let input = generate_long_input();
    let result = executor
        .execute(&input, 100000000, 1000000000)
        .await
        .expect("Runtime error");

    assert_eq!(result.output.trim(), input.trim());
}

#[tokio::test]
async fn test_long_input_cin() {
    let executor = WasmExecutor::new(
        include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/fixtures/long_input_cin_strip.wasm"
        ))
        .to_vec(),
    );
    let input = generate_long_input();
    let result = executor
        .execute(&input, 100000000, 1000000000)
        .await
        .expect("Runtime error");

    assert_eq!(result.output.trim(), input.trim());
}
