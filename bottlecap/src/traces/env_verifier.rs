// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use async_trait::async_trait;
use hyper::{Body, Client, Method, Request, Response};
use log::{debug, error};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::Path;
use std::process;
use std::sync::Arc;
use std::time::{Duration, Instant};

use datadog_trace_utils::trace_utils;

const GCP_METADATA_URL: &str = "http://metadata.google.internal/computeMetadata/v1/?recursive=true";
const AZURE_LINUX_FUNCTION_ROOT_PATH_STR: &str = "/home/site/wwwroot";
const AZURE_WINDOWS_FUNCTION_ROOT_PATH_STR: &str = "C:\\home\\site\\wwwroot";
const AZURE_HOST_JSON_NAME: &str = "host.json";
const AZURE_FUNCTION_JSON_NAME: &str = "function.json";

#[derive(Default, Debug, Deserialize, Serialize, Eq, PartialEq)]
pub struct GCPMetadata {
    pub instance: GCPInstance,
    pub project: GCPProject,
}

#[derive(Debug, Deserialize, Serialize, Eq, PartialEq)]
pub struct GCPInstance {
    pub region: String,
}
impl Default for GCPInstance {
    fn default() -> Self {
        Self {
            region: "unknown".to_string(),
        }
    }
}

#[derive(Debug, Deserialize, Serialize, Eq, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct GCPProject {
    pub project_id: String,
}
impl Default for GCPProject {
    fn default() -> Self {
        Self {
            project_id: "unknown".to_string(),
        }
    }
}

#[async_trait]
pub trait EnvVerifier {
    /// Verifies the mini agent is running in the intended environment. if not, exit the process.
    /// Returns MiniAgentMetadata, a struct of metadata collected from the environment.
    async fn verify_environment(
        &self,
        verify_env_timeout: u64,
        env_type: &trace_utils::EnvironmentType,
        os: &str,
    ) -> trace_utils::MiniAgentMetadata;
}

pub struct ServerlessEnvVerifier {
    gmc: Arc<Box<dyn GoogleMetadataClient + Send + Sync>>,
}

impl Default for ServerlessEnvVerifier {
    fn default() -> Self {
        Self::new()
    }
}

impl ServerlessEnvVerifier {
    pub fn new() -> Self {
        Self {
            gmc: Arc::new(Box::new(GoogleMetadataClientWrapper {})),
        }
    }

    #[cfg(test)]
    pub(crate) fn new_with_google_metadata_client(
        gmc: Box<dyn GoogleMetadataClient + Send + Sync>,
    ) -> Self {
        Self { gmc: Arc::new(gmc) }
    }

    async fn verify_gcp_environment_or_exit(
        &self,
        verify_env_timeout: u64,
    ) -> trace_utils::MiniAgentMetadata {
        let gcp_metadata_request = ensure_gcp_function_environment(self.gmc.as_ref().as_ref());
        let gcp_metadata = match tokio::time::timeout(
            Duration::from_millis(verify_env_timeout),
            gcp_metadata_request,
        )
        .await
        {
            Ok(result) => match result {
                Ok(metadata) => {
                    debug!("Successfully fetched Google Metadata.");
                    metadata
                }
                Err(err) => {
                    error!("The Mini Agent can only be run in Google Cloud Functions & Azure Functions. Verification has failed, shutting down now. Error: {err}");
                    process::exit(1);
                }
            },
            Err(_) => {
                error!("Google Metadata request timeout of {verify_env_timeout} ms exceeded. Using default values.");
                GCPMetadata::default()
            }
        };
        trace_utils::MiniAgentMetadata {
            gcp_project_id: Some(gcp_metadata.project.project_id),
            gcp_region: Some(get_region_from_gcp_region_string(
                gcp_metadata.instance.region,
            )),
        }
    }
}

#[async_trait]
impl EnvVerifier for ServerlessEnvVerifier {
    async fn verify_environment(
        &self,
        verify_env_timeout: u64,
        env_type: &trace_utils::EnvironmentType,
        os: &str,
    ) -> trace_utils::MiniAgentMetadata {
        match env_type {
            trace_utils::EnvironmentType::AzureFunction => {
                verify_azure_environment_or_exit(os).await;
                trace_utils::MiniAgentMetadata::default()
            }
            trace_utils::EnvironmentType::CloudFunction => {
                return self
                    .verify_gcp_environment_or_exit(verify_env_timeout)
                    .await;
            }
            trace_utils::EnvironmentType::LambdaFunction => {
                trace_utils::MiniAgentMetadata::default()
            }
        }
    }
}

