use std::io::Result;
use structopt::StructOpt;

use aws_sdk_lambda as lambda;

use crate::security::build_config;

use super::common::build_layer_name;
use super::common::get_file_as_vec;
use super::common::BuildArchitecture;

#[derive(Debug, StructOpt)]
pub struct DeployOptions {
    #[structopt(long)]
    layer_path: String,
    #[structopt(long)]
    layer_name: String,
    #[structopt(long, possible_values = &BuildArchitecture::variants(), case_insensitive = true, default_value = "amd64")]
    architecture: BuildArchitecture,
    #[structopt(long)]
    layer_suffix: Option<String>,
    #[structopt(long)]
    key: Option<String>,
    #[structopt(long, default_value = "sa-east-1")]
    region: String,
}

pub async fn deploy(args: &DeployOptions) -> Result<()> {
    let config = build_config(&args.key, &args.region).await;
    let lambda_client = lambda::Client::new(&config);

    // build the content object
    let content = aws_sdk_lambda::model::LayerVersionContentInput::builder();
    let lambda_blob = get_file_as_vec(&args.layer_path);

    let layer_name = build_layer_name(&args.layer_name, &args.architecture, &args.layer_suffix);

    // publish layer
    let builder = lambda_client.publish_layer_version();
    builder
        .set_layer_name(Some(layer_name))
        .set_content(Some(content.set_zip_file(Some(lambda_blob)).build()))
        .send()
        .await
        .expect("error while publishing the layer");
    Ok(())
}
