use structopt::StructOpt;
use std::io::Write;
use std::{io::Result};

use aws_sdk_sts as sts;

#[derive(Debug,StructOpt)]
pub struct AuthOptions {
    #[structopt(long)]
    mfa_arn: String,
    #[structopt(long)]
    mfa_code: String,
}

pub async fn auth(args: &AuthOptions)-> Result<()> {
    std::env::set_var("AWS_REGION", "us-east-1");
    let config = aws_config::load_from_env().await;

    // get AWS credentials from MFA
    let sts_client = sts::Client::new(&config);
    let command = sts_client.get_session_token();
    let sts_response = command
    .set_serial_number(Some(args.mfa_arn.clone()))
    .set_token_code(Some(args.mfa_code.clone()))
    .send().await.expect("could not call sts");

    let credentials = sts_response.credentials().expect("could not load credentials");
    
    let github_env_file = std::env::var("GITHUB_ENV").expect("could not find GITHUB_ENV file");
    
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .append(true)
        .open(github_env_file)
        .expect("could not open GITHUB_ENV file");

    writeln!(file, "AWS_ACCESS_KEY_ID={}", credentials.access_key_id().expect("could not find access key"))?;
    writeln!(file, "AWS_SECRET_ACCESS_KEY={}", credentials.secret_access_key().expect("could not find secret access key"))?;
    writeln!(file, "AWS_SESSION_TOKEN={}", credentials.session_token().expect("could not find session token"))?;

    Ok(())
}