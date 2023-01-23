use std::io::Result;
use std::io::{Error, ErrorKind};
use structopt::StructOpt;

use aws_sdk_lambda as lambda;

#[derive(Debug, StructOpt)]
pub struct DeployFunctionOptions {
    #[structopt(long)]
    layer_name: String,
    #[structopt(long)]
    runtime: String,
    #[structopt(long)]
    region: String,
}

pub async fn deploy_function(args: &DeployFunctionOptions) -> Result<()> {
    let latest_layer_version = get_latest_version(&args.layer_name, &args.region).await?;

    Ok(())
}

async fn get_latest_version(layer_name: &str, region: &str) -> Result<i64> {
    std::env::set_var("AWS_REGION", region);
    let config = aws_config::load_from_env().await;
    let lambda_client = lambda::Client::new(&config);
    let result = lambda_client
        .list_layer_versions()
        .set_layer_name(Some(String::from(layer_name)))
        .set_max_items(Some(1))
        .send()
        .await
        .expect("could not list layer versions");

    let layer_versions = match result.layer_versions() {
        Some(layer_versions) => layer_versions,
        None => return Err(Error::new(ErrorKind::InvalidData, "could not get layer versions")),
    };
    
    let latest_version = match layer_versions.first() {
        Some(latest_version) => latest_version,
        None => return Err(Error::new(ErrorKind::InvalidData, "could not get layer versions")),
    };
    Ok(latest_version.version())
}