
use aws_sdk_s3 as s3;

use std::{
    fs::File,
    io::{Result, Write}
};
use structopt::StructOpt;

use super::common::build_config;

const BUCKET_NAME: &str = "dd-lambda-signing-bucket-serverless-sandbox";

#[derive(Debug, StructOpt)]
pub struct DownloadSignedOptions {
    #[structopt(long)]
    pub job_id: String,
    #[structopt(long)]
    pub assume_role: Option<String>,
    #[structopt(long)]
    pub external_id: Option<String>,
    #[structopt(long)]
    destination_path: String,
}

pub async fn download_signed(args: &DownloadSignedOptions) -> Result<()> {
    let config = build_config(
        "sa-east-1",
        args.assume_role.clone(),
        args.external_id.clone(),
    )
    .await;
    let s3_client = s3::Client::new(&config);
    download_object(args, &s3_client).await?;
    Ok(())
}

async fn download_object(args: &DownloadSignedOptions, s3_client: &s3::Client) -> Result<()> {
    let response = s3_client
        .get_object()
        .bucket(BUCKET_NAME)
        .key(args.job_id.to_string() + ".zip")
        .send()
        .await
        .expect("error while download the signed layer");
    let data = response.body.collect().await?.into_bytes();
    let buffer = Vec::from(data);
    let mut file = File::create(args.destination_path.clone())?;
    file.write_all(&buffer)?;
    Ok(())
}
