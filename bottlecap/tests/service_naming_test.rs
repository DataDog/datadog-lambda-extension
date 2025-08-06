use std::collections::HashMap;

use bottlecap::lifecycle::invocation::triggers::{
    Trigger, dynamodb_event::DynamoDbRecord, kinesis_event::KinesisRecord,
    lambda_function_url_event::LambdaFunctionUrlEvent, msk_event::MSKEvent, s3_event::S3Record,
    sns_event::SnsRecord, sqs_event::SqsRecord,
};

use bottlecap::lifecycle::invocation::triggers::ServiceNameResolver;

/// Small helper for integration tests: loads a payload JSON file from
/// `bottlecap/tests/payloads/<file_name>` and returns its content as `String`.
fn read_json_file(file_name: &str) -> String {
    use std::fs;
    use std::path::PathBuf;

    // `CARGO_MANIFEST_DIR` points at the `bottlecap` crate root when this
    // integration test is compiled.
    let mut path = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    path.push("tests/payloads");
    path.push(file_name);
    fs::read_to_string(path).expect("Failed to read test payload file")
}

#[test]
#[serial]
fn test_dynamodb_service_name_instance_fallback() {
    unsafe { std::env::remove_var("DD_TRACE_AWS_SERVICE_REPRESENTATION_ENABLED") };
    let json = read_json_file("dynamodb_event.json");
    let payload = serde_json::from_str(&json).expect("Failed to deserialize");
    let event = DynamoDbRecord::new(payload).expect("deserialize DynamoDbRecord");
    let table_name = event.get_specific_identifier();
    let service = event.resolve_service_name(&HashMap::new(), &table_name, "dynamodb");
    assert_eq!(service, "ExampleTableWithStream");
}

#[test]
#[serial]
fn test_s3_service_name_instance_fallback() {
    unsafe { std::env::remove_var("DD_TRACE_AWS_SERVICE_REPRESENTATION_ENABLED") };
    let json = read_json_file("s3_event.json");
    let payload = serde_json::from_str(&json).expect("Failed to deserialize");
    let event = S3Record::new(payload).expect("deserialize S3Record");
    let bucket_name = event.get_specific_identifier();
    let service = event.resolve_service_name(&HashMap::new(), &bucket_name, "s3");
    assert_eq!(service, "example-bucket");
}

#[test]
#[serial]
fn test_sqs_service_name_instance_fallback() {
    unsafe { std::env::remove_var("DD_TRACE_AWS_SERVICE_REPRESENTATION_ENABLED") };
    let json = read_json_file("sqs_event.json");
    let payload = serde_json::from_str(&json).expect("Failed to deserialize");
    let event = SqsRecord::new(payload).expect("deserialize SqsRecord");
    let queue_name = event.get_specific_identifier();
    let service = event.resolve_service_name(&HashMap::new(), &queue_name, "sqs");
    assert_eq!(service, "MyQueue");
}

#[test]
#[serial]
fn test_kinesis_service_name_instance_fallback() {
    unsafe { std::env::remove_var("DD_TRACE_AWS_SERVICE_REPRESENTATION_ENABLED") };
    let json = read_json_file("kinesis_event.json");
    let payload = serde_json::from_str(&json).expect("Failed to deserialize");
    let event = KinesisRecord::new(payload).expect("deserialize KinesisRecord");
    let stream_name = event.get_specific_identifier();
    let service = event.resolve_service_name(&HashMap::new(), &stream_name, "kinesis");
    assert_eq!(service, "kinesisStream");
}

#[test]
#[serial]
fn test_msk_service_name_instance_fallback() {
    unsafe { std::env::remove_var("DD_TRACE_AWS_SERVICE_REPRESENTATION_ENABLED") };
    let json = read_json_file("msk_event.json");
    let payload = serde_json::from_str(&json).expect("Failed to deserialize");
    let event = MSKEvent::new(payload).expect("deserialize MSKEvent");
    let cluster_name = event.get_specific_identifier();
    let service = event.resolve_service_name(&HashMap::new(), &cluster_name, "msk");
    assert_eq!(service, "demo-cluster");
}

#[test]
#[serial]
fn test_sns_service_name_instance_fallback() {
    unsafe { std::env::remove_var("DD_TRACE_AWS_SERVICE_REPRESENTATION_ENABLED") };
    let json = read_json_file("sns_event.json");
    let payload = serde_json::from_str(&json).expect("Failed to deserialize");
    let event = SnsRecord::new(payload).expect("deserialize SnsRecord");
    let topic_name = event.get_specific_identifier();
    let service = event.resolve_service_name(&HashMap::new(), &topic_name, "sns");
    assert_eq!(service, "serverlessTracingTopicPy");
}

use serial_test::serial;
use std::env;

fn with_env_var<F: FnOnce()>(f: F) {
    // Helper to set env var to "false" and restore original value after running closure
    const VAR: &str = "DD_TRACE_AWS_SERVICE_REPRESENTATION_ENABLED";
    let original = env::var(VAR).ok();
    unsafe { env::set_var(VAR, "false") };
    f();
    if let Some(val) = original {
        unsafe { env::set_var(VAR, val) };
    } else {
        unsafe { env::remove_var(VAR) };
    }
}

#[test]
#[serial]
fn test_dynamodb_service_name_env_var_false_uses_fallback() {
    with_env_var(|| {
        let json = read_json_file("dynamodb_event.json");
        let payload = serde_json::from_str(&json).expect("Failed to deserialize");
        let event = DynamoDbRecord::new(payload).expect("deserialize DynamoDbRecord");
        let table_name = event.get_specific_identifier();
        let service = event.resolve_service_name(&HashMap::new(), &table_name, "dynamodb");
        assert_eq!(service, "dynamodb");
    });
}

#[test]
#[serial]
fn test_s3_service_name_env_var_false_uses_fallback() {
    with_env_var(|| {
        let json = read_json_file("s3_event.json");
        let payload = serde_json::from_str(&json).expect("Failed to deserialize");
        let event = S3Record::new(payload).expect("deserialize S3Record");
        let bucket_name = event.get_specific_identifier();
        let service = event.resolve_service_name(&HashMap::new(), &bucket_name, "s3");
        assert_eq!(service, "s3");
    });
}

#[test]
#[serial]
fn test_lambda_url_service_name_env_var_false_uses_fallback() {
    with_env_var(|| {
        let json = read_json_file("lambda_function_url_event.json");
        let payload = serde_json::from_str(&json).expect("Failed to deserialize");
        let event =
            LambdaFunctionUrlEvent::new(payload).expect("deserialize LambdaFunctionUrlEvent");
        let domain = event.get_specific_identifier();
        let service = event.resolve_service_name(&HashMap::new(), &domain, "lambda_url");
        assert_eq!(service, "lambda_url");
    });
}
