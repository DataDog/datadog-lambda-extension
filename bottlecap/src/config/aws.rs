use std::env;
use tokio::time::Instant;

const AWS_DEFAULT_REGION: &str = "AWS_DEFAULT_REGION";
const AWS_ACCESS_KEY_ID: &str = "AWS_ACCESS_KEY_ID";
const AWS_SECRET_ACCESS_KEY: &str = "AWS_SECRET_ACCESS_KEY";
const AWS_SESSION_TOKEN: &str = "AWS_SESSION_TOKEN";
const AWS_CONTAINER_CREDENTIALS_FULL_URI: &str = "AWS_CONTAINER_CREDENTIALS_FULL_URI";
const AWS_CONTAINER_AUTHORIZATION_TOKEN: &str = "AWS_CONTAINER_AUTHORIZATION_TOKEN";
const AWS_LAMBDA_FUNCTION_NAME: &str = "AWS_LAMBDA_FUNCTION_NAME";
const AWS_LAMBDA_RUNTIME_API: &str = "AWS_LAMBDA_RUNTIME_API";
const AWS_LWA_LAMBDA_RUNTIME_API_PROXY: &str = "AWS_LWA_LAMBDA_RUNTIME_API_PROXY";
const AWS_LAMBDA_EXEC_WRAPPER: &str = "AWS_LAMBDA_EXEC_WRAPPER";
const AWS_LAMBDA_MAX_CONCURRENCY: &str = "AWS_LAMBDA_MAX_CONCURRENCY";

#[allow(clippy::module_name_repetitions)]
#[derive(Debug, Clone)]
pub struct AwsConfig {
    pub region: String,
    pub aws_lwa_proxy_lambda_runtime_api: Option<String>,
    pub function_name: String,
    pub runtime_api: String,
    pub sandbox_init_time: Instant,
    pub exec_wrapper: Option<String>,
    pub max_concurrency: Option<String>,
}

impl AwsConfig {
    #[must_use]
    pub fn from_env(start_time: Instant) -> Self {
        Self {
            region: env::var(AWS_DEFAULT_REGION).unwrap_or("us-east-1".to_string()),
            aws_lwa_proxy_lambda_runtime_api: env::var(AWS_LWA_LAMBDA_RUNTIME_API_PROXY).ok(),
            function_name: env::var(AWS_LAMBDA_FUNCTION_NAME).unwrap_or_default(),
            runtime_api: env::var(AWS_LAMBDA_RUNTIME_API).unwrap_or_default(),
            sandbox_init_time: start_time,
            exec_wrapper: env::var(AWS_LAMBDA_EXEC_WRAPPER).ok(),
            max_concurrency: env::var(AWS_LAMBDA_MAX_CONCURRENCY).ok(),
        }
    }

    #[must_use]
    pub fn is_elevator_mode(&self) -> bool {
        self.max_concurrency.is_some()
    }
}

#[allow(clippy::module_name_repetitions)]
#[derive(Debug, Clone)]
pub struct AwsCredentials {
    pub aws_access_key_id: String,
    pub aws_secret_access_key: String,
    pub aws_session_token: String,
    pub aws_container_credentials_full_uri: String,
    pub aws_container_authorization_token: String,
}

impl AwsCredentials {
    #[must_use]
    pub fn from_env() -> Self {
        Self {
            aws_access_key_id: env::var(AWS_ACCESS_KEY_ID).unwrap_or_default(),
            aws_secret_access_key: env::var(AWS_SECRET_ACCESS_KEY).unwrap_or_default(),
            aws_session_token: env::var(AWS_SESSION_TOKEN).unwrap_or_default(),
            aws_container_credentials_full_uri: env::var(AWS_CONTAINER_CREDENTIALS_FULL_URI)
                .unwrap_or_default(),
            aws_container_authorization_token: env::var(AWS_CONTAINER_AUTHORIZATION_TOKEN)
                .unwrap_or_default(),
        }
    }
}

#[must_use]
pub fn get_aws_partition_by_region(region: &str) -> String {
    match region {
        r if r.starts_with("us-gov-") => "aws-us-gov".to_string(),
        r if r.starts_with("cn-") => "aws-cn".to_string(),
        _ => "aws".to_string(),
    }
}

#[must_use]
pub fn build_lambda_function_arn(account_id: &str, region: &str, function_name: &str) -> String {
    let aws_partition = get_aws_partition_by_region(region);
    format!("arn:{aws_partition}:lambda:{region}:{account_id}:function:{function_name}")
}
