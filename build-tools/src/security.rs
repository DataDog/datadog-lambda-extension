use std::fs::File;
use std::io::Result;
use std::io::Write;

use aes_gcm::{Nonce, Aes256Gcm, KeyInit, aead::Aead};

pub fn decrypt_env_var(key: &str, env_name: &str) -> String {
    let value = std::env::var(env_name).expect("could not find env var");
    let vec: Vec<_> = value
    .split('|')
    .map(|str| str.parse::<u8>().expect("could not read env var"))
    .collect();
    let nonce = build_nounce();
    let nonce = Nonce::from_slice(nonce.as_ref());
    let cipher = Aes256Gcm::new_from_slice(key.as_bytes())
        .expect("could not create a key");
    let plaintext = cipher.decrypt(nonce, vec.as_ref()).unwrap();
    let plaintext = std::str::from_utf8(&plaintext).expect("could not decrypt the env variable");
    String::from(plaintext)
}

pub fn encrypt_to_ouput(key: &str, file: &mut File, env_name: &str, data: Option<&str>) -> Result<()> {
    let nonce = build_nounce();
    let nonce = Nonce::from_slice(nonce.as_ref());
    let cipher = Aes256Gcm::new_from_slice(key.as_bytes())
        .expect("could not create a key");
    let aws_access_key_id = data.expect("could not find data");
    let ciphertext = cipher
        .encrypt(nonce, aws_access_key_id.as_bytes())
        .expect("could not create the ciphertext");
    writeln!(file, "{}", friendly_env_cipher(&env_name, &ciphertext))?;
    Ok(())
}

fn build_nounce() -> String {
    std::env::var("GITHUB_RUN_ID").expect("could not find run id")
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