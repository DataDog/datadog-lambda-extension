use structopt::clap::arg_enum;

arg_enum! {
    #[derive(Debug)]
    pub enum BuildArchitecture {
        Arm64,
        Amd64
    }
}