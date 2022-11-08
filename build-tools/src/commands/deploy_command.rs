use structopt::StructOpt;
use std::{io::Result};
use std::fs::File;
use std::io::Read;

use aws_sdk_lambda as lambda;

#[derive(Debug,StructOpt)]
pub struct DeployOptions {
    #[structopt(long)]
    layer_path: String,
    #[structopt(long)]
    layer_name: String,
    #[structopt(long, default_value = "sa-east-1")]
    region: String,
}

pub async fn deploy(args: &DeployOptions)-> Result<()> {
    println!("hello in deploy");
  
    std::env::set_var("AWS_REGION", args.region.clone());

    let config = aws_config::load_from_env().await;
    let lambda_client = lambda::Client::new(&config);

    let content = aws_sdk_lambda::model::LayerVersionContentInput::builder();

    let blob = get_file_as_byte_vec(&args.layer_path);
    let lambda_blob = aws_sdk_lambda::types::Blob::new(blob);

    let builder = lambda_client.publish_layer_version();
    builder
    .set_layer_name(Some(args.layer_name.clone()))
    .set_content(Some(content.set_zip_file(Some(lambda_blob)).build()))
    .send().await.expect("error while publishing the layer");

    Ok(())
 
}

fn get_file_as_byte_vec(filename: &String) -> Vec<u8> {
    let mut f = File::open(&filename).expect("could not find the zip");
    let metadata = std::fs::metadata(&filename).expect("unable to read metadata");
    let mut buffer = vec![0; metadata.len() as usize];
    f.read(&mut buffer).expect("buffer overflow");
    buffer
}