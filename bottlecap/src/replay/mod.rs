//! Record/replay prototype: record a function's invocations and outbound
//! HTTP(S) in Lambda, replay them offline with the `lambda-replay` binary.
//! See `REPLAY_PLAN.md` at the repo root.

pub mod ca;
pub mod format;
pub mod matcher;
pub mod mitm;
pub mod player;
pub mod recorder;
pub mod rules;
pub mod s3;

use crate::config::aws::AwsConfig;
use crate::replay::ca::CertificateAuthority;
use crate::replay::format::{FORMAT_VERSION, Record};
use crate::replay::recorder::Recorder;
use crate::replay::s3::S3Uploader;
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, OnceLock};
use std::time::Duration;
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;
use tracing::debug;

/// Port of the MITM proxy, as set in `datadog_wrapper`.
pub const MITM_PORT: u16 = 9100;
/// CA files and local segments; `datadog_wrapper` points the runtime here.
pub const REPLAY_DIR: &str = "/tmp/dd-replay";

static RECORDER: OnceLock<Arc<Recorder>> = OnceLock::new();

#[must_use]
pub fn record_enabled() -> bool {
    std::env::var("DD_REPLAY_RECORD")
        .is_ok_and(|v| matches!(v.to_ascii_lowercase().as_str(), "1" | "true"))
}

/// The recorder, when record mode is on.
pub fn recorder() -> Option<&'static Arc<Recorder>> {
    RECORDER.get()
}

/// Environment variables kept in a recording: everything but credentials.
#[must_use]
pub fn recordable_env() -> BTreeMap<String, String> {
    std::env::vars()
        .filter(|(k, _)| {
            let u = k.to_ascii_uppercase();
            !(u.contains("SECRET")
                || u.contains("TOKEN")
                || u.contains("PASSWORD")
                || u.ends_with("API_KEY")
                || u.ends_with("APP_KEY")
                || u == "AWS_ACCESS_KEY_ID"
                || u.starts_with("AWS_CONTAINER_"))
        })
        .collect()
}

/// Starts record mode: writes the CA files the wrapper points the runtime at,
/// starts the MITM proxy and installs the recorder. Must run before the
/// extension registers, since the runtime starts after registration.
pub async fn start_recording(aws_config: &AwsConfig) -> anyhow::Result<CancellationToken> {
    let ca = Arc::new(CertificateAuthority::generate()?);
    ca.write_files(&PathBuf::from(REPLAY_DIR))?;

    let now = chrono::Utc::now();
    let sandbox_id = format!(
        "{}-{:x}",
        now.format("%Y%m%dT%H%M%SZ"),
        u64::from(std::process::id()) ^ u64::from(now.timestamp_subsec_nanos())
    );
    let session = Record::Session {
        version: FORMAT_VERSION,
        sandbox_id: sandbox_id.clone(),
        function_name: aws_config.function_name.clone(),
        region: aws_config.region.clone(),
        runtime: std::env::var("AWS_EXECUTION_ENV").unwrap_or_default(),
        env: recordable_env(),
    };

    let client = datadog_fips::reqwest_adapter::create_reqwest_client_builder()
        .map_err(|e| anyhow::anyhow!("{e}"))?
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .build()?;
    let uploader = std::env::var("DD_REPLAY_S3_BUCKET").ok().map(|bucket| {
        S3Uploader::new(
            bucket,
            std::env::var("DD_REPLAY_S3_PREFIX").unwrap_or_default(),
            aws_config.region.clone(),
            client.clone(),
        )
    });
    let recorder = Arc::new(Recorder::new(
        PathBuf::from(REPLAY_DIR).join("recordings"),
        sandbox_id.clone(),
        session,
        aws_config.function_name.clone(),
        Some(PathBuf::from(REPLAY_DIR).join("runtime.env")),
        uploader,
    ));
    let _ = RECORDER.set(Arc::clone(&recorder));

    let listener = TcpListener::bind(("127.0.0.1", MITM_PORT)).await?;
    let handler = mitm::forwarding_handler(client, move |req, resp, duration| {
        recorder.on_exchange(req, resp, duration);
    });
    let shutdown = CancellationToken::new();
    tokio::spawn(mitm::serve(listener, ca, handler, shutdown.clone()));
    debug!("REPLAY | recording sandbox {sandbox_id}, MITM on 127.0.0.1:{MITM_PORT}");
    Ok(shutdown)
}

/// Called before the extension asks for its next event: Lambda may freeze
/// the sandbox right after, so finish pending uploads first.
pub async fn before_next_event() {
    if let Some(r) = recorder() {
        r.wait_uploads(Duration::from_secs(2)).await;
    }
}

/// Called on SHUTDOWN: write anything recorded since the last invocation.
pub async fn on_shutdown() {
    if let Some(r) = recorder() {
        r.flush().await;
        r.wait_uploads(Duration::from_millis(1500)).await;
    }
}
