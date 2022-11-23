use aws_sdk_lambda::types::ByteStream;
use aws_sdk_s3 as s3;
use aws_sdk_signer as signer;
use core::time;
use rand::{distributions::Alphanumeric, Rng};
use signer::model::{Destination, S3Destination, S3Source, SigningStatus, Source};
use std::{
    io::{Error, ErrorKind, Result},
    path::Path,
    thread,
};
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
    upload_object(args, &key, &s3_client).await?;
    let job_id = sign_object(&key, &signer_client).await?;
    verify(&job_id, &signer_client).await?;
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
        None => Err(Error::new(ErrorKind::NotFound, "could not find the job id")),
    }
}

fn is_job_completed(status: Option<&SigningStatus>) -> Result<bool> {
    match status {
        Some(SigningStatus::InProgress) => Ok(false),
        Some(SigningStatus::Succeeded) => Ok(true),
        _ => Err(Error::new(ErrorKind::InvalidData, "job has failed")),
    }
}

async fn verify(job_id: &str, signer_client: &signer::Client) -> Result<()> {
    let result = signer_client
        .describe_signing_job()
        .job_id(job_id)
        .send()
        .await
        .expect("could not verify the job id");

    let delay = time::Duration::from_secs(15);

    for _ in 0..5 {
        let result = is_job_completed(result.status())?;
        if result {
            return Ok(());
        }
        thread::sleep(delay);
    }
    Err(Error::new(ErrorKind::TimedOut, "the signing job timeouts"))
}

fn random_string() -> String {
    rand::thread_rng()
        .sample_iter(&Alphanumeric)
        .take(32)
        .map(char::from)
        .collect()
}

fn build_s3_key() -> String {
    random_string() + ".zip"
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
