use commands::{
    build_command::build, deploy_command::deploy, deploy_function_command::deploy_function,
    invoke_function_command::invoke_function, list_region_command::list_region, sign_command::sign,
    download_signed_command::download_signed,
};
use std::io::Result;
use structopt::StructOpt;

mod commands;

#[derive(Debug, StructOpt)]
enum SubCommand {
    #[structopt(name = "build", about = "Build extension")]
    Build(commands::build_command::BuildOptions),
    #[structopt(name = "deploy", about = "Deploy to AWS")]
    Deploy(commands::deploy_command::DeployOptions),
    #[structopt(name = "sign", about = "Sign Layer")]
    Sign(commands::sign_command::SignOptions),
    #[structopt(name = "download_signed", about = "Download Signed Layer")]
    DownloadSigned(commands::download_signed_command::DownloadSignedOptions),
    #[structopt(name = "list_region", about = "List AWS Region")]
    ListRegion(commands::list_region_command::ListRegionOptions),
    #[structopt(name = "deploy_lambda", about = "Deploy AWS Lambda Function")]
    DeployLambdaFunction(commands::deploy_function_command::DeployFunctionOptions),
    #[structopt(name = "invoke_lambda", about = "Invoke AWS Lambda Function")]
    InvokeLambdaFunction(commands::invoke_function_command::InvokeFunctionOptions),
}

#[derive(Debug, StructOpt)]
#[structopt(
    name = "build-tools",
    author = "Team Serverless",
    about = "build-tools - Let's you build and release layers"
)]
struct BuildTools {
    #[structopt(subcommand)]
    cmd: SubCommand,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = BuildTools::from_args();
    match args.cmd {
        SubCommand::Build(opt) => build(&opt),
        SubCommand::Deploy(opt) => deploy(&opt).await,
        SubCommand::Sign(opt) => sign(&opt).await,
        SubCommand::DownloadSigned(opt) => download_signed(&opt).await,
        SubCommand::ListRegion(opt) => list_region(&opt).await,
        SubCommand::DeployLambdaFunction(opt) => deploy_function(&opt).await,
        SubCommand::InvokeLambdaFunction(opt) => invoke_function(&opt).await,
    }
}
