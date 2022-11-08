use structopt::StructOpt;
use std::{io::Result};
use std::fs::File;
use std::io::Read;

use aws_sdk_lambda as lambda;
use aws_sdk_sts as sts;

#[derive(Debug,StructOpt)]
pub struct DeployOptions {
    #[structopt(long)]
    mfa_arn: String,
    #[structopt(long)]
    layer_path: String,
    #[structopt(long)]
    layer_name: String,
    #[structopt(long)]
    mfa_code: String,
    #[structopt(long, default_value = "sa-east-1")]
    region: String,
}

pub async fn deploy(args: &DeployOptions)-> Result<()> {
    println!("hello in deploy");
    let config = aws_config::load_from_env().await;

    // get AWS credentials from MFA
    let sts_client = sts::Client::new(&config);
    let command = sts_client.get_session_token();
    let sts_response = command
    .set_serial_number(Some(args.mfa_arn.clone()))
    .set_token_code(Some(args.mfa_code.clone()))
    .send();
    
    let credentials = sts_response.await.expect("could not get credentials from MFA");
    let credentials= credentials.credentials().expect("could not locate credentials");

    

    std::env::set_var("AWS_ACCESS_KEY_ID", credentials.access_key_id().expect("could not find access key"));
    std::env::set_var("AWS_SECRET_ACCESS_KEY", credentials.secret_access_key().expect("could not find access key"));
    std::env::set_var("AWS_SESSION_TOKEN", credentials.session_token().expect("could not find session token"));
    std::env::set_var("AWS_REGION", String::from(args.region.clone()));

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