use std::{fmt::Display, io::Result};
use structopt::StructOpt;

use aws_sdk_lambda as lambda;

use crate::security::build_config;

use super::common::{build_layer_name, BuildArchitecture};

pub struct RegionVersion {
    region: String,
    version: i64,
}

impl RegionVersion {
    fn new(region: String, version: i64) -> Self {
        RegionVersion { region, version }
    }
}

impl Display for RegionVersion {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "[Region:{} Version:{}]", self.region, self.version)
    }
}

#[derive(Debug, StructOpt)]
pub struct CheckLayerConsistencyOptions {
    #[structopt(long)]
    regions: Vec<String>,
    #[structopt(long)]
    key: Option<String>,
    #[structopt(long)]
    layer_name: String,
    #[structopt(long, possible_values = &BuildArchitecture::variants(), case_insensitive = true, default_value = "amd64")]
    architecture: BuildArchitecture,
    #[structopt(long)]
    layer_suffix: Option<String>,
}

pub async fn get_layer_version(
    key: &Option<String>,
    layer_name: String,
    region: &str,
) -> RegionVersion {
    let config = build_config(key, region).await;
    let lambda_client = lambda::Client::new(&config);
    let result = lambda_client
        .get_layer_version()
        .set_layer_name(Some(layer_name))
        .send()
        .await
        .expect("could not get layer version");
    RegionVersion::new(String::from(region), result.version())
}

pub async fn check_consistency(args: &CheckLayerConsistencyOptions) -> Result<()> {
    let mut last_checked_version: Option<RegionVersion> = None;
    let layer_name = build_layer_name(&args.layer_name, &args.architecture, &args.layer_suffix);
    for region in args.regions.iter() {
        let current_version = get_layer_version(&args.key, layer_name.clone(), region).await;
        if let Some(checked_version) = last_checked_version {
            if checked_version.version != current_version.version {
                let error_message = format!(
                    "layer version mismatch: {} and {}",
                    checked_version, current_version
                );
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    error_message,
                ));
            }
            last_checked_version = Some(checked_version);
        }
    }
    Ok(())
}
