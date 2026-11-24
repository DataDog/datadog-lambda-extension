// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0
// Integration tests are exempt from the production lint policy in Cargo.toml.
#![allow(
    unreachable_pub,
    clippy::pedantic,
    clippy::panic,
    clippy::unwrap_used,
    clippy::expect_used
)]

//! With `DD_PROXY_HTTPS` set to a CONNECT proxy and `DD_TLS_CERT_FILE` holding the CA
//! that signed the intake's certificate, a trace reaches the intake through the tunnel.
//! The handshake inside the tunnel must trust that CA, as the TLS-intercepting proxies
//! some customers run in front of Datadog require.

mod fixtures;
#[path = "common/harness.rs"]
mod harness;

use std::io::BufReader;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use harness::prelude::*;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

const TRACES_PATH: &str = "/api/v0.2/traces";

// Test PKI shared with `src/http.rs`: a private CA that is in no public root store, and a
// `localhost` leaf it signed. See the comment on these constants there to regenerate.
const CA_PEM: &str = r"
-----BEGIN CERTIFICATE-----
MIIBnzCCAUWgAwIBAgIUf5l+dmNjL/xU10EM0qk4Iseh3nQwCgYIKoZIzj0EAwIw
HDEaMBgGA1UEAwwRQm90dGxlY2FwIFRlc3QgQ0EwIBcNMjYwNjI1MTgzNjMwWhgP
MjEyNjA2MDExODM2MzBaMBwxGjAYBgNVBAMMEUJvdHRsZWNhcCBUZXN0IENBMFkw
EwYHKoZIzj0CAQYIKoZIzj0DAQcDQgAEK9O0WnLz1pRAD3RZzYzsCf8vEieLLwnV
D8RNYJBaEBZBJbE+4snr+0vhVm2mGtIojZTJ0bc5Z98JHpZ57LBpwqNjMGEwHQYD
VR0OBBYEFJtZHbWEv1FI0KacM/4ctJkUuMg2MB8GA1UdIwQYMBaAFJtZHbWEv1FI
0KacM/4ctJkUuMg2MA8GA1UdEwEB/wQFMAMBAf8wDgYDVR0PAQH/BAQDAgEGMAoG
CCqGSM49BAMCA0gAMEUCIQDFLc+zjel9LGmytihROgErrPc6WDxmziypva+k2cSN
CAIgYb1nsDag2/bwlzf4OOcYvqU9xNRdXMarkRxn4o5rPR4=
-----END CERTIFICATE-----
";

const SERVER_CERT_PEM: &str = r"
-----BEGIN CERTIFICATE-----
MIIBvTCCAWSgAwIBAgIUOkYng/RZxQcYlMY5I+RAXzR7dSkwCgYIKoZIzj0EAwIw
HDEaMBgGA1UEAwwRQm90dGxlY2FwIFRlc3QgQ0EwIBcNMjYwNjI1MTgzNjMwWhgP
MjEyNjA2MDExODM2MzBaMBQxEjAQBgNVBAMMCWxvY2FsaG9zdDBZMBMGByqGSM49
AgEGCCqGSM49AwEHA0IABOuKun0roY5MAEOgA3NNebQa9l56HVsNFwGJ4a5chM2T
s+vToAoyflPMZfxuS6PUv2WHrahmUH5WZKr0XaT/RIijgYkwgYYwCQYDVR0TBAIw
ADAOBgNVHQ8BAf8EBAMCB4AwEwYDVR0lBAwwCgYIKwYBBQUHAwEwFAYDVR0RBA0w
C4IJbG9jYWxob3N0MB0GA1UdDgQWBBTcqKduDXHEOeUXAL6fqwAYl50VVTAfBgNV
HSMEGDAWgBSbWR21hL9RSNCmnDP+HLSZFLjINjAKBggqhkjOPQQDAgNHADBEAiAT
nwOMjOwnQoeLvZgHhNqrzlVGNqnT4FPudJTgTKrAAAIgVMw8wqk70JXLm4pEdN8/
oMUo5j7nV6S8Hd02b5hW5pM=
-----END CERTIFICATE-----
";

const SERVER_KEY_PEM: &str = r"
-----BEGIN PRIVATE KEY-----
MIGHAgEAMBMGByqGSM49AgEGCCqGSM49AwEHBG0wawIBAQQgkflx4lEcI8Fafj8E
EgBejgnIKCAkaQXo+p0h3a6IsQOhRANCAATrirp9K6GOTABDoANzTXm0GvZeeh1b
DRcBieGuXITNk7Pr06AKMn5TzGX8bkuj1L9lh62oZlB+VmSq9F2k/0SI
-----END PRIVATE KEY-----
";

/// An HTTPS endpoint presenting the private-CA leaf for `localhost`. It decrypts and
/// relays to `intake`, the plain-HTTP fault intake, which records and decodes the
/// requests. Returns the port.
async fn tls_front(intake: SocketAddr) -> u16 {
    #[cfg(feature = "fips")]
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    #[cfg(not(feature = "fips"))]
    let _ = rustls::crypto::ring::default_provider().install_default();
    let certs = rustls_pemfile::certs(&mut BufReader::new(SERVER_CERT_PEM.as_bytes()))
        .collect::<Result<Vec<_>, _>>()
        .expect("parse server cert");
    let key = rustls_pemfile::private_key(&mut BufReader::new(SERVER_KEY_PEM.as_bytes()))
        .expect("read server key")
        .expect("server key present");
    let config = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(certs, key)
        .expect("server tls config");
    let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(config));
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind tls front");
    let port = listener.local_addr().expect("addr").port();
    tokio::spawn(async move {
        loop {
            let Ok((sock, _)) = listener.accept().await else {
                return;
            };
            let acceptor = acceptor.clone();
            tokio::spawn(async move {
                let Ok(mut tls) = acceptor.accept(sock).await else {
                    return;
                };
                let Ok(mut plain) = TcpStream::connect(intake).await else {
                    return;
                };
                let _ = tokio::io::copy_bidirectional(&mut tls, &mut plain).await;
            });
        }
    });
    port
}

