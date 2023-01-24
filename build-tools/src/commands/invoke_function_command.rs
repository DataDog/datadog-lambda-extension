use std::{io::Result, thread, time::Duration};
use structopt::StructOpt;

use aws_sdk_lambda as lambda;

#[derive(Debug, StructOpt)]
pub struct InvokeFunctionOptions {
    #[structopt(long)]
    pub region: String,
    #[structopt(long)]
    pub arn: String,
    #[structopt(long)]
    pub nb: i32,
    #[structopt(long)]
    pub remove_after: bool,
}

pub async fn invoke_function(args: &InvokeFunctionOptions) -> Result<()> {
    std::env::set_var("AWS_REGION", &args.region);
    let config = aws_config::load_from_env().await;
    let lambda_client = lambda::Client::new(&config);
    let mut timeout = 15;
    for _ in 0..args.nb {
        for _ in 0..2 {
            // doing thiw twice to make sure metrics are flushed
            lambda_client
                .invoke()
                .set_function_name(Some(args.arn.clone()))
                .send()
                .await
                .expect("could not invoke the function");
        }
        // update some configuration to make sure next invocation will be a cold start
        lambda_client
            .update_function_configuration()
            .set_timeout(Some(timeout))
            .set_function_name(Some(args.arn.clone()))
            .send()
            .await
            .expect("could not update the function");
        timeout += 1;
        thread::sleep(Duration::from_secs(5));
    }
    if args.remove_after {
        remove_function(&lambda_client, args.arn.clone()).await;
    }
    Ok(())
}

async fn remove_function(client: &lambda::Client, arn: String) {
    client
        .delete_function()
        .set_function_name(Some(arn))
        .send()
        .await
        .expect("could not remove the function");
}
