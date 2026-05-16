//! Generic UDP listener that delegates parsing to a protocol-specific
//! function and forwards results onto the [`FlowBus`].
//!
//! NetFlow v5/v9 and IPFIX all use UDP, so they share this scaffolding.
//! This v1 only wires up NetFlow v5; v9/IPFIX land in #15.

use std::net::SocketAddr;

use anyhow::Context;
use lumen_core::Flow;
use tokio::net::UdpSocket;
use tracing::{debug, info, warn};

use super::FlowBus;
use crate::metrics::FLOWS_INGESTED;

const MAX_DATAGRAM_BYTES: usize = 65_535;

/// Protocol-specific parser: takes a single datagram, returns parsed flows.
pub trait DatagramParser: Send + Sync + 'static {
    fn name(&self) -> &'static str;
    fn parse(&self, datagram: &[u8]) -> Result<Vec<Flow>, String>;
}

pub struct NetflowV5Parser;

impl DatagramParser for NetflowV5Parser {
    fn name(&self) -> &'static str {
        "netflow_v5"
    }

    fn parse(&self, datagram: &[u8]) -> Result<Vec<Flow>, String> {
        super::netflow_v5::parse(datagram).map_err(|e| e.to_string())
    }
}

/// Bind a UDP socket on `listen` and feed parsed flows into `bus`. Returns
/// when the socket is closed (in practice, when the daemon exits).
pub async fn run<P: DatagramParser>(
    listen: SocketAddr,
    parser: P,
    bus: FlowBus,
) -> anyhow::Result<()> {
    let socket = UdpSocket::bind(listen)
        .await
        .with_context(|| format!("binding UDP {} for {}", listen, parser.name()))?;

    let actual = socket.local_addr()?;
    info!(
        protocol = parser.name(),
        listen = %actual,
        "ingest listener up"
    );

    let mut buf = vec![0u8; MAX_DATAGRAM_BYTES];
    loop {
        let (n, peer) = match socket.recv_from(&mut buf).await {
            Ok(v) => v,
            Err(e) => {
                warn!(protocol = parser.name(), error = %e, "udp recv failed");
                continue;
            }
        };

        match parser.parse(&buf[..n]) {
            Ok(flows) => {
                debug!(
                    protocol = parser.name(),
                    peer = %peer,
                    bytes = n,
                    flows = flows.len(),
                    "datagram parsed"
                );
                metrics::counter!(FLOWS_INGESTED, "protocol" => parser.name())
                    .increment(flows.len() as u64);
                for flow in flows {
                    bus.publish(flow);
                }
            }
            Err(reason) => {
                warn!(
                    protocol = parser.name(),
                    peer = %peer,
                    bytes = n,
                    reason = %reason,
                    "datagram dropped"
                );
            }
        }
    }
}
