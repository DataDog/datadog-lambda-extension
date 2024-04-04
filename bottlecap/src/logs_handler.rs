use lambda_extension::{Error, LambdaLog, LambdaLogRecord};

pub async fn logs_handler(logs: Vec<LambdaLog>) -> Result<(), Error> {
    for log in logs {
        match log.record {
            LambdaLogRecord::Function(_record) => {
                // do something with the function log record
            }
            LambdaLogRecord::Extension(_record) => {
                // do something with the extension log record
            }
            _ => (),
        }
    }

    Ok(())
}
