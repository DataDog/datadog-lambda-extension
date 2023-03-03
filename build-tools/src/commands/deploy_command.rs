use std::io::Result;
use structopt::StructOpt;

use aws_sdk_lambda as lambda;

use crate::security::build_config;

use super::common::get_file_as_vec;
use super::common::BuildArchitecture;

#[derive(Debug, StructOpt)]
pub struct DeployOptions {
    #[structopt(long)]
    layer_path: String,
    #[structopt(long)]
    layer_name: String,
    #[structopt(long, possible_values = &BuildArchitecture::variants(), case_insensitive = true, default_value = "amd64")]
    architecture: BuildArchitecture,
    #[structopt(long)]
    layer_suffix: Option<String>,
    #[structopt(long)]
    key: Option<String>,
    #[structopt(long, default_value = "sa-east-1")]
    region: String,
    #[structopt(long)]
    description: String,
}

pub async fn deploy(args: &DeployOptions) -> Result<()> {
    let config = build_config(&args.key, &args.region).await;
    let lambda_client = lambda::Client::new(&config);

    // build the content object
    let content = aws_sdk_lambda::model::LayerVersionContentInput::builder();
    let lambda_blob = get_file_as_vec(&args.layer_path);

    let layer_name = build_layer_name(&args.layer_name, &args.architecture, &args.layer_suffix);

    let description = String::from(&args.description);
    // publish layer
    let builder = lambda_client.publish_layer_version();
    builder
        .set_layer_name(Some(layer_name))
        .set_content(Some(content.set_zip_file(Some(lambda_blob)).build()))
        .set_description(Some(description))
        .send()
        .await
        .expect("error while publishing the layer");
    Ok(())
}

fn build_layer_name(
    layer_name: &str,
    architecture: &BuildArchitecture,
    layer_suffix: &Option<String>,
) -> String {
    let layer_with_suffix = if let Some(suffix) = layer_suffix {
        match suffix.len() {
            0 => String::from(layer_name),
            _ => String::from(layer_name) + "-" + suffix,
        }
    } else {
        String::from(layer_name)
    };
    let layer = match architecture {
        BuildArchitecture::Amd64 => layer_with_suffix,
        BuildArchitecture::Arm64 => layer_with_suffix + "-ARM",
    };
    layer.replace('.', "") //layer cannot contain dots
}

#[cfg(test)]
mod tests {

    use super::*;

    #[test]
    fn build_layer_name_test() {
        //ARM64
        assert_eq!(
            "layer-suffix-ARM",
            build_layer_name(
                "layer",
                &BuildArchitecture::Arm64,
                &Some("suffix".to_string())
            )
        );
        assert_eq!(
            "layer-ARM",
            build_layer_name("layer", &BuildArchitecture::Arm64, &Some("".to_string()))
        );
        assert_eq!(
            "layer-ARM",
            build_layer_name("layer", &BuildArchitecture::Arm64, &None)
        );
        //AMD64
        assert_eq!(
            "layer-suffix",
            build_layer_name(
                "layer",
                &BuildArchitecture::Amd64,
                &Some("suffix".to_string())
            )
        );
        assert_eq!(
            "layer",
            build_layer_name("layer", &BuildArchitecture::Amd64, &Some("".to_string()))
        );
        assert_eq!(
            "layer",
            build_layer_name("layer", &BuildArchitecture::Amd64, &None)
        );
    }
}
