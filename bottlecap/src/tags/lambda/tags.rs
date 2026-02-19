use crate::config;
use std::collections::HashMap;
use std::env::consts::ARCH;
use std::fs;
use std::sync::Arc;
use std::time::Instant;
use tracing::{debug, warn};

// Environment variables for the Lambda execution environment info
const QUALIFIER_ENV_VAR: &str = "AWS_LAMBDA_FUNCTION_VERSION";
const RUNTIME_VAR: &str = "AWS_EXECUTION_ENV";
const MEMORY_SIZE_VAR: &str = "AWS_LAMBDA_FUNCTION_MEMORY_SIZE";
pub const INIT_TYPE: &str = "AWS_LAMBDA_INITIALIZATION_TYPE";
const INIT_TYPE_KEY: &str = "init_type";
// Value for INIT_TYPE when the function is using SnapStart
pub const SNAP_START_VALUE: &str = "snap-start";

// FunctionARNKey is the tag key for a function's arn
pub const FUNCTION_ARN_KEY: &str = "function_arn";
// FunctionNameKey is the tag key for a function's name
const FUNCTION_NAME_KEY: &str = "functionname";
// ExecutedVersionKey is the tag key for a function's executed version
const EXECUTED_VERSION_KEY: &str = "executedversion";
// RuntimeKey is the tag key for a function's runtime (e.g. node, python)
const RUNTIME_KEY: &str = "runtime";
// The identified runtime family, set only if serverless AAP is enabled.
const RUNTIME_FAMILY_KEY: &str = "_dd.runtime_family";
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

// ComputeStatsKey is the tag key indicating whether trace stats should be computed
const COMPUTE_STATS_KEY: &str = "_dd.compute_stats";
// FunctionTagsKey is the tag key for a function's tags to be set on the top level tracepayload
const FUNCTION_TAGS_KEY: &str = "_dd.tags.function";
// TODO(astuyve) decide what to do with the version
const EXTENSION_VERSION_KEY: &str = "dd_extension_version";
// TODO(duncanista) figure out a better way to not hardcode this
pub const EXTENSION_VERSION: &str = "92-next";

const REGION_KEY: &str = "region";
const ACCOUNT_ID_KEY: &str = "account_id";
const AWS_ACCOUNT_KEY: &str = "aws_account";
const RESOURCE_KEY: &str = "resource";

#[derive(Debug, Clone)]
pub struct Lambda {
    tags_map: HashMap<String, String>,
}

fn arch_to_platform<'a>() -> &'a str {
    match ARCH {
        "aarch64" => "arm64",
        _ => ARCH,
    }
}

fn tags_from_env(
    mut tags_map: HashMap<String, String>,
    config: Arc<config::Config>,
    metadata: &HashMap<String, String>,
) -> HashMap<String, String> {
    if metadata.contains_key(FUNCTION_ARN_KEY) {
        let parts = metadata[FUNCTION_ARN_KEY].split(':').collect::<Vec<&str>>();
        if parts.len() > 6 {
            tags_map.insert(REGION_KEY.to_string(), parts[3].to_string());
            // TODO deprecate ACCOUNT_ID?
            tags_map.insert(ACCOUNT_ID_KEY.to_string(), parts[4].to_string());
            tags_map.insert(AWS_ACCOUNT_KEY.to_string(), parts[4].to_string());
            tags_map.insert(FUNCTION_NAME_KEY.to_string(), parts[6].to_string());
            tags_map.insert(RESOURCE_KEY.to_string(), parts[6].to_string());
            if let Ok(qualifier) = std::env::var(QUALIFIER_ENV_VAR)
                && qualifier != "$LATEST"
            {
                tags_map.insert(
                    RESOURCE_KEY.to_string(),
                    format!("{}:{}", parts[6], qualifier),
                );
                tags_map.insert(EXECUTED_VERSION_KEY.to_string(), qualifier);
            }
        }
        tags_map.insert(
            FUNCTION_ARN_KEY.to_string(),
            metadata[FUNCTION_ARN_KEY].clone().to_lowercase(),
        );
    }
    if let Some(version) = &config.version {
        tags_map.insert(VERSION_KEY.to_string(), version.clone());
    }
    if let Some(env) = &config.env {
        tags_map.insert(ENV_KEY.to_string(), env.clone());
    }
    if let Some(service) = &config.service {
        tags_map.insert(SERVICE_KEY.to_string(), service.to_lowercase());
    }
    if let Ok(init_type) = std::env::var(INIT_TYPE) {
        tags_map.insert(INIT_TYPE_KEY.to_string(), init_type);
    }
    if let Ok(memory_size) = std::env::var(MEMORY_SIZE_VAR) {
        tags_map.insert(MEMORY_SIZE_KEY.to_string(), memory_size);
    }
    if let Ok(runtime) = std::env::var(RUNTIME_VAR) {
        if config.serverless_appsec_enabled
            && let Some(runtime_family) = identify_runtime_family(&runtime)
        {
            tags_map.insert(RUNTIME_FAMILY_KEY.to_string(), runtime_family.to_string());
        }
        tags_map.insert(RUNTIME_KEY.to_string(), runtime);
    }

    tags_map.insert(ARCHITECTURE_KEY.to_string(), arch_to_platform().to_string());
    tags_map.insert(
        EXTENSION_VERSION_KEY.to_string(),
        EXTENSION_VERSION.to_string(),
    );

    if !config.tags.is_empty() {
        tags_map.extend(config.tags.clone());
    }

    // The value of _dd.compute_stats is the opposite of config.compute_trace_stats_on_extension.
    // "config.compute_trace_stats_on_extension == true" means computing stats on the extension side,
    // so we set _dd.compute_stats to 0 so stats won't be computed on the backend side.
    let compute_stats = i32::from(!config.compute_trace_stats_on_extension);
    tags_map.insert(COMPUTE_STATS_KEY.to_string(), compute_stats.to_string());

    tags_map
}

