use clap::Parser;
use commands::{
    build_command::build, deploy_command::deploy, deploy_function_command::deploy_function,
    invoke_function_command::invoke_function, list_region_command::list_region, sign_command::sign,
};
use std::io::Result;

mod commands;

#[derive(Debug, Parser)]
enum SubCommand {
    #[command(name = "build", about = "Build extension")]
    Build(commands::build_command::BuildOptions),
    #[command(name = "deploy", about = "Deploy to AWS")]
    Deploy(commands::deploy_command::DeployOptions),
    #[command(name = "sign", about = "Sign Layer")]
    Sign(commands::sign_command::SignOptions),
    #[command(name = "list_region", about = "List AWS Region")]
    ListRegion(commands::list_region_command::ListRegionOptions),
    #[command(name = "deploy_lambda", about = "Deploy AWS Lambda Function")]
    DeployLambdaFunction(commands::deploy_function_command::DeployFunctionOptions),
    #[command(name = "invoke_lambda", about = "Invoke AWS Lambda Function")]
    InvokeLambdaFunction(commands::invoke_function_command::InvokeFunctionOptions),
}

#[derive(Debug, Parser)]
#[command(
    name = "build-tools",
    author = "Team Serverless",
    about = "build-tools - Let's you build and release layers"
)]
struct BuildTools {
    #[command(subcommand)]
    cmd: SubCommand,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = BuildTools::parse();
    match args.cmd {
        SubCommand::Build(opt) => build(&opt),
        SubCommand::Deploy(opt) => deploy(&opt).await,
        SubCommand::Sign(opt) => sign(&opt).await,
        SubCommand::ListRegion(opt) => list_region(&opt).await,
        SubCommand::DeployLambdaFunction(opt) => deploy_function(&opt).await,
        SubCommand::InvokeLambdaFunction(opt) => invoke_function(&opt).await,
    }
}
