use lambda::model::{Environment, FunctionCode, Runtime};
use serde::Deserialize;
use std::collections::HashMap;
use std::io::Result;
use std::io::{Error, ErrorKind};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use structopt::StructOpt;

use aws_sdk_lambda as lambda;

use super::common::get_file_as_vec;
use super::invoke_function_command::{self, InvokeFunctionOptions};

const LATEST_RELEASE: &str =
    "https://api.github.com/repos/datadog/datadog-lambda-extension/releases/latest";
const FALLBACK_LATEST_EXTESION_VERSION: i32 = 36;

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
            .duration_since(UNIX_EPOCH)
            .expect("could not get the current timestamp");
        format!(
            "serverless-perf-test-{}-{}",
            self.sanitized_runtime(),
            current_timestamp.as_secs()
        )
    }
    fn sanitized_runtime(&self) -> String {
        self.runtime.as_str().replace('.', "")
    }
}

#[derive(Deserialize)]
struct GithubRelease {
    tag_name: String,
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
    pr_id: String,
    #[structopt(long)]
    should_invoke: bool,
}

pub async fn deploy_function(args: DeployFunctionOptions) -> Result<()> {
    let api_key = std::env::var("DD_API_KEY").expect("could not find DD_API_KEY env");
    std::env::set_var("AWS_REGION", &args.region);
    let config = aws_config::load_from_env().await;
    let lambda_client = lambda::Client::new(&config);
    let layer_arn = get_latest_arn(&lambda_client, &args.layer_name).await?;
    let deployed_arn = create_function(&lambda_client, &args, layer_arn, api_key).await?;
    if args.should_invoke {
        thread::sleep(Duration::from_secs(15));
        let invoke_args = InvokeFunctionOptions {
            arn: deployed_arn,
            region: args.region.clone(),
            nb: 5, // to make sure enhanced metrics are flushed
            remove_after: true,
        };
        invoke_function_command::invoke_function(invoke_args).await?;
    }
    Ok(())
}

async fn create_function(
    client: &lambda::Client,
    args: &DeployFunctionOptions,
    layer_arn: String,
    api_key: String,
) -> Result<String> {
    let runtime_config = get_config_from_runtime(&args.runtime)?;
    let sanitized_runtime = runtime_config.sanitized_runtime();
    let function_name = runtime_config.generate_function_name();
    let mut env_map = HashMap::new();
    env_map.insert(String::from("DD_LAMBDA_HANDLER"), runtime_config.dd_handler);
    env_map.insert(String::from("DD_API_KEY"), api_key);
    env_map.insert(String::from("DD_SERVICE"), String::from("serverless-perf"));
    env_map.insert(
        String::from("DD_TAGS"),
        format!("runtime:{},pr:{}", sanitized_runtime, args.pr_id),
    );
    let environment = Environment::builder().set_variables(Some(env_map)).build();

    let lambda_blob = get_file_as_vec(&runtime_config.package_path);
    let function_code = FunctionCode::builder()
        .set_zip_file(Some(lambda_blob))
        .build();

    let result = client
        .create_function()
        .set_role(Some(args.role.clone()))
        .set_function_name(Some(function_name))
        .set_code(Some(function_code))
        .set_runtime(Some(runtime_config.runtime))
        .set_handler(Some(runtime_config.handler))
        .set_layers(Some(vec![
            layer_arn,
            get_latest_extension_arn(&args.region),
        ]))
        .set_environment(Some(environment))
        .send()
        .await
        .expect("could not deploy the function");
    let function_arn = result.function_arn().expect("could not find the arn");
    Ok(function_arn.to_string())
}

async fn get_latest_arn(client: &lambda::Client, layer_name: &str) -> Result<String> {
    let layer_name = layer_name.replace('.', ""); // layers cannot contain dots
    let result = client
        .list_layer_versions()
        .set_layer_name(Some(layer_name))
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
        None => Err(Error::new(
            ErrorKind::InvalidData,
            "could not get layer versions",
        )),
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

fn get_latest_extension_arn(region: &str) -> String {
    let version = fetch_extension_version_from_github();
    format!(
        "arn:aws:lambda:{}:464622532012:layer:Datadog-Extension:{}",
        region, version
    )
}

fn fetch_extension_version_from_github() -> String {
    let client = reqwest::blocking::Client::new();
    let result = match client.post(LATEST_RELEASE).send() {
        Ok(result) => result,
        Err(_) => return FALLBACK_LATEST_EXTESION_VERSION.to_string(),
    };
    match result.json::<GithubRelease>() {
        Ok(result) => result.tag_name.replace(['.', 'v'], "").trim().to_string(),
        Err(_) => FALLBACK_LATEST_EXTESION_VERSION.to_string(),
    }
}
