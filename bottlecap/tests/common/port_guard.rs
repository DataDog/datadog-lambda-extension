// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

// `#[path]`-included by several test binaries; not every binary uses every item.
#![allow(dead_code)]
//! Hold or probe the fixed ports the extension binds.
//!
//! The binary binds 8124 (lifecycle listener, `127.0.0.1`), 8125/udp
//! (DogStatsD, `0.0.0.0`), 8126 (trace agent, `127.0.0.1`), 8999 (Telemetry
//! API listener, `0.0.0.0`) and 9000 (runtime API proxy, `0.0.0.0`, only
//! with LWA or the Datadog wrapper). A guard must bind the same address as
//! the extension: on macOS a `127.0.0.1` bind coexists with a `0.0.0.0` one
//! when `SO_REUSEADDR` is set (tokio sets it), so a wildcard guard would not
//! conflict with the trace agent there. Tests that spawn the binary are
//! serialized (`#[serial_test::serial(bottlecap_ports)]` and the nextest
//! `fixed-ports` group); this module checks the ports are free before a
//! spawn and holds one of them for the conflict tests.

use std::net::{IpAddr, Ipv4Addr, TcpListener, UdpSocket};
use std::time::{Duration, Instant};

pub const LIFECYCLE_PORT: u16 = 8124;
pub const DOGSTATSD_UDP_PORT: u16 = 8125;
pub const TRACE_AGENT_PORT: u16 = 8126;
pub const TELEMETRY_PORT: u16 = 8999;
pub const PROXY_PORT: u16 = 9000;

const LOOPBACK: IpAddr = IpAddr::V4(Ipv4Addr::LOCALHOST);
const ANY: IpAddr = IpAddr::V4(Ipv4Addr::UNSPECIFIED);

/// TCP ports the extension binds in every mode.
pub const FIXED_TCP_PORTS: [u16; 3] = [LIFECYCLE_PORT, TRACE_AGENT_PORT, TELEMETRY_PORT];

/// The address the extension binds `port` on.
pub fn bind_ip(port: u16) -> IpAddr {
    match port {
        LIFECYCLE_PORT | TRACE_AGENT_PORT => LOOPBACK,
        _ => ANY,
    }
}

/// Keeps a port bound until dropped.
pub struct PortGuard {
    pub port: u16,
    tcp: Option<TcpListener>,
    udp: Option<UdpSocket>,
}

impl PortGuard {
    /// Bind `port` over TCP on the address the extension uses. `None` when
    /// something else holds it.
    pub fn tcp(port: u16) -> Option<Self> {
        let tcp = TcpListener::bind((bind_ip(port), port)).ok()?;
        Some(Self {
            port,
            tcp: Some(tcp),
            udp: None,
        })
    }

    /// Bind `port` over UDP on the address the extension uses.
    pub fn udp(port: u16) -> Option<Self> {
        let udp = UdpSocket::bind((bind_ip(port), port)).ok()?;
        Some(Self {
            port,
            tcp: None,
            udp: Some(udp),
        })
    }

    /// Bind over TCP, waiting up to `budget` for the port to free up.
    pub async fn tcp_wait(port: u16, budget: Duration) -> Option<Self> {
        let start = Instant::now();
        loop {
            if let Some(g) = Self::tcp(port) {
                return Some(g);
            }
            if start.elapsed() > budget {
                return None;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    pub async fn udp_wait(port: u16, budget: Duration) -> Option<Self> {
        let start = Instant::now();
        loop {
            if let Some(g) = Self::udp(port) {
                return Some(g);
            }
            if start.elapsed() > budget {
                return None;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }
}

/// Whether a TCP bind on `port` succeeds on both the loopback and the
/// wildcard address, since a listener on either would conflict on Linux.
/// The two probes run one after the other: the first listener is dropped
/// before the second bind, or on Linux the second would fail against our own
/// probe (two listeners cannot overlap, even with `SO_REUSEADDR`).
pub fn tcp_port_free(port: u16) -> bool {
    let loopback = TcpListener::bind((LOOPBACK, port)).is_ok();
    if !loopback {
        return false;
    }
    TcpListener::bind((ANY, port)).is_ok()
}

/// Same as [`tcp_port_free`] for UDP.
pub fn udp_port_free(port: u16) -> bool {
    let loopback = UdpSocket::bind((LOOPBACK, port)).is_ok();
    if !loopback {
        return false;
    }
    UdpSocket::bind((ANY, port)).is_ok()
}

/// Ports among the fixed set that are busy right now.
pub fn busy_fixed_ports() -> Vec<String> {
    let mut busy: Vec<String> = FIXED_TCP_PORTS
        .iter()
        .filter(|p| !tcp_port_free(**p))
        .map(|p| format!("tcp/{p}"))
        .collect();
    if !udp_port_free(DOGSTATSD_UDP_PORT) {
        busy.push(format!("udp/{DOGSTATSD_UDP_PORT}"));
    }
    busy
}

/// Wait until every fixed port is free (a previous child may still be
/// dying). Returns the ports still busy after `budget`.
pub async fn wait_fixed_ports_free(budget: Duration) -> Vec<String> {
    let start = Instant::now();
    loop {
        let busy = busy_fixed_ports();
        if busy.is_empty() || start.elapsed() > budget {
            return busy;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// Fail fast with a clear message when the fixed ports are not free. Call it
/// before each spawn so a leaked process from another test is named, not
/// mistaken for a bug in the extension.
pub async fn require_fixed_ports_free() {
    let busy = wait_fixed_ports_free(Duration::from_secs(5)).await;
    assert!(
        busy.is_empty(),
        "fixed ports still busy before spawn: {busy:?}. Spawning tests must carry #[serial_test::serial(bottlecap_ports)] and run in the `fixed-ports` nextest group."
    );
}
