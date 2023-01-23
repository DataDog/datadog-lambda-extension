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
