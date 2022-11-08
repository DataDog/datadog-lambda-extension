use commands::{build_command::build, deploy_command::deploy};
use structopt::StructOpt;

use std::io::Result;

mod commands;

#[derive(Debug,StructOpt)]
enum SubCommand {
    #[structopt(name = "build", about = "Build extension")]
    Build(commands::build_command::BuildOptions),
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
        SubCommand::Build(opt) => {
            build(&opt)
        }
        SubCommand::Deploy(opt) => {
            deploy(&opt).await
        }
    }
}