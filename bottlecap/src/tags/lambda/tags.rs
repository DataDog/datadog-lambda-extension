use crate::config;
use log::warn;
use std::collections::hash_map;
use std::env::consts::ARCH;
use std::fs::File;
use std::io;
use std::io::BufRead;
use std::path::Path;
use std::sync::Arc;

// Environment variables for the Lambda execution environment info
const QUALIFIER_ENV_VAR: &str = "AWS_LAMBDA_FUNCTION_VERSION";
const RUNTIME_VAR: &str = "AWS_EXECUTION_ENV";
const MEMORY_SIZE_VAR: &str = "AWS_LAMBDA_FUNCTION_MEMORY_SIZE";
const INIT_TYPE: &str = "AWS_LAMBDA_INITIALIZATION_TYPE";
const INIT_TYPE_KEY: &str = "init_type";

// FunctionARNKey is the tag key for a function's arn
pub const FUNCTION_ARN_KEY: &str = "function_arn";
// FunctionNameKey is the tag key for a function's name
const FUNCTION_NAME_KEY: &str = "functionname";
// ExecutedVersionKey is the tag key for a function's executed version
const EXECUTED_VERSION_KEY: &str = "executedversion";
// RuntimeKey is the tag key for a function's runtime (e.g node, python)
const RUNTIME_KEY: &str = "runtime";
// MemorySizeKey is the tag key for a function's allocated memory size
const MEMORY_SIZE_KEY: &str = "memorysize";
// TODO(astuyve): fetch architecture from the runtime
// ArchitectureKey is the tag key for a function's architecture (e.g. x86_64, arm64)
const ARCHITECTURE_KEY: &str = "architecture";

// EnvKey is the tag key for a function's env environment variable
const ENV_KEY: &str = "env";
// VersionKey is the tag key for a function's version environment variable
const VERSION_KEY: &str = "version";
// ServiceKey is the tag key for a function's service environment variable
const SERVICE_KEY: &str = "service";

// TODO(astuyve): origin tags when tracing is supported
// const TRACE_ORIGIN_METADATA_KEY: &str = "_dd.origin";
// const TRACE_ORIGIN_METADATA_VALUE: &str = "lambda";

// ComputeStatsKey is the tag key indicating whether trace stats should be computed
const COMPUTE_STATS_KEY: &str = "_dd.compute_stats";
// ComputeStatsValue is the tag value indicating trace stats should be computed
const COMPUTE_STATS_VALUE: &str = "1";
// TODO(astuyve) decide what to do with the version
// const EXTENSION_VERSION_KEY: &str = "dd_extension_version";

const REGION_KEY: &str = "region";
const ACCOUNT_ID_KEY: &str = "account_id";
const AWS_ACCOUNT_KEY: &str = "aws_account";
const RESOURCE_KEY: &str = "resource";

// TODO(astuyve) platform tags
// X86LambdaPlatform is for the lambda platform X86_64
// const X86_LAMBDA_PLATFORM: &str = "x86_64";
// ArmLambdaPlatform is for the lambda platform Arm64
// const ARM_LAMBDA_PLATFORM: &str = "arm64";
// AmdLambdaPlatform is for the lambda platform Amd64, which is an extension of X86_64
// const AMD_LAMBDA_PLATFORM: &str = "amd64";

#[derive(Debug, Clone)]
pub struct Lambda {
    tags_map: hash_map::HashMap<String, String>,
}

fn arch_to_platform<'a>() -> &'a str {
    match ARCH {
        "aarch64" => "arm64",
        _ => ARCH,
    }
}

