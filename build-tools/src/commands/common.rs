use std::{fs::File, io::Read};

use aws_sdk_ec2::types::Blob;
use structopt::clap::arg_enum;

arg_enum! {
    #[derive(Debug)]
    pub enum BuildArchitecture {
        Arm64,
        Amd64
    }
}

pub fn get_file_as_vec(filename: &String) -> Blob {
    let mut f = File::open(filename).expect("could not find the zip");
    let metadata = std::fs::metadata(filename).expect("unable to read metadata");
    let mut buffer = vec![0; metadata.len() as usize];
    f.read_exact(&mut buffer).expect("buffer error");
    Blob::new(buffer)
}

pub fn build_layer_name(
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
