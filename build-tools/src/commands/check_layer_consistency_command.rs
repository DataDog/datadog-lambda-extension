use std::{fmt::Display, io::Result};
use structopt::StructOpt;

use aws_sdk_lambda as lambda;

use crate::security::build_config;

use super::common::{build_layer_name, BuildArchitecture};

#[derive(Debug)]
struct RegionArgs(Vec<String>);

impl std::str::FromStr for RegionArgs {
    type Err = Box<dyn std::error::Error>;
    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        Ok(RegionArgs(
            s.split(',').map(|x| x.trim().to_owned()).collect(),
        ))
    }
}

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
    regions: RegionArgs,
    #[structopt(long)]
    key: Option<String>,
    #[structopt(long)]
    layer_name: String,
    #[structopt(long, possible_values = &BuildArchitecture::variants(), case_insensitive = true, default_value = "amd64")]
    architecture: BuildArchitecture,
    #[structopt(long)]
    layer_suffix: Option<String>,
}

pub async fn get_latest_layer_version(
    key: &Option<String>,
    layer_name: String,
    region: &str,
) -> RegionVersion {
    let region = sanitize(region);
    println!("sanitized region = >{}<", region);
    let config = build_config(key, &region).await;
    let lambda_client = lambda::Client::new(&config);
    let result = lambda_client
        .list_layer_versions()
        .set_layer_name(Some(layer_name))
        .send()
        .await
        .expect("could not get layer version");
    let layer_versions = result
        .layer_versions()
        .expect("could not list layer versions");

    let latest_version = match layer_versions.get(0) {
        Some(layer_version) => layer_version.version(),
        None => 0,
    };
    RegionVersion::new(region, latest_version)
}

pub async fn check_consistency(args: &CheckLayerConsistencyOptions) -> Result<()> {
    let mut last_checked_version: Option<RegionVersion> = None;
    let layer_name = build_layer_name(&args.layer_name, &args.architecture, &args.layer_suffix);
    println!("layer name = {}", layer_name);
    println!("regions = {:?}", args.regions.0);
    for region in args.regions.0.iter() {
        println!("single region = {}", region);
        let current_version = get_latest_layer_version(&args.key, layer_name.clone(), region).await;
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
        }
        last_checked_version = Some(current_version);
    }
    Ok(())
}

fn sanitize(region: &str) -> String {
    // making sure that the list param from github action does not contain any invalid chars
    let region = region.replace('[', "");
    let region = region.replace(']', "");
    let region = region.replace(',', "");
    region.trim().to_string()
}