/// The region found in GCP Metadata comes in the format: "projects/123123/regions/us-east1"
/// This function extracts just the region (us-east1) from this GCP region string.
/// If the string does not have 4 parts (separated by "/") or extraction fails, return "unknown"
fn get_region_from_gcp_region_string(str: String) -> String {
    let split_str = str.split('/').collect::<Vec<&str>>();
    if split_str.len() != 4 {
        return "unknown".to_string();
    }
    match split_str.last() {
        Some(res) => res.to_string(),
        None => "unknown".to_string(),
    }
}

/// GoogleMetadataClient trait is used so we can mock a google metadata server response in unit
/// tests
#[async_trait]
pub(crate) trait GoogleMetadataClient {
    async fn get_metadata(&self) -> anyhow::Result<Response<Body>>;
}
struct GoogleMetadataClientWrapper {}

#[async_trait]
impl GoogleMetadataClient for GoogleMetadataClientWrapper {
    async fn get_metadata(&self) -> anyhow::Result<Response<Body>> {
        let req = Request::builder()
            .method(Method::POST)
            .uri(GCP_METADATA_URL)
            .header("Metadata-Flavor", "Google")
            .body(Body::empty())
            .map_err(|err| anyhow::anyhow!(err.to_string()))?;

        let client = Client::new();
        match client.request(req).await {
            Ok(res) => Ok(res),
            Err(err) => anyhow::bail!(err.to_string()),
        }
    }
}

/// Checks if we are running in a Google Cloud Function environment.
/// If true, returns Metadata from the Google Cloud environment.
/// Otherwise, returns an error with the verification failure reason.
async fn ensure_gcp_function_environment(
    metadata_client: &(dyn GoogleMetadataClient + Send + Sync),
) -> anyhow::Result<GCPMetadata> {
    let response = metadata_client.get_metadata().await.map_err(|err| {
        anyhow::anyhow!("Can't communicate with Google Metadata Server. Error: {err}")
    })?;

    let (parts, body) = response.into_parts();
    let headers = parts.headers;
    match headers.get("Server") {
        Some(val) => {
            if val != "Metadata Server for Serverless" {
                anyhow::bail!("In Google Cloud, but not in a function environment.")
            }
        }
        None => {
            anyhow::bail!("In Google Cloud, but server identifier not found.")
        }
    }

    let gcp_metadata = match get_gcp_metadata_from_body(body).await {
        Ok(res) => res,
        Err(err) => {
            error!("Failed to get GCP Function Metadata. Will not enrich spans. {err}");
            return Ok(GCPMetadata::default());
        }
    };

    Ok(gcp_metadata)
}

async fn get_gcp_metadata_from_body(body: hyper::Body) -> anyhow::Result<GCPMetadata> {
    let bytes = hyper::body::to_bytes(body).await?;
    let body_str = String::from_utf8(bytes.to_vec())?;
    let gcp_metadata: GCPMetadata = serde_json::from_str(&body_str)?;
    Ok(gcp_metadata)
}

async fn verify_azure_environment_or_exit(os: &str) {
    let now = Instant::now();
    match ensure_azure_function_environment(Box::new(AzureVerificationClientWrapper {}), os).await {
        Ok(_) => {
            debug!("Successfully verified Azure Function Environment.");
        }
        Err(e) => {
            error!("The Mini Agent can only be run in Google Cloud Functions & Azure Functions. Verification has failed, shutting down now. Error: {e}");
            process::exit(1);
        }
    }
    debug!(
        "Time taken to verify Azure Functions env: {} ms",
        now.elapsed().as_millis()
    );
}

/// AzureVerificationClient trait is used so we can mock the azure function local url response in
/// unit tests
trait AzureVerificationClient {
    fn get_function_root_files(&self, path: &Path) -> anyhow::Result<Vec<String>>;
}
struct AzureVerificationClientWrapper {}

