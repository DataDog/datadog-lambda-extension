//! The file that Lambda starts as the Datadog extension. Lambda starts the runtime only after
//! every extension has registered, and the full extension binary takes tens of milliseconds to
//! load. This small binary registers first, then replaces itself with the full extension, which
//! uses the registration instead of making its own.

use std::env;
use std::io::{self, Read, Write};
use std::net::TcpStream;
use std::os::unix::process::CommandExt;
use std::process::{self, Command};

use bottlecap::config::aws::LAMBDA_MANAGED_INSTANCES_INIT_TYPE;
use bottlecap::extension::{
    EXTENSION_ACCEPT_FEATURE_HEADER, EXTENSION_FEATURES, EXTENSION_ID_HEADER, EXTENSION_NAME,
    EXTENSION_NAME_HEADER, EXTENSION_ROUTE, LAUNCHER_ACCOUNT_ID_ENV_VAR,
    LAUNCHER_EXTENSION_ID_ENV_VAR, RegisterResponse, get_subscription_event_types,
};

/// Where the layer puts the full extension. It must be outside `/opt/extensions`, or Lambda starts
/// it as a second extension.
const EXTENSION_BINARY_PATH: &str = "/opt/datadog-agent";

fn main() {
    let runtime_api = env::var("AWS_LAMBDA_RUNTIME_API").unwrap_or_default();
    let managed_instance_mode = env::var("AWS_LAMBDA_INITIALIZATION_TYPE")
        .is_ok_and(|value| value == LAMBDA_MANAGED_INSTANCES_INIT_TYPE);
    let registration = match register(&runtime_api, managed_instance_mode) {
        Ok(registration) => registration,
        Err(e) => {
            log_error(&format!("Failed to register extension: {e}"));
            process::exit(1);
        }
    };

    let mut command = Command::new(EXTENSION_BINARY_PATH);
    command.env(LAUNCHER_EXTENSION_ID_ENV_VAR, &registration.extension_id);
    if let Some(account_id) = &registration.account_id {
        command.env(LAUNCHER_ACCOUNT_ID_ENV_VAR, account_id);
    }
    // `exec` returns only when it fails, for example when a container image has this file but not
    // the full extension.
    let error = command.exec();
    log_error(&format!(
        "Failed to start {EXTENSION_BINARY_PATH}: {error}. Calling /next without Datadog instrumentation"
    ));
    loop {
        if let Err(e) = request(
            &runtime_api,
            "GET",
            &format!("/{EXTENSION_ROUTE}/event/next"),
            &[(EXTENSION_ID_HEADER, &registration.extension_id)],
            "",
        ) {
            log_error(&format!("Error getting next event: {e}"));
            process::exit(1);
        }
    }
}

fn register(runtime_api: &str, managed_instance_mode: bool) -> io::Result<RegisterResponse> {
    let body = serde_json::json!({ "events": get_subscription_event_types(managed_instance_mode) });
    let response = request(
        runtime_api,
        "POST",
        &format!("/{EXTENSION_ROUTE}/register"),
        &[
            (EXTENSION_NAME_HEADER, EXTENSION_NAME),
            (EXTENSION_ACCEPT_FEATURE_HEADER, EXTENSION_FEATURES),
            ("Content-Type", "application/json"),
        ],
        &body.to_string(),
    )?;
    parse_register_response(&response)
}

fn parse_register_response(response: &str) -> io::Result<RegisterResponse> {
    let (head, body) = response
        .split_once("\r\n\r\n")
        .ok_or_else(|| io::Error::other("malformed HTTP response"))?;
    let status_line = head.lines().next().unwrap_or_default();
    if status_line.split(' ').nth(1) != Some("200") {
        return Err(io::Error::other(format!(
            "unexpected status: {status_line}"
        )));
    }
    let extension_id = head
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case(EXTENSION_ID_HEADER)
                .then(|| value.trim().to_string())
        })
        .ok_or_else(|| io::Error::other(format!("no {EXTENSION_ID_HEADER} header")))?;
    let mut registration: RegisterResponse = serde_json::from_str(body)?;
    registration.extension_id = extension_id;
    Ok(registration)
}

/// Sends one HTTP/1.1 request to the Runtime API on a new connection, and returns the whole
/// response. The connection closes after the response, so reading to the end is enough.
fn request(
    runtime_api: &str,
    method: &str,
    path: &str,
    headers: &[(&str, &str)],
    body: &str,
) -> io::Result<String> {
    let mut stream = TcpStream::connect(runtime_api)?;
    let mut request =
        format!("{method} {path} HTTP/1.1\r\nHost: {runtime_api}\r\nConnection: close\r\n");
    for (name, value) in headers {
        request.push_str(&format!("{name}: {value}\r\n"));
    }
    request.push_str(&format!("Content-Length: {}\r\n\r\n{body}", body.len()));
    stream.write_all(request.as_bytes())?;
    let mut response = String::new();
    stream.read_to_string(&mut response)?;
    Ok(response)
}

/// Writes an error line in the same JSON format as the extension's logger.
fn log_error(message: &str) {
    println!(
        "{}",
        serde_json::json!({ "status": "ERROR", "message": format!("DD_EXTENSION | ERROR | {message}") })
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_register_response() {
        let response = "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\
            lambda-extension-identifier: 3da1f2dc-3222-475e-9205-e2e6c6318895\r\n\r\n\
            {\"functionName\":\"f\",\"functionVersion\":\"$LATEST\",\"handler\":\"h\",\"accountId\":\"123456789012\"}";
        let registration = parse_register_response(response).expect("valid response");
        assert_eq!(
            registration.extension_id,
            "3da1f2dc-3222-475e-9205-e2e6c6318895"
        );
        assert_eq!(registration.account_id.as_deref(), Some("123456789012"));
    }

    #[test]
    fn test_parse_register_response_rejects_error_status() {
        let response = "HTTP/1.1 403 Forbidden\r\nContent-Type: application/json\r\n\r\n{}";
        assert!(parse_register_response(response).is_err());
    }
}