fn tags_from_env(
    mut tags_map: hash_map::HashMap<String, String>,
    config: Arc<config::Config>,
    metadata: &hash_map::HashMap<String, String>,
) -> hash_map::HashMap<String, String> {
    if metadata.contains_key(FUNCTION_ARN_KEY) {
        let parts = metadata[FUNCTION_ARN_KEY].split(':').collect::<Vec<&str>>();
        if parts.len() > 6 {
            tags_map.insert(REGION_KEY.to_string(), parts[3].to_string());
            // TODO deprecate ACCOUNT_ID?
            tags_map.insert(ACCOUNT_ID_KEY.to_string(), parts[4].to_string());
            tags_map.insert(AWS_ACCOUNT_KEY.to_string(), parts[4].to_string());
            tags_map.insert(FUNCTION_NAME_KEY.to_string(), parts[6].to_string());
            tags_map.insert(RESOURCE_KEY.to_string(), parts[6].to_string());
            if let Ok(qualifier) = std::env::var(QUALIFIER_ENV_VAR) {
                if qualifier != "$LATEST" {
                    tags_map.insert(
                        RESOURCE_KEY.to_string(),
                        format!("{}:{}", parts[6], qualifier),
                    );
                    tags_map.insert(EXECUTED_VERSION_KEY.to_string(), qualifier);
                }
            }
        }
        tags_map.insert(
            FUNCTION_ARN_KEY.to_string(),
            metadata[FUNCTION_ARN_KEY].clone().to_lowercase(),
        );
    }
    if let Some(version) = &config.version {
        tags_map.insert(VERSION_KEY.to_string(), version.to_string());
    }
    if let Some(env) = &config.env {
        tags_map.insert(ENV_KEY.to_string(), env.to_string());
    }
    if let Some(service) = &config.service {
        tags_map.insert(SERVICE_KEY.to_string(), service.to_string());
    }
    if let Ok(init_type) = std::env::var(INIT_TYPE) {
        tags_map.insert(INIT_TYPE_KEY.to_string(), init_type);
    }
    if let Ok(memory_size) = std::env::var(MEMORY_SIZE_VAR) {
        tags_map.insert(MEMORY_SIZE_KEY.to_string(), memory_size);
    }
    if let Ok(exec_runtime_env) = std::env::var(RUNTIME_VAR) {
        // AWS_Lambda_java8
        let runtime = exec_runtime_env.split('_').last().unwrap_or("unknown");
        tags_map.insert(RUNTIME_KEY.to_string(), runtime.to_string());
    } else {
        tags_map.insert(
            RUNTIME_KEY.to_string(),
            resolve_provided_runtime("/etc/os-release"),
        );
    }

    tags_map.insert(ARCHITECTURE_KEY.to_string(), arch_to_platform().to_string());

    if let Some(tags) = &config.tags {
        for tag in tags.split(',') {
            let parts = tag.split(':').collect::<Vec<&str>>();
            if parts.len() == 2 {
                tags_map.insert(parts[0].to_string(), parts[1].to_string());
            }
        }
    }

    tags_map.insert(
        COMPUTE_STATS_KEY.to_string(),
        COMPUTE_STATS_VALUE.to_string(),
    );
    tags_map
}

fn resolve_provided_runtime(path: &str) -> String {
    let path = Path::new(path);

    let file = match File::open(path) {
        Err(why) => {
            warn!(
                "Couldn't read provided runtime. Cannot read: {}. Returning unknown",
                why
            );
            return "unknown".to_string();
        }
        Ok(file) => file,
    };
    let reader = io::BufReader::new(file);
    for line in reader.lines().map_while(Result::ok) {
        if line.starts_with("PRETTY_NAME=") {
            let parts: Vec<&str> = line.split('=').collect();
            if parts.len() > 1 {
                match parts[1]
                    .split(' ')
                    .last()
                    .unwrap_or("")
                    .replace('\"', "")
                    .as_str()
                {
                    "2" => return "provided.al2".to_string(),
                    s if s.starts_with("2023") => return "provided.al2023".to_string(),
                    _ => break,
                }
            }
        }
    }
    "unknown".to_string()
}

impl Lambda {
    #[must_use]
    pub fn new_from_config(
        config: Arc<config::Config>,
        metadata: &hash_map::HashMap<String, String>,
    ) -> Self {
        Lambda {
            tags_map: tags_from_env(hash_map::HashMap::new(), config, metadata),
        }
    }

    #[must_use]
    pub fn get_tags_vec(&self) -> Vec<String> {
        self.tags_map
            .iter()
            .map(|(k, v)| format!("{k}:{v}"))
            .collect()
    }

    #[must_use]
    pub fn get_function_arn(&self) -> Option<&String> {
        self.tags_map.get(FUNCTION_ARN_KEY)
    }

