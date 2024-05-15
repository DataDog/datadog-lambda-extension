use std::io::Error;
use crate::config::Config;
use aws_config;
use aws_sdk_secretsmanager::Client;
use aws_sdk_secretsmanager;
use tracing::debug;
use std::thread;


type DecoderFn = fn(String) -> Result<String, Error>;

pub fn resolve_secrets(config: Config, decoder: DecoderFn) -> Result<Config, String> {
    if !config.api_key.is_empty() {
        debug!("DD_API_KEY found, not trying to resolve secrets");
        Ok(config)
    } else {
        if !config.api_key_secret_arn.is_empty() {
            debug!("DD_API_KEY_SECRET_ARN found, trying to resolve ARN secret");
            let string = config.api_key_secret_arn.clone();
            let wait_fot_ret = thread::spawn(move || { decoder(string) });
            match wait_fot_ret.join() {
                Ok(Ok(secret)) => {
                    Ok(Config {
                        api_key: secret,
                        ..config.clone()
                    })
                }
                Ok(Err(e)) => { Err(e.to_string()) }

                Err(e) => {
                    if let Some(s) = e.downcast_ref::<&str>() {
                        Err(format!("Thread panicked with message: {}", s))
                    } else if let Some(s) = e.downcast_ref::<String>() {
                        Err(format!("Thread panicked with message: {}", s))
                    } else {
                        Err("Thread panicked".to_string())
                    }
                }
            }
        } else {
            Err("No API key or secret ARN found".to_string())
        }
    }
}

pub fn decrypt_secret_arn(secret_arn: String) -> Result<String, Error> {
    let this_thread_runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let config = this_thread_runtime
        .block_on(aws_config::load_defaults(aws_config::BehaviorVersion::latest()));

    let client = Client::new(&config);
    let resp = this_thread_runtime
        .block_on(client.get_secret_value().secret_id(secret_arn).send());
    match resp {
        Ok(secret_output) => {
            match secret_output.secret_string {
                Some(secret) => Ok(secret),
                None => Err(Error::new(std::io::ErrorKind::Other, "No secret found"))
            }
        }
        Err(_) => Err(Error::new(std::io::ErrorKind::Other, "No secret found"))
    }
}


#[cfg(test)]
mod tests {
    use super::*;

    fn immediate_return(secret_arn: String) -> Result<String, Error> {
        Ok(secret_arn)
    }

    #[test]
    fn test_resolve_secrets_sync() {
        let secret_arn = "arn:aws:secretsmanager:region:account-id:secret:secret-name".to_string();

        let result = match resolve_secrets(
            Config {
                api_key_secret_arn: secret_arn.clone(),
                ..Config::default()
            }, immediate_return) {
            Ok(config) => config.api_key,
            Err(e) => panic!("{}", e)
        };

        assert_eq!(result, secret_arn);
    }
}