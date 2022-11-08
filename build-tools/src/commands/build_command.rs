use structopt::StructOpt;




use std::env;
use std::io::Result;

use std::process::Command;



#[derive(Debug,StructOpt)]
pub struct BuildOptions {
    #[structopt(long)]
    sandbox: bool,
    #[structopt(long)]
    release_candidate: bool,
    #[structopt(long, default_value = "amd64")]
    architecture: String,
    #[structopt(long, default_value = "")]
    agent_version: String,
    #[structopt(long, default_value = "1")]
    suffix: String,
    #[structopt(long, default_value = "")]
    version: String,
    #[structopt(long, default_value = ".")]
    context_path: String,
    #[structopt(long, default_value = ".")]
    destination_path: String,
    #[structopt(long)]
    cloudrun: bool,
}

pub fn build(args: &BuildOptions) -> Result<()> {
    validate_args(&args);
    match args.cloudrun {
        true => build_cloud_run(),
        false => build_extension(&args),
    }
}

fn validate_args(args: &BuildOptions) {
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

fn build_extension(args: &BuildOptions) -> Result<()> {
    let github_workspace = env::var("GITHUB_WORKSPACE").expect("could not find GITHUB_WORKSPACE env var");
    let destination_path = &args.destination_path;
    let dockerfile_path = format!("{}/scripts/Dockerfile.build", github_workspace);
    let image_name = build_image(args, "cmd/serverless", dockerfile_path.as_str())?;
    println!("image built");
    let docker_container_id = create_container(image_name.as_str())?;
    println!("container created");
    std::fs::create_dir(destination_path)?;
    println!("folder created at : {}", destination_path);
    copy_zip_file(docker_container_id.as_str(), destination_path)?;
    println!("zip copied");
    remove_container(&docker_container_id)?;
    println!("container removed");
    Ok(())
}

fn build_image(args: &BuildOptions, cmd_path: &str, dockerfile_path: &str) -> Result<String> {
    env::set_var("DOCKER_BUILDKIT", "1");
    let docker_architecture = format!("linux/{}", args.architecture);
    let docker_image_name = format!("datadog/build-lambda-extension-adm64:{}", args.version);
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