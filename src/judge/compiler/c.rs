use super::Compiler;
use super::compile_executor::{CompileExecutor, local::LocalCompileExecutor};
use crate::config;
use crate::judge::compiler::{CompilerError, CompilerResourceLimits, CompilerResult};
use crate::judge::executor::wasm_executor::WasmExecutor;
use async_trait::async_trait;

pub struct CCompiler;
#[async_trait]
impl Compiler for CCompiler {
    async fn compile(
        &self,
        source: &str,
        limits: CompilerResourceLimits,
    ) -> Result<CompilerResult, CompilerError> {
        let args = ["-o", "-", "-std=c11", "-O2", "-Wall", "-x", "c", "-"];

        let mut compile_executor = LocalCompileExecutor::new(
            config::clang_path(),
            args.into_iter()
                .chain(config::clang_additional_flags().iter().map(|s| s.as_str())),
        );

        let result = compile_executor.execute(source, limits).await?;
        Ok(CompilerResult {
            executor: Box::new(WasmExecutor::new(result.output).map_err(|e| {
                CompilerError::CompilationError(format!("Failed to create WasmExecutor: {:?}", e))
            })?),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::CCompiler;
    use crate::judge::compiler::test_support::{
        ExpectedError, assert_compile_error, assert_compiles, default_limits,
    };

    const SOURCE: &str = r#"
    #include <stdio.h>
    int main() {
        int a, b;
        scanf("%d %d", &a, &b);
        printf("%d\n", a + b);
        return 0;
    }
    "#;

    #[tokio::test]
    async fn test_c_compiler() {
        assert_compiles(&mut CCompiler, SOURCE).await;
    }

    #[tokio::test]
    async fn test_c_compiler_tle() {
        let mut limits = default_limits();
        limits.time_limit_ms = 0;
        assert_compile_error(
            &mut CCompiler,
            SOURCE,
            limits,
            ExpectedError::TimeLimitExceeded(0),
        )
        .await;
    }

    #[tokio::test]
    async fn test_c_compiler_artifact_size_le() {
        let mut limits = default_limits();
        limits.build_artifact_size_limit_byte = 0;
        assert_compile_error(
            &mut CCompiler,
            SOURCE,
            limits,
            ExpectedError::BuildArtifactSizeLimitExceeded(0),
        )
        .await;
    }
}
