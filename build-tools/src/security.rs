use std::fs::File;
use std::io::Result;
use std::io::Write;

use aes_gcm::{Nonce, Aes256Gcm, KeyInit, aead::Aead};
use aws_config::SdkConfig;
use aws_sdk_sts::model::Credentials;

pub fn decrypt_env_var(key: &str, env_name: &str) -> String {
    let value = std::env::var(env_name).expect("could not find env var");
    let vec: Vec<_> = value
    .split('|')
    .map(|str| str.parse::<u8>().expect("could not read env var"))
    .collect();
    let nonce = build_nonce();
    let nonce = Nonce::from_slice(nonce.as_ref());
    let cipher = Aes256Gcm::new_from_slice(key.as_bytes())
        .expect("could not create a key");
    let plaintext = cipher.decrypt(nonce, vec.as_ref()).unwrap();
    let plaintext = std::str::from_utf8(&plaintext).expect("could not decrypt the env variable");
    String::from(plaintext)
}

pub fn encrypt_credentials_to_output(key: &str, credentials: &Credentials) -> Result<()> {
    println!("encrypt_credentials_to_output");
    let github_env_file =
        std::env::var("GITHUB_OUTPUT").expect("could not find GITHUB_OUTPUT file");
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .append(true)
        .open(github_env_file)
        .expect("could not open GITHUB_OUTPUT file");
    encrypt_to_ouput(&key, &mut file, "AWS_ACCESS_KEY_ID", credentials.access_key_id())?;
    encrypt_to_ouput(&key, &mut file, "AWS_SECRET_ACCESS_KEY", credentials.secret_access_key())?;
    encrypt_to_ouput(&key, &mut file, "AWS_SESSION_TOKEN", credentials.session_token())?;
    Ok(())
}

pub async fn build_config(key: &str, region: &str) -> SdkConfig{
    std::env::set_var("AWS_ACCESS_KEY_ID", decrypt_env_var(&key, "AWS_ACCESS_KEY_ID"));
    std::env::set_var("AWS_SECRET_ACCESS_KEY", decrypt_env_var(&key, "AWS_SECRET_ACCESS_KEY"));
    std::env::set_var("AWS_SESSION_TOKEN", decrypt_env_var(&key, "AWS_SESSION_TOKEN"));
    std::env::set_var("AWS_REGION", region);
    aws_config::load_from_env().await
}

fn encrypt_to_ouput(key: &str, file: &mut File, env_name: &str, data: Option<&str>) -> Result<()> {
    println!("encrypt_to_ouput");
    let nonce = build_nonce();
    println!("after build_nonce {}", nonce);
    let nonce = Nonce::from_slice(nonce.as_ref());
    println!("after build_nonce 2 {:?}", nonce);
    let cipher = Aes256Gcm::new_from_slice(key.as_bytes())
        .expect("could not create a key");
    println!("after cipher");
    let aws_access_key_id = data.expect("could not find data");
    let ciphertext = cipher
        .encrypt(nonce, aws_access_key_id.as_bytes())
        .expect("could not create the ciphertext");
    println!("AAA");
    writeln!(file, "{}", friendly_env_cipher(&env_name, &ciphertext))?;
    println!("BBB");
    Ok(())
}

fn build_nonce() -> String {
    //need extactly 12 chars (10 with RUN_ID + 2)
    std::env::var("GITHUB_RUN_ID").expect("could not find run id").to_string() + &"XX"
}

fn friendly_env_cipher(env_name: &str, ciphertext: &Vec<u8>) -> String {
    let mut final_env = String::new();
    let mut i = 0;
    for value in ciphertext {
        if i!=0 {
            final_env.push_str("|");
        }
        final_env.push_str(&value.to_string());
        i = i+1;
    }
    env_name.to_string() + &"=" + &final_env
}