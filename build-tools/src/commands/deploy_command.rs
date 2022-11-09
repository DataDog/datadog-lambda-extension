use std::fs::File;
use std::io::Read;
use std::io::Result;
use structopt::StructOpt;

use aws_sdk_lambda as lambda;

#[derive(Debug, StructOpt)]
pub struct DeployOptions {
    #[structopt(long)]
    layer_path: String,
    #[structopt(long)]
    layer_name: String,
    #[structopt(long)]
    layer_suffix: Option<String>,
    #[structopt(long, default_value = "sa-east-1")]
    region: String,
}

pub async fn deploy(args: &DeployOptions) -> Result<()> {
    std::env::set_var("AWS_REGION", args.region.clone());
    let config = aws_config::load_from_env().await;
    let lambda_client = lambda::Client::new(&config);

    // build the content object
    let content = aws_sdk_lambda::model::LayerVersionContentInput::builder();
    let blob = get_file_as_vec(&args.layer_path);
    let lambda_blob = aws_sdk_lambda::types::Blob::new(blob);

    let layer_name = build_layer_name(&args.layer_name, &args.layer_suffix);

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

fn get_file_as_vec(filename: &String) -> Vec<u8> {
    let mut f = File::open(filename).expect("could not find the zip");
    let metadata = std::fs::metadata(filename).expect("unable to read metadata");
    let mut buffer = vec![0; metadata.len() as usize];
    f.read_exact(&mut buffer).expect("buffer error");
    buffer
}

fn build_layer_name(layer_name: &str, layer_suffix: &Option<String>) -> String {
    match layer_suffix {
        None => String::from(layer_name),
        Some(layer_suffix) => String::from(layer_name) + "-" + layer_suffix
    }
}