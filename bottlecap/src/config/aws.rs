use std::{collections::HashMap, env, time::Instant};

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

#[allow(clippy::module_name_repetitions)]
#[derive(Debug, Clone)]
pub struct AwsConfig {
    pub region: String,
    pub aws_lwa_proxy_lambda_runtime_api: Option<String>,
    pub function_name: String,
    pub runtime_api: String,
    pub sandbox_init_time: Instant,
    pub exec_wrapper: Option<String>,
}

use std::sync::Arc;
use tokio::sync::RwLock;

impl AwsConfig {
    #[must_use]
    pub async fn from_env(envs: Arc<RwLock<HashMap<String, String>>>, start_time: Instant) -> Self {
        let envs = envs.read().await;
        Self {
            region: envs.get(AWS_DEFAULT_REGION).unwrap_or(&"us-east-1".to_string()).clone(),
            aws_lwa_proxy_lambda_runtime_api: envs.get(AWS_LWA_LAMBDA_RUNTIME_API_PROXY).cloned(),
            function_name: envs.get(AWS_LAMBDA_FUNCTION_NAME).unwrap_or(&String::new()).clone(),
            runtime_api: envs.get(AWS_LAMBDA_RUNTIME_API).unwrap_or(&String::new()).clone(),
            sandbox_init_time: start_time,
            exec_wrapper: envs.get(AWS_LAMBDA_EXEC_WRAPPER).cloned(),
        }
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
    pub async fn from_env(envs: Arc<RwLock<HashMap<String, String>>>) -> Self {
        let envs = envs.read().await;
        Self {
            aws_access_key_id: envs.get(AWS_ACCESS_KEY_ID).unwrap_or(&String::new()).clone(),
            aws_secret_access_key: envs.get(AWS_SECRET_ACCESS_KEY).unwrap_or(&String::new()).clone(),
            aws_session_token: envs.get(AWS_SESSION_TOKEN).unwrap_or(&String::new()).clone(),
            aws_container_credentials_full_uri: envs.get(AWS_CONTAINER_CREDENTIALS_FULL_URI)
                .unwrap_or(&String::new()).clone(),
            aws_container_authorization_token: envs.get(AWS_CONTAINER_AUTHORIZATION_TOKEN)
                .unwrap_or(&String::new()).clone(),
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
