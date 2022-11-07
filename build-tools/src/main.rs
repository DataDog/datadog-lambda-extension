use clap::Parser;
use std::env;
use std::io::Result;

use std::process::Command;

#[derive(Parser, Debug)]
struct Args {
    #[arg(long)]
    sandbox: bool,
    #[arg(long)]
    release_candidate: bool,
    #[arg(long, default_value_t = String::from("amd64"))]
    architecture: String,
    #[arg(long, default_value_t = String::from(""))]
    agent_version: String,
    #[arg(long, default_value_t = String::from("1"))]
    suffix: String,
    #[arg(long, default_value_t = String::from(""))]
    version: String,
    #[arg(long, default_value_t = String::from("."))]
    context_path: String,
    #[arg(long, default_value_t = String::from("."))]
    destination_path: String,
    #[arg(long)]
    cloudrun: bool,
}

fn main() -> Result<()> {
    let args = Args::parse();
    println!("sandbox = {}", args.sandbox);
    println!("architecture = {}", args.architecture);
    println!("suffix = {}", args.suffix);
    println!("release_candidate = {}", args.release_candidate);
    println!("version = {}", args.version);
    println!("agent_version = {}", args.agent_version);
    println!("cloudrun = {}", args.cloudrun);
    println!("context_path = {}", args.context_path);
    println!("destination_path = {}", args.destination_path);

    validate_args(&args);
    match args.cloudrun {
        true => build_cloud_run(),
        false => build_extension(&args),
    }
}

fn validate_args(args: &Args) {
    validate_architecture_from_string(args.architecture.as_str());
}

fn validate_architecture_from_string(arch: &str) -> bool {
    match arch {
        "arm64" => true,
        "amd64" => true,
        _ => panic!("invalid architecture"),
    }
}

fn build_cloud_run() -> Result<()> {
    panic!("not implemented yet");
}

fn build_extension(args: &Args) -> Result<()> {
    println!("building the extension");
    let github_workspace = env::var("GITHUB_WORKSPACE").expect("could not find GITHUB_WORKSPACE env var");
    let destination_path = &args.destination_path;
    let dockerfile_path = format!("{}/scripts/Dockerfile.build", github_workspace);
    let image_name = build_image(args, "cmd/serverless", dockerfile_path.as_str())?;
    let docker_container_id = create_container(image_name.as_str())?;
    copy_zip_file(docker_container_id.as_str(), destination_path)?;
    remove_container(&docker_container_id)?;
    Ok(())
}

fn build_image(args: &Args, cmd_path: &str, dockerfile_path: &str) -> Result<String> {
    env::set_var("DOCKER_BUILDKIT", "1");
    let docker_architecture = format!("linux/{}", args.architecture);
    let docker_image_name = format!("datadog/build-lambda-extension-adm64:{}", args.version);
    let extension_version_build_arg = format!("EXTENSION_VERSION={}", args.version);
    let agent_version_build_arg = format!("AGENT_VERSION={}", args.agent_version);
    let cmd_path_build_arg = format!("CMD_PATH={}", cmd_path);

    let output = Command::new("docker")
        .args([
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
        ])
        .output()?;
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
