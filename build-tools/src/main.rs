use commands::{build_command::build, deploy_command::deploy, auth_command::auth};
use structopt::StructOpt;

use std::io::Result;

mod commands;

#[derive(Debug,StructOpt)]
enum SubCommand {
    #[structopt(name = "build", about = "Build extension")]
    Build(commands::build_command::BuildOptions),
    #[structopt(name = "auth", about = "Auth to AWS")]
    Auth(commands::auth_command::AuthOptions),
    #[structopt(name = "deploy", about = "Deploy to AWS")]
    Deploy(commands::deploy_command::DeployOptions)
}

#[derive(Debug, StructOpt)]
#[structopt(name = "build-tools", author = "Team Serverless", about = "build-tools - Let's you build and release layers")]
struct CLI {
    #[structopt(subcommand)]
    cmd: SubCommand
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = CLI::from_args();
    match args.cmd {
        SubCommand::Auth(opt) => {
            auth(&opt).await
        }
        SubCommand::Build(opt) => {
            build(&opt)
        }
        SubCommand::Deploy(opt) => {
            deploy(&opt).await
        }
    }
}