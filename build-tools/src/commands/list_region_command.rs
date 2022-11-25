use std::io::Result;
use std::io::Write;
use std::io::{Error, ErrorKind};

use aws_sdk_ec2 as ec2;

pub async fn list_region() -> Result<()> {
    // set a random AWS_REGION as ec2:DescribeRgions in region-agnostic
    std::env::set_var("AWS_REGION", "us-east-1");

    println!("AWS_ACCESS_KEY_ID={:?}", std::env::var("AWS_ACCESS_KEY_ID"));
    println!("AWS_SECRET_ACCESS_KEY={:?}", std::env::var("AWS_SECRET_ACCESS_KEY"));
    println!("AWS_SESSION_TOKEN={:?}", std::env::var("AWS_SESSION_TOKEN"));


    let config = aws_config::load_from_env().await;
    let ec2_client = ec2::Client::new(&config);
    let regions = get_list(&ec2_client).await?;
    write_region(&regions)?;
    Ok(())
}

async fn get_list(ec2_client: &ec2::Client) -> Result<Vec<String>> {
    let result = ec2_client
        .describe_regions()
        .send()
        .await
        .expect("could not list regions");

    if let Some(regions) = result.regions() {
        let result: Vec<_> = regions.iter().map(|region| region.region_name()).collect();

        let result = result
            .iter()
            .flatten()
            .map(|region_name| String::from(*region_name))
            .collect::<Vec<_>>();
        Ok(result)
    } else {
        Err(Error::new(ErrorKind::InvalidData, "could not get regions"))
    }
}

fn write_region(regions: &Vec<String>) -> Result<()> {
    let github_env_file =
        std::env::var("GITHUB_OUTPUT").expect("could not find GITHUB_OUTPUT file");
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .append(true)
        .open(github_env_file)
        .expect("could not open GITHUB_OUTPUT file");
    writeln!(file, "AWS_REGIONS={:?}", regions)?;
    Ok(())
}
