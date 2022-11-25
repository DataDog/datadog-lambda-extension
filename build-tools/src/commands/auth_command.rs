use std::io::Result;
use structopt::StructOpt;

use aws_sdk_sts as sts;

use crate::security::encrypt_credentials_to_output;

#[derive(Debug, StructOpt)]
pub struct AuthOptions {
    #[structopt(long)]
    mfa_arn: String,
    #[structopt(long)]
    mfa_code: String,
    #[structopt(long)]
    key: String,
}

pub async fn auth(args: &AuthOptions) -> Result<()> {
    // set a random AWS_REGION as sts in region-agnostic
    std::env::set_var("AWS_REGION", "us-east-1");

    // get AWS credentials from MFA
    let config = aws_config::load_from_env().await;
    let sts_client = sts::Client::new(&config);
    let command = sts_client.get_session_token();
    let sts_response = command
        .set_serial_number(Some(args.mfa_arn.clone()))
        .set_token_code(Some(args.mfa_code.clone()))
        .send()
        .await
        .expect("could not call sts");

    let credentials = sts_response
        .credentials()
        .expect("could not load credentials");

    // write new crendials to GITHUB_OUTPUT (encrypted)
    // so that other steps/jobs could reuse them
    encrypt_credentials_to_output(&args.key, credentials)?;
    Ok(())
}
