use lambda::model::{Environment, FunctionCode, Runtime};
use std::collections::HashMap;
use std::io::Result;
use std::io::{Error, ErrorKind};
use std::time::SystemTime;
use structopt::StructOpt;

use aws_sdk_lambda as lambda;

use super::common::get_file_as_vec;

struct RuntimeConfig {
    handler: String,
    dd_handler: String,
    package_path: String,
    runtime: Runtime,
}

impl RuntimeConfig {
    fn new(handler: String, dd_handler: String, package_path: String, runtime: Runtime) -> Self {
        RuntimeConfig {
            handler,
            dd_handler,
            package_path,
            runtime,
        }
    }
    fn generate_function_name(&self) -> String {
        let current_timestamp = SystemTime::now()
            .elapsed()
            .expect("could not get the current timestamp");
        format!(
            "serverless-perf-test-{}-{}",
            self.runtime.as_str(),
            current_timestamp.as_secs()
        )
    }
}

#[derive(Debug, StructOpt)]
pub struct DeployFunctionOptions {
    #[structopt(long)]
    layer_name: String,
    #[structopt(long)]
    runtime: String,
    #[structopt(long)]
    region: String,
    #[structopt(long)]
    role: String,
    #[structopt(long)]
    extension_arn: String,
    #[structopt(long)]
    api_key: String,
}

pub async fn deploy_function(args: &DeployFunctionOptions) -> Result<()> {
    std::env::set_var("AWS_REGION", &args.region);
    let config = aws_config::load_from_env().await;
    let lambda_client = lambda::Client::new(&config);
    let layer_arn = get_latest_arn(&lambda_client, &args.layer_name).await?;
    create_function(&lambda_client, &args, layer_arn).await?;
    Ok(())
}

async fn create_function(
    client: &lambda::Client,
    args: &DeployFunctionOptions,
    layer_arn: String,
) -> Result<()> {
    let runtime_config = get_config_from_runtime(&args.runtime)?;
    let function_name = runtime_config.generate_function_name();
    let mut env_map = HashMap::new();
    env_map.insert(String::from("DD_LAMBDA_HANDLER"), runtime_config.dd_handler);
    env_map.insert(String::from("DD_API_KEY"), args.api_key.clone());
    let environment = Environment::builder().set_variables(Some(env_map)).build();

    let lambda_blob = get_file_as_vec(&runtime_config.package_path);
    let function_code = FunctionCode::builder()
        .set_zip_file(Some(lambda_blob))
        .build();

    client
        .create_function()
        .set_role(Some(args.role.clone()))
        .set_function_name(Some(function_name))
        .set_code(Some(function_code))
        .set_runtime(Some(runtime_config.runtime))
        .set_handler(Some(runtime_config.handler))
        .set_layers(Some(vec![layer_arn, args.extension_arn.clone()]))
        .set_environment(Some(environment))
        .send()
        .await
        .expect("could not deploy the function");
    Ok(())
}

async fn get_latest_arn(client: &lambda::Client, layer_name: &str) -> Result<String> {
    let result = client
        .list_layer_versions()
        .set_layer_name(Some(String::from(layer_name)))
        .set_max_items(Some(1))
        .send()
        .await
        .expect("could not list layer versions");

    let layer_versions = match result.layer_versions() {
        Some(layer_versions) => layer_versions,
        None => {
            return Err(Error::new(
                ErrorKind::InvalidData,
                "could not get layer versions",
            ))
        }
    };

    let latest_version = match layer_versions.first() {
        Some(latest_version) => latest_version.layer_version_arn(),
        None => {
            return Err(Error::new(
                ErrorKind::InvalidData,
                "could not get layer versions",
            ))
        }
    };
    match latest_version {
        Some(version) => Ok(version.to_string()),
        None => {
            return Err(Error::new(
                ErrorKind::InvalidData,
                "could not get layer versions",
            ))
        }
    }
}

fn get_config_from_runtime(runtime: &str) -> Result<RuntimeConfig> {
    match runtime {
        "nodejs14.x" => Ok(RuntimeConfig::new(
            String::from("app.handler"),
            String::from("/opt/nodejs/node_modules/datadog-lambda-js/handler.handler"),
            String::from("functions/nodejs.zip"),
            lambda::model::Runtime::Nodejs14x,
        )),
        "nodejs16.x" => Ok(RuntimeConfig::new(
            String::from("app.handler"),
            String::from("/opt/nodejs/node_modules/datadog-lambda-js/handler.handler"),
            String::from("functions/nodejs.zip"),
            lambda::model::Runtime::Nodejs16x,
        )),
        "nodejs18.x" => Ok(RuntimeConfig::new(
            String::from("app.handler"),
            String::from("/opt/nodejs/node_modules/datadog-lambda-js/handler.handler"),
            String::from("functions/nodejs.zip"),
            lambda::model::Runtime::Nodejs18x,
        )),
        _ => Err(Error::new(ErrorKind::InvalidData, "invalid runtime")),
    }
}
