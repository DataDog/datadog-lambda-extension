use std::env;
use std::io::Result;
use std::process::Command;
use structopt::StructOpt;

use super::common::BuildArchitecture;

#[derive(Debug, StructOpt)]
pub struct DeployFunctionOptions {
    #[structopt(long)]
    layer_name: String,
    #[structopt(long)]
    layer_version: i32,
    #[structopt(long)]
    runtime: String,
}

pub async fn deploy_function(args: &DeployFunctionOptions) -> Result<()> {
    Ok(())
}