pub fn resolve_runtime_from_proc(proc_path: &str, fallback_provided_al_path: &str) -> String {
    let start = Instant::now();
    match fs::read_dir(proc_path) {
        Ok(proc_dir) => {
            let search_environ_runtime = proc_dir
                .filter_map(Result::ok)
                .filter(|entry| {
                    entry.path().is_dir()
                        && entry
                            .file_name()
                            .into_string()
                            .ok()
                            .is_some_and(|pid_folder| pid_folder.chars().all(char::is_numeric))
                })
                .filter(|pid_folder| pid_folder.file_name().ne("1"))
                .filter_map(|pid_folder| fs::read(pid_folder.path().join("environ")).ok())
                .find(|environ_bytes| {
                    String::from_utf8(environ_bytes.clone())
                        .map(|s| s.contains(RUNTIME_VAR))
                        .unwrap_or(false)
                })
                .and_then(|runtime_byte_strings| {
                    runtime_byte_strings
                        .split(|byte| *byte == b'\0')
                        .filter_map(|s| String::from_utf8(s.to_vec()).ok())
                        .find(|line| line.contains(RUNTIME_VAR))
                        .and_then(|runtime_var_line| {
                            // AWS_EXECUTION_ENV=AWS_Lambda_java8
                            runtime_var_line.split('_').next_back().map(String::from)
                        })
                });

            let search_time = start.elapsed().as_micros().to_string();
            if let Some(runtime_from_environ) = search_environ_runtime {
                debug!("Proc runtime search successful in {search_time}us: {runtime_from_environ}");
                return runtime_from_environ.replace('\"', "");
            }
            debug!("Proc runtime search unsuccessful after {search_time}us");
        }
        Err(e) => {
            debug!("Could not resolve runtime {e}");
        }
    }

    debug!("Checking '{fallback_provided_al_path}' for provided_al");
    let start = Instant::now();

    let provided_al = fs::read_to_string(fallback_provided_al_path)
        .ok()
        .and_then(|fallback_provided_al_content| {
            fallback_provided_al_content
                .lines()
                .find(|line| line.starts_with("PRETTY_NAME="))
                .and_then(
                    |pretty_name_line| match pretty_name_line.replace('\"', "").as_str() {
                        "PRETTY_NAME=Amazon Linux 2" => Some("provided.al2".to_string()),
                        s if s.starts_with("PRETTY_NAME=Amazon Linux 2023") => {
                            Some("provided.al2023".to_string())
                        }
                        _ => None,
                    },
                )
        })
        .unwrap_or_else(|| {
            warn!("Failed to read os-release file or extract runtime");
            "unknown".to_string()
        });

    debug!(
        "Provided runtime {provided_al}, it took: {:?}",
        start.elapsed()
    );
    provided_al
}

