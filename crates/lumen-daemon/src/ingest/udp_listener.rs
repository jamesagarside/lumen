//! UDP NetFlow listener: v5, v9, and IPFIX on the same socket,
//! dispatched by the version byte in the datagram header.
//!
//! v9 and IPFIX are template-based, so the listener owns a
//! `TemplateCache` that lives for the lifetime of the daemon —
//! templates the exporter has sent stay valid until it sends a new
//! one for the same `(observation_domain, template_id)`. v5 is
//! self-describing and stateless; the cache is ignored for it.

use std::net::SocketAddr;

use anyhow::Context;
use tokio::net::UdpSocket;
use tracing::{debug, info, warn};

use super::netflow_v5;
use super::netflow_v9::{self, TemplateCache};
use super::FlowBus;
use crate::metrics::FLOWS_INGESTED;

const MAX_DATAGRAM_BYTES: usize = 65_535;

/// Bind a UDP socket on `listen` and accept NetFlow v5 / v9 / IPFIX
/// datagrams, parsing each according to its version byte. Returns
/// only when the socket closes (in practice, when the daemon exits).
pub async fn run(listen: SocketAddr, bus: FlowBus) -> anyhow::Result<()> {
    let socket = UdpSocket::bind(listen)
        .await
        .with_context(|| format!("binding UDP {} for netflow listener", listen))?;
    let actual = socket.local_addr()?;
    info!(listen = %actual, "netflow listener up (v5/v9/ipfix)");

    let mut cache = TemplateCache::new();
    let mut buf = vec![0u8; MAX_DATAGRAM_BYTES];

    loop {
        let (n, peer) = match socket.recv_from(&mut buf).await {
            Ok(v) => v,
            Err(e) => {
                warn!(error = %e, "udp recv failed");
                continue;
            }
        };
        if n < 2 {
            warn!(peer = %peer, bytes = n, "datagram too short for version byte");
            continue;
        }

        let version = u16::from_be_bytes([buf[0], buf[1]]);
        let (protocol_label, parse_result) = match version {
            5 => (
                "netflow_v5",
                netflow_v5::parse(&buf[..n]).map_err(|e| e.to_string()),
            ),
            9 => (
                "netflow_v9",
                netflow_v9::parse(&buf[..n], &mut cache, peer).map_err(|e| e.to_string()),
            ),
            10 => (
                "ipfix",
                netflow_v9::parse(&buf[..n], &mut cache, peer).map_err(|e| e.to_string()),
            ),
            _ => {
                warn!(peer = %peer, version, bytes = n, "unsupported netflow version");
                continue;
            }
        };

        match parse_result {
            Ok(flows) => {
                debug!(
                    protocol = protocol_label,
                    peer = %peer,
                    bytes = n,
                    flows = flows.len(),
                    "datagram parsed"
                );
                metrics::counter!(FLOWS_INGESTED, "protocol" => protocol_label)
                    .increment(flows.len() as u64);
                for flow in flows {
                    bus.publish(flow);
                }
            }
            Err(reason) => {
                warn!(
                    protocol = protocol_label,
                    peer = %peer,
                    bytes = n,
                    reason = %reason,
                    "datagram dropped"
                );
            }
        }
    }
}