    #[must_use]
    pub fn get_tags_map(&self) -> &hash_map::HashMap<String, String> {
        &self.tags_map
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::config::Config;
    use serial_test::serial;
    use std::io::Write;

    #[test]
    // #[serial]
    fn test_new_from_config() {
        let metadata = hash_map::HashMap::new();
        let tags = Lambda::new_from_config(Arc::new(config::Config::default()), &metadata);
        assert_eq!(tags.tags_map.len(), 3);
        assert_eq!(
            tags.tags_map.get(COMPUTE_STATS_KEY).unwrap(),
            COMPUTE_STATS_VALUE
        );
        let arch = arch_to_platform();
        assert_eq!(
            tags.tags_map.get(ARCHITECTURE_KEY).unwrap(),
            &arch.to_string()
        );
        assert_eq!(tags.tags_map.get(RUNTIME_KEY).unwrap(), "unknown");
    }

    #[test]
    fn test_new_with_function_arn_metadata() {
        let mut metadata = hash_map::HashMap::new();
        metadata.insert(
            FUNCTION_ARN_KEY.to_string(),
            "arn:aws:lambda:us-west-2:123456789012:function:my-function".to_string(),
        );
        let tags = Lambda::new_from_config(Arc::new(config::Config::default()), &metadata);
        assert_eq!(tags.tags_map.get(REGION_KEY).unwrap(), "us-west-2");
        assert_eq!(tags.tags_map.get(ACCOUNT_ID_KEY).unwrap(), "123456789012");
        assert_eq!(tags.tags_map.get(AWS_ACCOUNT_KEY).unwrap(), "123456789012");
        assert_eq!(tags.tags_map.get(FUNCTION_NAME_KEY).unwrap(), "my-function");
        assert_eq!(tags.tags_map.get(RESOURCE_KEY).unwrap(), "my-function");
        assert_eq!(
            tags.tags_map.get(FUNCTION_ARN_KEY).unwrap(),
            "arn:aws:lambda:us-west-2:123456789012:function:my-function"
        );
    }

    #[test]
    #[serial] //run test serially since it sets and unsets env vars
    fn test_with_lambda_env_vars() {
        let mut metadata = hash_map::HashMap::new();
        metadata.insert(
            FUNCTION_ARN_KEY.to_string(),
            "arn:aws:lambda:us-west-2:123456789012:function:My-function".to_string(),
        );
        let config = Arc::new(Config {
            service: Some("my-service".to_string()),
            tags: Some("test:tag,env:test".to_string()),
            env: Some("test".to_string()),
            version: Some("1.0.0".to_string()),
            ..Config::default()
        });
        std::env::set_var(MEMORY_SIZE_VAR, "128");
        std::env::set_var(RUNTIME_VAR, "AWS_Lambda_java8");
        let tags = Lambda::new_from_config(config, &metadata);
        std::env::remove_var(MEMORY_SIZE_VAR);
        std::env::remove_var(RUNTIME_VAR);
        assert_eq!(tags.tags_map.get(ENV_KEY).unwrap(), "test");
        assert_eq!(tags.tags_map.get(VERSION_KEY).unwrap(), "1.0.0");
        assert_eq!(tags.tags_map.get(SERVICE_KEY).unwrap(), "my-service");
        assert_eq!(tags.tags_map.get(MEMORY_SIZE_KEY).unwrap(), "128");
        assert_eq!(
            tags.tags_map.get(FUNCTION_ARN_KEY).unwrap(),
            "arn:aws:lambda:us-west-2:123456789012:function:my-function"
        );
    }

    #[test]
    fn test_resolve_provided_al2() {
        let path = "/tmp/test-os-release1";
        let content = "NAME =\"Amazon Linux\"\nVERSION=\"2\nPRETTY_NAME=\"Amazon Linux 2\"";
        let mut file = File::create(path).unwrap();
        file.write_all(content.as_bytes()).unwrap();

        let runtime = resolve_provided_runtime(path);
        std::fs::remove_file(path).unwrap();
        assert_eq!(runtime, "provided.al2");
    }

    #[test]
    fn test_resolve_provided_al2023() {
        let path = "/tmp/test-os-release2";
        let content =
            "NAME=\"Amazon Linux\"\nVERSION=\"2\nPRETTY_NAME=\"Amazon Linux 2023.4.20240429\"";
        let mut file = File::create(path).unwrap();
        file.write_all(content.as_bytes()).unwrap();

        let runtime = resolve_provided_runtime(path);
        std::fs::remove_file(path).unwrap();
        assert_eq!(runtime, "provided.al2023");
    }
}