impl Lambda {
    #[must_use]
    pub fn new_from_config(
        config: Arc<config::Config>,
        metadata: &HashMap<String, String>,
    ) -> Self {
        Lambda {
            tags_map: tags_from_env(HashMap::new(), config, metadata),
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
    pub fn get_function_name(&self) -> Option<&String> {
        self.tags_map.get(FUNCTION_NAME_KEY)
    }

    #[must_use]
    pub fn get_tags_map(&self) -> &HashMap<String, String> {
        &self.tags_map
    }

    #[must_use]
    pub fn get_function_tags_map(&self) -> HashMap<String, String> {
        let tags = self
            .tags_map
            .iter()
            .map(|(k, v)| format!("{k}:{v}"))
            .collect::<Vec<String>>()
            .join(",");
        HashMap::from_iter([(FUNCTION_TAGS_KEY.to_string(), tags)])
    }
}

/// Identify the runtime family for a given runtime identifier (as provided by the [`RUNTIME_VAR`] environment
/// variable). This is used by Serverless AAP to categorize the technology used by the client application, and is one of
/// the following values: `nodejs`, `ruby`, `jvm`, `dotnet`, `go`, `php`, or `python`. Note that some of these we cannot
/// identify here (i.e, `go` as it uses the `provided` runtime).
fn identify_runtime_family(runtime: &str) -> Option<&'static str> {
    if runtime.starts_with("nodejs") {
        Some("nodejs")
    } else if runtime.starts_with("python") {
        Some("python")
    } else if runtime.starts_with("java") {
        Some("jvm") // And not "java", intentionally.
    } else if runtime.starts_with("dotnet") {
        Some("dotnet")
    } else if runtime.starts_with("ruby") {
        Some("ruby")
    } else {
        None
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::config::Config;
    use serial_test::serial;
    use std::collections::HashMap;
    use std::fs::File;
    use std::io::Write;
    use std::path::Path;

    #[test]
    fn test_new_from_config() {
        let metadata = HashMap::new();
        let tags = Lambda::new_from_config(Arc::new(Config::default()), &metadata);
        assert_eq!(tags.tags_map.len(), 3);
        assert_eq!(tags.tags_map.get(COMPUTE_STATS_KEY).unwrap(), "1");
        let arch = arch_to_platform();
        assert_eq!(
            tags.tags_map.get(ARCHITECTURE_KEY).unwrap(),
            &arch.to_string()
        );

        assert_eq!(
            tags.tags_map.get(EXTENSION_VERSION_KEY).unwrap(),
            EXTENSION_VERSION
        );
    }

    #[test]
    fn test_new_with_function_arn_metadata() {
        let mut metadata = HashMap::new();
        metadata.insert(
            FUNCTION_ARN_KEY.to_string(),
            "arn:aws:lambda:us-west-2:123456789012:function:my-function".to_string(),
        );
        let tags = Lambda::new_from_config(Arc::new(Config::default()), &metadata);
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
        let mut metadata = HashMap::new();
        metadata.insert(
            FUNCTION_ARN_KEY.to_string(),
            "arn:aws:lambda:us-west-2:123456789012:function:My-function".to_string(),
        );
        let config = Arc::new(Config {
            service: Some("my-service".to_string()),
            tags: HashMap::from([
                ("test".to_string(), "tag".to_string()),
                ("env".to_string(), "test".to_string()),
            ]),
            env: Some("test".to_string()),
            version: Some("1.0.0".to_string()),
            ..Config::default()
        });
        unsafe { std::env::set_var(MEMORY_SIZE_VAR, "128") };
        unsafe { std::env::set_var(RUNTIME_VAR, "AWS_Lambda_java8") };
        let tags = Lambda::new_from_config(config, &metadata);
        unsafe { std::env::remove_var(MEMORY_SIZE_VAR) };
        unsafe { std::env::remove_var(RUNTIME_VAR) };
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
    fn test_resolve_runtime() {
        let proc_id_folder = Path::new("/tmp/test-bottlecap/proc_root/123");
        fs::create_dir_all(proc_id_folder).unwrap();
        let path = proc_id_folder.join("environ");
        let content = "\0NAME =\"AmazonLinux\"\0V=\"2\0AWS_EXECUTION_ENV=\"AWS_Lambda_java123\"\0somethingelse=\"abd\0\"";

        let mut file = File::create(&path).unwrap();
        file.write_all(content.as_bytes()).unwrap();

        let runtime =
            resolve_runtime_from_proc(proc_id_folder.parent().unwrap().to_str().unwrap(), "");
        fs::remove_file(path).unwrap();
        assert_eq!(runtime, "java123");
    }

    #[test]
    fn test_resolve_provided_al2() {
        let path = "/tmp/test-os-release1";
        let content = "NAME =\"Amazon Linux\"\nVERSION=\"2\nPRETTY_NAME=\"Amazon Linux 2\"";
        let mut file = File::create(path).unwrap();
        file.write_all(content.as_bytes()).unwrap();

        let runtime = resolve_runtime_from_proc("", path);
        fs::remove_file(path).unwrap();
        assert_eq!(runtime, "provided.al2");
    }

    #[test]
    fn test_resolve_provided_al2023() {
        let path = "/tmp/test-os-release2";
        let content =
            "NAME=\"Amazon Linux\"\nVERSION=\"2\nPRETTY_NAME=\"Amazon Linux 2023.4.20240429\"";
        let mut file = File::create(path).unwrap();
        file.write_all(content.as_bytes()).unwrap();

        let runtime = resolve_runtime_from_proc("", path);
        fs::remove_file(path).unwrap();
        assert_eq!(runtime, "provided.al2023");
    }

    #[test]
    fn test_get_function_tags_map() {
        let mut metadata = HashMap::new();
        metadata.insert(
            FUNCTION_ARN_KEY.to_string(),
            "arn:aws:lambda:us-west-2:123456789012:function:my-function".to_string(),
        );
        let config = Arc::new(Config {
            service: Some("my-service".to_string()),
            tags: HashMap::from([
                ("key1".to_string(), "value1".to_string()),
                ("key2".to_string(), "value2".to_string()),
            ]),
            env: Some("test".to_string()),
            version: Some("1.0.0".to_string()),
            ..Config::default()
        });
        let tags = Lambda::new_from_config(config, &metadata);
        let function_tags = tags.get_function_tags_map();
        assert_eq!(function_tags.len(), 1);
        let fn_tags_map: HashMap<String, String> = function_tags
            .get(FUNCTION_TAGS_KEY)
            .unwrap()
            .split(',')
            .map(|tag| {
                let parts = tag.split(':').collect::<Vec<&str>>();
                (parts[0].to_string(), parts[1].to_string())
            })
            .collect();
        assert_eq!(fn_tags_map.len(), 14);
        assert_eq!(fn_tags_map.get("key1").unwrap(), "value1");
        assert_eq!(fn_tags_map.get("key2").unwrap(), "value2");
        assert_eq!(fn_tags_map.get(ACCOUNT_ID_KEY).unwrap(), "123456789012");
        assert_eq!(fn_tags_map.get(ENV_KEY).unwrap(), "test");
        assert_eq!(fn_tags_map.get(FUNCTION_ARN_KEY).unwrap(), "arn");
        assert_eq!(fn_tags_map.get(FUNCTION_NAME_KEY).unwrap(), "my-function");
        assert_eq!(fn_tags_map.get(REGION_KEY).unwrap(), "us-west-2");
        assert_eq!(fn_tags_map.get(SERVICE_KEY).unwrap(), "my-service");
        assert_eq!(fn_tags_map.get(VERSION_KEY).unwrap(), "1.0.0");
    }

    #[test]
    fn test_get_function_tags_map_with_appsec_enabled() {
        let mut metadata = HashMap::new();
        metadata.insert(
            FUNCTION_ARN_KEY.to_string(),
            "arn:aws:lambda:us-west-2:123456789012:function:my-function".to_string(),
        );
        let config = Arc::new(Config {
            service: Some("my-service".to_string()),
            tags: HashMap::from([
                ("key1".to_string(), "value1".to_string()),
                ("key2".to_string(), "value2".to_string()),
            ]),
            env: Some("test".to_string()),
            version: Some("1.0.0".to_string()),
            serverless_appsec_enabled: true,
            ..Config::default()
        });
        let tags = Lambda::new_from_config(config, &metadata);
        let function_tags = tags.get_function_tags_map();
        assert_eq!(function_tags.len(), 1);
        let fn_tags_map: HashMap<String, String> = function_tags
            .get(FUNCTION_TAGS_KEY)
            .unwrap()
            .split(',')
            .map(|tag| {
                let parts = tag.split(':').collect::<Vec<&str>>();
                (parts[0].to_string(), parts[1].to_string())
            })
            .collect();
        assert_eq!(fn_tags_map.len(), 14);
        assert_eq!(fn_tags_map.get("key1").unwrap(), "value1");
        assert_eq!(fn_tags_map.get("key2").unwrap(), "value2");
        assert_eq!(fn_tags_map.get(ACCOUNT_ID_KEY).unwrap(), "123456789012");
        assert_eq!(fn_tags_map.get(ENV_KEY).unwrap(), "test");
        assert_eq!(fn_tags_map.get(FUNCTION_ARN_KEY).unwrap(), "arn");
        assert_eq!(fn_tags_map.get(FUNCTION_NAME_KEY).unwrap(), "my-function");
        assert_eq!(fn_tags_map.get(REGION_KEY).unwrap(), "us-west-2");
        assert_eq!(fn_tags_map.get(SERVICE_KEY).unwrap(), "my-service");
        assert_eq!(fn_tags_map.get(VERSION_KEY).unwrap(), "1.0.0");
    }
}