/// A CONNECT proxy that tunnels only to `127.0.0.1:allowed_port` and answers `403` to any
/// other target, so no request leaves the machine. It never terminates TLS: the client
/// verifies the destination's certificate inside the tunnel.
struct ConnectProxy {
    port: u16,
    /// The `host:port` of every CONNECT it received.
    targets: Arc<Mutex<Vec<String>>>,
}

async fn connect_proxy(allowed_port: u16) -> ConnectProxy {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind proxy");
    let port = listener.local_addr().expect("addr").port();
    let targets = Arc::new(Mutex::new(Vec::new()));
    let seen = Arc::clone(&targets);
    tokio::spawn(async move {
        loop {
            let Ok((mut client, _)) = listener.accept().await else {
                return;
            };
            let seen = Arc::clone(&seen);
            tokio::spawn(async move {
                let mut head = Vec::new();
                let mut buf = [0u8; 1024];
                while !head.windows(4).any(|w| w == b"\r\n\r\n") {
                    match client.read(&mut buf).await {
                        Ok(0) | Err(_) => return,
                        Ok(n) => head.extend_from_slice(&buf[..n]),
                    }
                }
                let head = String::from_utf8_lossy(&head).into_owned();
                let mut first = head.lines().next().unwrap_or_default().split_whitespace();
                if first.next() != Some("CONNECT") {
                    return;
                }
                let target = first.next().unwrap_or_default().to_string();
                seen.lock().expect("targets lock").push(target.clone());
                let upstream = if target.rsplit(':').next() == Some(&allowed_port.to_string()) {
                    TcpStream::connect(("127.0.0.1", allowed_port)).await.ok()
                } else {
                    None
                };
                let Some(mut upstream) = upstream else {
                    let _ = client.write_all(b"HTTP/1.1 403 Forbidden\r\n\r\n").await;
                    return;
                };
                if client
                    .write_all(b"HTTP/1.1 200 Connection established\r\n\r\n")
                    .await
                    .is_ok()
                {
                    let _ = tokio::io::copy_bidirectional(&mut client, &mut upstream).await;
                }
            });
        }
    });
    ConnectProxy { port, targets }
}

/// A trace flushed at the end of an invocation reaches an HTTPS intake whose certificate a
/// private CA signed, through `DD_PROXY_HTTPS`, when `DD_TLS_CERT_FILE` names that CA.
#[tokio::test(flavor = "multi_thread")]
#[serial_test::serial(bottlecap_ports)]
async fn trace_reaches_private_ca_intake_through_connect_proxy() {
    const TRACE_ID: u64 = 7;
    let s = Scenario::start(FaultMode::Ok).await;
    let front = tls_front(s.intake.addr).await;
    let proxy = connect_proxy(front).await;
    let ca_dir = tempfile::tempdir().expect("tempdir");
    let ca_path = ca_dir.path().join("ca.pem");
    std::fs::write(&ca_path, CA_PEM).expect("write ca");

    let mut bc = s
        .bottlecap()
        .env("DD_APM_DD_URL", &format!("https://localhost:{front}"))
        .env(
            "DD_PROXY_HTTPS",
            &format!("http://127.0.0.1:{}", proxy.port),
        )
        .env("DD_TLS_CERT_FILE", ca_path.to_str().expect("utf8 path"))
        .spawn();
    let deadline = s
        .invoke("req-1", secs(10), 1, bc.startup_deadline())
        .await
        .unwrap_or_else(|| panic!("no /next poll. {}", bc.log_tail(40)));

    let status = post_trace(msgpack_trace(TRACE_ID, 0), STARTUP_BUDGET)
        .await
        .unwrap();
    assert_eq!(
        status,
        200,
        "the trace agent refused the trace. {}",
        bc.log_tail(40)
    );
    deliver_start(&s.api, "req-1", true).await.unwrap();
    deliver_runtime_done(&s.api, "req-1", true).await.unwrap();

    let delivered = s
        .intake
        .wait_for(
            1,
            |r| {
                r.path.contains(TRACES_PATH)
                    && r.status == 202
                    && r.api_key() == Some(TEST_API_KEY)
                    && r.trace_ids().is_ok_and(|ids| ids.contains(&TRACE_ID))
            },
            deadline,
        )
        .await;
    let released = s.api.wait_for_next_calls(2, deadline).await;
    let targets = proxy.targets.lock().unwrap().clone();
    s.shutdown_cleanly(&mut bc).await;

    assert!(
        delivered,
        "the trace did not reach the private-CA intake through the proxy. CONNECTs: {targets:?}\nIntake:\n{}\n{}",
        s.intake.summary(),
        bc.log_tail(40)
    );
    assert!(
        targets.contains(&format!("localhost:{front}")),
        "the trace did not go through the proxy. CONNECTs: {targets:?}"
    );
    assert!(
        released,
        "/next was not called by the invoke deadline. {}",
        bc.log_tail(40)
    );
}
