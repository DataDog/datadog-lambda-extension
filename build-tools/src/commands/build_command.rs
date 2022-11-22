use std::env;
use std::io::Result;
use std::process::Command;
use structopt::StructOpt;

use super::common::BuildArchitecture;

#[derive(Debug, StructOpt)]
pub struct BuildOptions {
    #[structopt(long, possible_values = &BuildArchitecture::variants(), case_insensitive = true, default_value = "amd64")]
    architecture: BuildArchitecture,
    #[structopt(long, default_value = "123")]
    agent_version: String,
    #[structopt(long, default_value = "123")]
    version: String,
    #[structopt(long)]
    context_path: String,
    #[structopt(long)]
    destination_path: String,
    #[structopt(long)]
    cloudrun: bool,
}

pub fn build(args: &BuildOptions) -> Result<()> {
    match args.cloudrun {
        true => build_cloud_run(),
        false => build_extension("cmd/serverless", args),
    }
}

fn build_cloud_run() -> Result<()> {
    panic!("not implemented yet");
}

fn build_extension(cmd_path: &str, args: &BuildOptions) -> Result<()> {
    let github_workspace =
        env::var("GITHUB_WORKSPACE").expect("could not find GITHUB_WORKSPACE env var");
    let destination_path = &args.destination_path;
    let dockerfile_path = format!("{}/scripts_v2/Dockerfile.build", github_workspace);
    let image_name = build_image(args, cmd_path, dockerfile_path.as_str())?;
    let docker_container_id = create_container(image_name.as_str())?;
    std::fs::create_dir(destination_path)?;
    copy_zip_file(docker_container_id.as_str(), destination_path)?;
    remove_container(&docker_container_id)?;
    Ok(())
}

fn build_image(args: &BuildOptions, cmd_path: &str, dockerfile_path: &str) -> Result<String> {
    env::set_var("DOCKER_BUILDKIT", "1");
    let architecture = args.architecture.to_string().to_ascii_lowercase();
    let docker_architecture = format!("linux/{}", architecture);
    let docker_image_name = format!(
        "datadog/build-lambda-extension-{}:{}",
        architecture, args.version
    );
    let extension_version_build_arg = format!("EXTENSION_VERSION={}", args.version);
    let agent_version_build_arg = format!("AGENT_VERSION={}", args.agent_version);
    let cmd_path_build_arg = format!("CMD_PATH={}", cmd_path);

    let docker_args = [
        "buildx",
        "build",
        "--platform",
        docker_architecture.as_str(),
        "-f",
        dockerfile_path,
        "-t",
        docker_image_name.as_str(),
        "--build-arg",
        extension_version_build_arg.as_str(),
        "--build-arg",
        agent_version_build_arg.as_str(),
        "--build-arg",
        cmd_path_build_arg.as_str(),
        args.context_path.as_str(),
        "--load",
    ];

    let output = Command::new("docker").args(docker_args).output()?;
    let string_output = std::str::from_utf8(&output.stderr);
    println!("{}", string_output.expect("could not read stderr"));

    match output.status.success() {
        true => Ok(docker_image_name),
        false => panic!("could not build the image"),
    }
}

fn create_container(image_name: &str) -> Result<String> {
    let output = Command::new("docker")
        .args(["create", image_name])
        .output()?;
    let docker_id = std::str::from_utf8(&output.stdout).expect("could not find docker_id");
    match output.status.success() {
        true => Ok(String::from(docker_id.trim())),
        false => panic!("could not run the image"),
    }
}

fn copy_zip_file(container_id: &str, destination_path: &str) -> Result<()> {
    let source = format!("{}:/datadog_extension.zip", container_id);
    let output = Command::new("docker")
        .args(["cp", source.as_str(), destination_path])
        .output()?;
    match output.status.success() {
        true => Ok(()),
        false => panic!("could not copy the zip file from the container"),
    }
}

fn remove_container(container_id: &str) -> Result<()> {
    let output = Command::new("docker").args(["rm", container_id]).output()?;
    match output.status.success() {
        true => Ok(()),
        false => panic!("could not stop the container"),
    }
}
