use std::{env, net::SocketAddr, sync::OnceLock};

use aws_sdk_s3::config::{BehaviorVersion, Credentials, Region};

use crate::judge::testcase::s3_fetcher::S3TestcaseFetcher;

struct RustFsConfig {
    endpoint: String,
    bucket: String,
    region: String,
    access_key: String,
    secret_key: String,
}

impl RustFsConfig {
    fn load_from_env() -> Result<Self, String> {
        let required = |name| match env::var(name) {
            Ok(value) if !value.trim().is_empty() => Ok(value),
            Ok(_) => Err(format!("{name} must not be empty")),
            Err(error) => Err(format!("{name}: {error}")),
        };
        let region = match env::var("RUSTFS_REGION") {
            Ok(region) if !region.trim().is_empty() => region,
            Ok(_) => return Err("RUSTFS_REGION must not be empty".into()),
            Err(env::VarError::NotPresent) => "us-east-1".into(),
            Err(error) => return Err(format!("RUSTFS_REGION: {error}")),
        };
        Ok(Self {
            endpoint: required("RUSTFS_ENDPOINT")?,
            bucket: required("RUSTFS_BUCKET")?,
            region,
            access_key: required("RUSTFS_ACCESS_KEY")?,
            secret_key: required("RUSTFS_SECRET_KEY")?,
        })
    }

    fn into_testcase_fetcher(self) -> S3TestcaseFetcher {
        let config = aws_sdk_s3::Config::builder()
            .behavior_version(BehaviorVersion::latest())
            .endpoint_url(self.endpoint)
            .region(Region::new(self.region))
            .credentials_provider(Credentials::new(
                self.access_key,
                self.secret_key,
                None,
                None,
                "rustfs",
            ))
            .force_path_style(true)
            .build();
        S3TestcaseFetcher::new(aws_sdk_s3::Client::from_conf(config), self.bucket)
    }
}

#[derive(Debug)]
struct Config {
    grpc_address: SocketAddr,
    clang_path: String,
    clangpp_path: String,
    clang_additional_flags: Vec<String>,
    clangpp_additional_flags: Vec<String>,
}

impl Config {
    fn load_from_env() -> Result<Self, String> {
        let grpc_address = env::var("GRPC_ADDR")
            .map_err(|_| "GRPC_ADDR environment variable is not set".to_string())
            .and_then(|addr| {
                addr.parse::<SocketAddr>()
                    .map_err(|_| "GRPC_ADDR is not a valid socket address".to_string())
            })?;

        let clang_path = env::var("CLANG_PATH")
            .map_err(|_| "CLANG_PATH environment variable is not set".to_string())?;
        let clangpp_path = env::var("CLANGPP_PATH")
            .map_err(|_| "CLANGPP_PATH environment variable is not set".to_string())?;

        let clang_additional_flags = env::var("CLANG_ADDITIONAL_FLAGS")
            .unwrap_or_default()
            .split_whitespace()
            .map(|s| s.to_string())
            .collect();
        let clangpp_additional_flags = env::var("CLANGPP_ADDITIONAL_FLAGS")
            .unwrap_or_default()
            .split_whitespace()
            .map(|s| s.to_string())
            .collect();

        Ok(Config {
            grpc_address,
            clang_path,
            clangpp_path,
            clang_additional_flags,
            clangpp_additional_flags,
        })
    }
}

struct AppConfig {
    runtime: Config,
    testcase_fetcher: S3TestcaseFetcher,
}

static CONFIG: OnceLock<AppConfig> = OnceLock::new();

pub fn init_config() -> Result<(), String> {
    if CONFIG.get().is_some() {
        return Ok(());
    }
    let runtime = Config::load_from_env()?;
    let rustfs = RustFsConfig::load_from_env()?;
    CONFIG.get_or_init(|| AppConfig {
        runtime,
        testcase_fetcher: rustfs.into_testcase_fetcher(),
    });
    Ok(())
}

fn app_config() -> &'static AppConfig {
    CONFIG
        .get()
        .expect("Call init_config() before using config")
}

pub fn testcase_fetcher() -> &'static S3TestcaseFetcher {
    &app_config().testcase_fetcher
}

#[cfg(not(test))]
fn with_config<R>(f: impl FnOnce(&Config) -> R) -> R {
    f(&app_config().runtime)
}

#[cfg(test)]
fn with_config<R>(f: impl FnOnce(&Config) -> R) -> R {
    if let Some(config) = CONFIG.get() {
        return f(&config.runtime);
    }
    let loaded = Config::load_from_env().expect("Not valid environment variables for testing");
    f(&loaded)
}

pub fn grpc_address() -> SocketAddr {
    with_config(|config| config.grpc_address)
}
pub fn clang_path() -> String {
    with_config(|config| config.clang_path.clone())
}
pub fn clangpp_path() -> String {
    with_config(|config| config.clangpp_path.clone())
}
pub fn clang_additional_flags() -> Vec<String> {
    with_config(|config| config.clang_additional_flags.clone())
}
pub fn clangpp_additional_flags() -> Vec<String> {
    with_config(|config| config.clangpp_additional_flags.clone())
}
