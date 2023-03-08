use std::env;
use std::io::Result;
use std::process::Command;
use structopt::StructOpt;

use aws_sdk_sts as sts;
use aws_sdk_sts::model::Credentials;

#[derive(Debug, StructOpt)]
pub struct AssumeRoleOptions {
    #[structopt(long)]
    role: String,
    #[structopt(long)]
    external_id: String,
}
