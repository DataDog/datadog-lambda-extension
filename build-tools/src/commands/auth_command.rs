use std::fs::File;
use std::io::Result;
use std::io::Write;
use structopt::StructOpt;

use aws_sdk_sts as sts;

use aes_gcm::{
    aead::{Aead, KeyInit},
    Aes256Gcm, Nonce,
};
use sts::model::Credentials;

#[derive(Debug, StructOpt)]
pub struct AuthOptions {
    #[structopt(long)]
    mfa_arn: String,
    #[structopt(long)]
    mfa_code: String,
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

    // write new credentials to ENV var
    // this is needed as MFA will have expired after the build process
    write_credentials_to_env_var(credentials)?;

    // write new crendials to OUTPUT
    // this is needed as matrix job will run on an other machine
    // need to be encrypted
    encrypt_credentials_to_output(credentials)?;
    Ok(())
}

fn write_credentials_to_env_var(credentials: &Credentials) -> Result<()> {
    let github_env_file = std::env::var("GITHUB_ENV").expect("could not find GITHUB_ENV file");
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .append(true)
        .open(github_env_file)
        .expect("could not open GITHUB_ENV file");

    write_to_env(&mut file, "AWS_ACCESS_KEY_ID", credentials.access_key_id())?;
    write_to_env(
        &mut file,
        "AWS_SECRET_ACCESS_KEY",
        credentials.secret_access_key(),
    )?;
    write_to_env(&mut file, "AWS_SESSION_TOKEN", credentials.session_token())?;
    Ok(())
}

fn write_to_env(file: &mut File, prefix: &str, data: Option<&str>) -> Result<()> {
    let data = data.expect("could not write env");
    writeln!(file,"{}={}",prefix,data)?;
    Ok(())
}

fn encrypt_credentials_to_output(credentials: &Credentials) -> Result<()> {
    let github_env_file =
        std::env::var("GITHUB_OUTPUT").expect("could not find GITHUB_OUTPUT file");
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .append(true)
        .open(github_env_file)
        .expect("could not open GITHUB_OUTPUT file");
    encrypt_to_ouput(&mut file, "AWS_ACCESS_KEY_ID=", credentials.access_key_id())?;
    Ok(())
}

fn encrypt_to_ouput(file: &mut File, prefix: &str, data: Option<&str>) -> Result<()> {
    let cipher = Aes256Gcm::new_from_slice("oooooooooooooooooooooooooooooooo".as_bytes())
        .expect("could not create a key");
    let nonce = Nonce::from_slice(b"unique nonce");
    let aws_access_key_id = data.expect("could not find data");
    let ciphertext = cipher
        .encrypt(nonce, aws_access_key_id.as_bytes())
        .expect("could not create the ciphertext");
    let mut buffer_modified = Vec::new();
    for value in prefix.as_bytes().iter().copied() {
        buffer_modified.push(value);
    }
    for value in ciphertext {
        buffer_modified.push(value);
    }
    file.write_all(buffer_modified.as_slice())?;
    Ok(())
}
