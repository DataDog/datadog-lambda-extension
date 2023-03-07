use aws_config::SdkConfig;

pub async fn build_config(region: &str) -> SdkConfig {
    std::env::set_var("AWS_REGION", region);
    aws_config::load_from_env().await
}