impl AzureVerificationClient for AzureVerificationClientWrapper {
    fn get_function_root_files(&self, path: &Path) -> anyhow::Result<Vec<String>> {
        let mut file_names: Vec<String> = Vec::new();

        let entries = fs::read_dir(path)?;
        for entry in entries {
            let entry = entry.map_err(|e| anyhow::anyhow!(e))?;
            let entry_name = entry.file_name();
            if entry_name == "node_modules" {
                continue;
            }

            file_names.push(entry_name.to_string_lossy().to_string());

            if entry.file_type()?.is_dir() {
                let sub_entries = fs::read_dir(entry.path())?;
                for sub_entry in sub_entries {
                    let sub_entry = sub_entry.map_err(|e| anyhow::anyhow!(e))?;
                    let sub_entry_name = sub_entry.file_name();
                    file_names.push(sub_entry_name.to_string_lossy().to_string());
                }
            }
        }
        Ok(file_names)
    }
}

/// Checks if we are running in an Azure Function environment.
/// If true, returns MiniAgentMetadata default.
/// Otherwise, returns an error with the verification failure reason.
async fn ensure_azure_function_environment(
    verification_client: Box<dyn AzureVerificationClient + Send + Sync>,
    os: &str,
) -> anyhow::Result<()> {
    let azure_linux_function_root_path = Path::new(AZURE_LINUX_FUNCTION_ROOT_PATH_STR);
    let azure_windows_function_root_path = Path::new(AZURE_WINDOWS_FUNCTION_ROOT_PATH_STR);
    let function_files = match os {
        "linux" => verification_client.get_function_root_files(azure_linux_function_root_path),
        "windows" => verification_client.get_function_root_files(azure_windows_function_root_path),
        _ => {
            anyhow::bail!("The Serverless Mini Agent does not support this platform.")
        }
    };

    let function_files = function_files.map_err(|e| anyhow::anyhow!(e))?;

    let mut host_json_exists = false;
    let mut function_json_exists = false;
    for file in function_files {
        if file == AZURE_HOST_JSON_NAME {
            host_json_exists = true;
        }
        if file == AZURE_FUNCTION_JSON_NAME {
            function_json_exists = true;
        }
    }

    if !host_json_exists && !function_json_exists {
        anyhow::bail!("Failed to validate an Azure Function directory system.");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use async_trait::async_trait;
    use datadog_trace_utils::trace_utils;
    use hyper::{Body, Response, StatusCode};
    use serde_json::json;
    use serial_test::serial;
    use std::{fs, path::Path, time::Duration};

    use crate::env_verifier::{
        ensure_azure_function_environment, ensure_gcp_function_environment,
        get_region_from_gcp_region_string, AzureVerificationClient, AzureVerificationClientWrapper,
        GCPInstance, GCPMetadata, GCPProject, GoogleMetadataClient, AZURE_FUNCTION_JSON_NAME,
        AZURE_HOST_JSON_NAME,
    };

    use super::{EnvVerifier, ServerlessEnvVerifier};

    #[tokio::test]
    #[cfg_attr(miri, ignore)]
    async fn test_ensure_gcp_env_false_if_metadata_server_unreachable() {
        struct MockGoogleMetadataClient {}
        #[async_trait]
        impl GoogleMetadataClient for MockGoogleMetadataClient {
            async fn get_metadata(&self) -> anyhow::Result<Response<Body>> {
                anyhow::bail!("Random Error")
            }
        }
        let gmc =
            Box::new(MockGoogleMetadataClient {}) as Box<dyn GoogleMetadataClient + Send + Sync>;
        let res = ensure_gcp_function_environment(gmc.as_ref()).await;
        assert!(res.is_err());
        assert_eq!(
            res.unwrap_err().to_string(),
            "Can't communicate with Google Metadata Server. Error: Random Error"
        );
    }

    #[tokio::test]
    #[cfg_attr(miri, ignore)]
    async fn test_ensure_gcp_env_false_if_no_server_in_response_headers() {
        struct MockGoogleMetadataClient {}
        #[async_trait]
        impl GoogleMetadataClient for MockGoogleMetadataClient {
            async fn get_metadata(&self) -> anyhow::Result<Response<Body>> {
                Ok(Response::builder()
                    .status(StatusCode::OK)
                    .body(Body::empty())
                    .unwrap())
            }
        }
        let gmc =
            Box::new(MockGoogleMetadataClient {}) as Box<dyn GoogleMetadataClient + Send + Sync>;
        let res = ensure_gcp_function_environment(gmc.as_ref()).await;
        assert!(res.is_err());
        assert_eq!(
            res.unwrap_err().to_string(),
            "In Google Cloud, but server identifier not found."
        );
    }

    #[tokio::test]
    #[cfg_attr(miri, ignore)]
    async fn test_ensure_gcp_env_if_server_header_not_serverless() {
        struct MockGoogleMetadataClient {}
        #[async_trait]
        impl GoogleMetadataClient for MockGoogleMetadataClient {
            async fn get_metadata(&self) -> anyhow::Result<Response<Body>> {
                Ok(Response::builder()
                    .status(StatusCode::OK)
                    .header("Server", "Metadata Server NOT for Serverless")
                    .body(Body::empty())
                    .unwrap())
            }
        }
        let gmc =
            Box::new(MockGoogleMetadataClient {}) as Box<dyn GoogleMetadataClient + Send + Sync>;
        let res = ensure_gcp_function_environment(gmc.as_ref()).await;
        assert!(res.is_err());
        assert_eq!(
            res.unwrap_err().to_string(),
            "In Google Cloud, but not in a function environment."
        );
    }

    #[tokio::test]
    #[cfg_attr(miri, ignore)]
    async fn test_ensure_gcp_env_true_if_cloud_function_env() {
        struct MockGoogleMetadataClient {}
        #[async_trait]
        impl GoogleMetadataClient for MockGoogleMetadataClient {
            async fn get_metadata(&self) -> anyhow::Result<Response<Body>> {
                Ok(Response::builder()
                    .status(StatusCode::OK)
                    .header("Server", "Metadata Server for Serverless")
                    .body(Body::from(
                        json!({
                            "instance": {
                                "region": "projects/123123/regions/us-east1",
                            },
                            "project": {
                                "projectId": "my-project"
                            }
                        })
                        .to_string(),
                    ))
                    .unwrap())
            }
        }
        let gmc =
            Box::new(MockGoogleMetadataClient {}) as Box<dyn GoogleMetadataClient + Send + Sync>;
        let res = ensure_gcp_function_environment(gmc.as_ref()).await;
        assert!(res.is_ok());
        assert_eq!(
            res.unwrap(),
            GCPMetadata {
                instance: GCPInstance {
                    region: "projects/123123/regions/us-east1".to_string()
                },
                project: GCPProject {
                    project_id: "my-project".to_string()
                }
            }
        );
    }

    #[tokio::test]
    #[cfg_attr(miri, ignore)]
    async fn test_gcp_verify_environment_timeout_exceeded_gives_unknown_values() {
        struct MockGoogleMetadataClient {}
        #[async_trait]
        impl GoogleMetadataClient for MockGoogleMetadataClient {
            async fn get_metadata(&self) -> anyhow::Result<Response<Body>> {
                // Sleep for 5 seconds to let the timeout trigger
                tokio::time::sleep(Duration::from_secs(5)).await;
                Ok(Response::builder()
                    .status(StatusCode::OK)
                    .body(Body::empty())
                    .unwrap())
            }
        }
        let gmc =
            Box::new(MockGoogleMetadataClient {}) as Box<dyn GoogleMetadataClient + Send + Sync>;
        let env_verifier = ServerlessEnvVerifier::new_with_google_metadata_client(gmc);
        let res = env_verifier
            .verify_environment(100, &trace_utils::EnvironmentType::CloudFunction, "linux")
            .await; // set the verify_env_timeout to a small value to trigger the timeout
        assert_eq!(
            res,
            trace_utils::MiniAgentMetadata {
                gcp_project_id: Some("unknown".to_string()),
                gcp_region: Some("unknown".to_string()),
            }
        );
    }

    #[test]
    fn test_gcp_region_string_extraction_valid_string() {
        let res = get_region_from_gcp_region_string("projects/123123/regions/us-east1".to_string());
        assert_eq!(res, "us-east1");
    }

    #[test]
    fn test_gcp_region_string_extraction_wrong_number_of_parts() {
        let res = get_region_from_gcp_region_string("invalid/parts/count".to_string());
        assert_eq!(res, "unknown");
    }

    #[test]
    fn test_gcp_region_string_extraction_empty_string() {
        let res = get_region_from_gcp_region_string("".to_string());
        assert_eq!(res, "unknown");
    }

    #[tokio::test]
    #[cfg_attr(miri, ignore)]
    async fn test_ensure_azure_env_windows_true() {
        struct MockAzureVerificationClient {}
        #[async_trait]
        impl AzureVerificationClient for MockAzureVerificationClient {
            fn get_function_root_files(&self, _path: &Path) -> anyhow::Result<Vec<String>> {
                Ok(vec!["host.json".to_string(), "function.json".to_string()])
            }
        }
        let res =
            ensure_azure_function_environment(Box::new(MockAzureVerificationClient {}), "windows")
                .await;
        assert!(res.is_ok());
    }

    #[tokio::test]
    #[cfg_attr(miri, ignore)]
    async fn test_ensure_azure_env_windows_false() {
        struct MockAzureVerificationClient {}
        #[async_trait]
        impl AzureVerificationClient for MockAzureVerificationClient {
            fn get_function_root_files(&self, _path: &Path) -> anyhow::Result<Vec<String>> {
                Ok(vec![
                    "random_file.json".to_string(),
                    "random_file_1.json".to_string(),
                ])
            }
        }
        let res =
            ensure_azure_function_environment(Box::new(MockAzureVerificationClient {}), "windows")
                .await;
        assert!(res.is_err());
        assert_eq!(
            res.unwrap_err().to_string(),
            "Failed to validate an Azure Function directory system."
        );
    }

    #[tokio::test]
    #[cfg_attr(miri, ignore)]
    async fn test_ensure_azure_env_linux_true() {
        struct MockAzureVerificationClient {}
        #[async_trait]
        impl AzureVerificationClient for MockAzureVerificationClient {
            fn get_function_root_files(&self, _path: &Path) -> anyhow::Result<Vec<String>> {
                Ok(vec!["host.json".to_string(), "function.json".to_string()])
            }
        }
        let res =
            ensure_azure_function_environment(Box::new(MockAzureVerificationClient {}), "linux")
                .await;
        assert!(res.is_ok());
    }

    #[tokio::test]
    #[cfg_attr(miri, ignore)]
    async fn test_ensure_azure_env_linux_false() {
        struct MockAzureVerificationClient {}
        #[async_trait]
        impl AzureVerificationClient for MockAzureVerificationClient {
            fn get_function_root_files(&self, _path: &Path) -> anyhow::Result<Vec<String>> {
                Ok(vec![
                    "random_file.json".to_string(),
                    "random_file_1.json".to_string(),
                ])
            }
        }
        let res =
            ensure_azure_function_environment(Box::new(MockAzureVerificationClient {}), "linux")
                .await;
        assert!(res.is_err());
        assert_eq!(
            res.unwrap_err().to_string(),
            "Failed to validate an Azure Function directory system."
        );
    }

    #[test]
    #[serial]
    fn test_get_function_root_files_returns_correct_files() {
        let temp_dir = tempfile::tempdir().unwrap();
        let temp_dir_path = temp_dir.path();

        fs::File::create(temp_dir_path.join(AZURE_HOST_JSON_NAME)).unwrap();
        fs::create_dir(temp_dir_path.join("HttpTrigger1")).unwrap();
        fs::File::create(temp_dir_path.join(format!("HttpTrigger1/{AZURE_FUNCTION_JSON_NAME}")))
            .unwrap();

        let client = AzureVerificationClientWrapper {};

        let files = client.get_function_root_files(temp_dir_path).unwrap();

        assert!(files.contains(&AZURE_HOST_JSON_NAME.to_string()));
        assert!(files.contains(&AZURE_FUNCTION_JSON_NAME.to_string()));
        assert!(files.contains(&"HttpTrigger1".to_string()));
    }

    #[test]
    #[serial]
    fn test_get_function_root_files_ignores_node_modules() {
        let temp_dir = tempfile::tempdir().unwrap();
        let temp_dir_path = temp_dir.path();

        fs::File::create(temp_dir_path.join(AZURE_HOST_JSON_NAME)).unwrap();
        fs::create_dir(temp_dir_path.join("node_modules")).unwrap();
        fs::File::create(temp_dir_path.join("node_modules/random.txt")).unwrap();

        let client = AzureVerificationClientWrapper {};

        let files = client.get_function_root_files(temp_dir_path).unwrap();

        assert_eq!(files, vec![AZURE_HOST_JSON_NAME]);
    }
}
