use aws_sdk_lambda::types::ByteStream;
use aws_sdk_s3 as s3;
use aws_sdk_signer as signer;
use rand::{distributions::Alphanumeric, Rng};
use signer::model::{Destination, S3Destination, S3Source, Source};
use std::{io::Result, path::Path};
use structopt::StructOpt;

const BUCKET_NAME: &str = "dd-lambda-signing-bucket-sandbox";
const SIGNING_PROFILE_NAME: &str = "DatadogLambdaSigningProfile";

#[derive(Debug, StructOpt)]
pub struct SignOptions {
    #[structopt(long)]
    layer_path: String,
}

pub async fn sign(args: &SignOptions) -> Result<()> {
    std::env::set_var("AWS_REGION", "sa-east-1");
    let config = aws_config::load_from_env().await;
    let s3_client = s3::Client::new(&config);
    let signer_client = signer::Client::new(&config);
    let key = build_s3_key();
    upload_object(args, &key, &s3_client)
        .await
        .expect("could not upload the layer");
    sign_object(&key, &signer_client)
        .await
        .expect("could not sign layer");
    Ok(())
}

async fn upload_object(args: &SignOptions, key: &str, s3_client: &s3::Client) -> Result<()> {
    let body = ByteStream::from_path(Path::new(&args.layer_path))
        .await
        .expect("could not load the file");
    s3_client
        .put_object()
        .body(body)
        .bucket(BUCKET_NAME)
        .key(key)
        .send()
        .await
        .expect("error while uploading the layer");
    Ok(())
}

async fn sign_object(key: &str, signer_client: &signer::Client) -> Result<String> {
    let source = build_source(key);
    let destination = build_destination();
    let result = signer_client
        .start_signing_job()
        .source(source)
        .destination(destination)
        .profile_name(SIGNING_PROFILE_NAME)
        .send()
        .await
        .expect("coud not start the signing job");
    match result.job_id() {
        Some(job_id) => Ok(String::from(job_id)),
        None => panic!("could not find the job id"),
    }
}

fn random_string() -> String {
    rand::thread_rng()
        .sample_iter(&Alphanumeric)
        .take(32)
        .map(char::from)
        .collect()
}

fn build_s3_key() -> String {
    random_string() + &".zip".to_string()
}

fn build_source(key: &str) -> Source {
    let s3_source = S3Source::builder()
        .bucket_name(BUCKET_NAME)
        .key(key)
        .version("null")
        .build();
    Source::builder().s3(s3_source).build()
}

fn build_destination() -> Destination {
    let s3_destination = S3Destination::builder().bucket_name(BUCKET_NAME).build();
    Destination::builder().s3(s3_destination).build()
}
