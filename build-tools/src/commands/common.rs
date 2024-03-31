use aws_config::SdkConfig;
use aws_sdk_sts as sts;
use aws_sdk_sts::types::Credentials;
use serde::Deserialize;
use std::fmt::Display;
use std::io::Result;
use std::{fs::File, io::Read};

use aws_sdk_ec2::primitives::Blob;
#[derive(Debug, clap::ValueEnum, Clone, Default, Deserialize)]
pub enum BuildArchitecture {
    #[serde(alias = "arm64")]
    Arm64,
    #[default]
    #[serde(alias = "amd64")]
    Amd64,
}

impl Display for BuildArchitecture {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BuildArchitecture::Arm64 => write!(f, "arm64"),
            BuildArchitecture::Amd64 => write!(f, "amd64"),
        }
    }
}

pub fn get_file_as_vec(filename: &String) -> Blob {
    let mut f = File::open(filename).expect("could not find the zip");
    let metadata = std::fs::metadata(filename).expect("unable to read metadata");
    let mut buffer = vec![0; metadata.len() as usize];
    f.read_exact(&mut buffer).expect("buffer error");
    Blob::new(buffer)
}

pub async fn build_config(
    region: &str,
    role: Option<String>,
    external_id: Option<String>,
) -> SdkConfig {
    std::env::set_var("AWS_REGION", region);
    if let Some(role) = role {
        if let Some(external_id) = external_id {
            return build_config_assuming_role(role, external_id)
                .await
                .expect("could not build the config for the assumed role");
        } else {
            panic!("role has been set but external is missing");
        }
    }
    aws_config::load_from_env().await
}

pub async fn build_config_assuming_role(role: String, external_id: String) -> Result<SdkConfig> {
    assume_role(role, external_id).await?;
    Ok(aws_config::load_from_env().await)
}

async fn assume_role(role: String, external_id: String) -> Result<()> {
    let config = aws_config::load_from_env().await;
    let sts_client = sts::Client::new(&config);
    let command = sts_client
        .assume_role()
        .external_id(external_id)
        .role_arn(role)
        .role_session_name("build-tools-sesion");

    let sts_response = command.send().await.expect("could not call sts");

    let credentials = sts_response
        .credentials()
        .expect("could not load credentials");

    load_credentials(credentials);
    Ok(())
}

fn load_credentials(credentials: &Credentials) {
    std::env::set_var("AWS_ACCESS_KEY_ID", credentials.access_key_id());
    std::env::set_var("AWS_SECRET_ACCESS_KEY", credentials.secret_access_key());
    std::env::set_var("AWS_SESSION_TOKEN", credentials.session_token());
}
