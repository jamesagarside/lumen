//! Syslog ingestion for iptables-style connection logs.
//!
//! Primary target: UniFi gateways (UDM-Pro, Dream Machine line) which
//! emit per-packet kernel logs when iptables rules match. The default
//! UniFi "Network → Settings → System → System Logs → Remote Syslog
//! Server" emits standard RFC 3164 syslog with the message body being
//! an iptables LOG action.
//!
//! Example line (one packet's worth):
//!
//! ```text
//! <134>Nov 17 10:11:12 unifi kernel: [LAN_LOCAL-D]IN=br1 OUT= MAC=...
//!   SRC=192.168.1.10 DST=8.8.8.8 LEN=78 TOS=0x00 PREC=0x00 TTL=64
//!   ID=12345 PROTO=UDP SPT=51234 DPT=53 LEN=58
//! ```
//!
//! Each line is one *packet*, not a flow — so we emit a `Flow` with
//! `packets=1` and `bytes=LEN`. The Live State Engine's edge
//! accumulation aggregates these naturally over time.
//!
//! The same iptables LOG format is used by OPNsense, pfSense (with
//! the filter log → syslog bridge), and any vanilla Linux router
//! running netfilter rules with `-j LOG`. So this parser doubles as
//! a generic iptables-LOG parser for those too — UniFi was just the
//! design driver.

use std::net::{IpAddr, SocketAddr};
use std::time::SystemTime;

use anyhow::Context;
use lumen_core::{Flow, FlowEndpoint, FlowSource, Protocol};
use tokio::net::UdpSocket;
use tracing::{debug, info, trace, warn};

use super::FlowBus;
use crate::metrics::FLOWS_INGESTED;

const MAX_DATAGRAM_BYTES: usize = 65_535;

pub async fn run(listen: SocketAddr, bus: FlowBus) -> anyhow::Result<()> {
    let socket = UdpSocket::bind(listen)
        .await
        .with_context(|| format!("binding UDP {} for syslog listener", listen))?;
    let actual = socket.local_addr()?;
    info!(listen = %actual, "syslog listener up");

    let mut buf = vec![0u8; MAX_DATAGRAM_BYTES];
    loop {
        let (n, peer) = match socket.recv_from(&mut buf).await {
            Ok(v) => v,
            Err(e) => {
                warn!(error = %e, "syslog recv failed");
                continue;
            }
        };
        let text = match std::str::from_utf8(&buf[..n]) {
            Ok(s) => s,
            Err(_) => {
                warn!(peer = %peer, bytes = n, "non-UTF-8 syslog datagram");
                continue;
            }
        };

        let mut emitted = 0usize;
        for line in text.lines() {
            if line.trim().is_empty() {
                continue;
            }
            match parse_line(line) {
                Ok(Some(flow)) => {
                    emitted += 1;
                    bus.publish(flow);
                }
                Ok(None) => {
                    // Recognised syslog line that didn't include the
                    // fields we need (e.g. an ICMP type-only log or
                    // a non-iptables kernel message). Trace-log it
                    // so we can debug template drift, not warn.
                    trace!(peer = %peer, "syslog line without flow fields, skipped");
                }
                Err(reason) => {
                    debug!(peer = %peer, reason = %reason, "syslog parse failed");
                }
            }
        }
        if emitted > 0 {
            metrics::counter!(FLOWS_INGESTED, "protocol" => "syslog").increment(emitted as u64);
            debug!(peer = %peer, bytes = n, emitted, "syslog datagram processed");
        }
    }
}

#[derive(Debug)]
pub enum ParseError {
    NoIptablesPayload,
    BadIp(String),
    BadPort(String),
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ParseError::NoIptablesPayload => write!(f, "no iptables key=value fields found"),
            ParseError::BadIp(v) => write!(f, "bad IP {v:?}"),
            ParseError::BadPort(v) => write!(f, "bad port {v:?}"),
        }
    }
}

impl std::error::Error for ParseError {}

/// Parse one syslog line. Returns `Ok(Some(Flow))` for a recognised
/// iptables LOG line, `Ok(None)` for a recognised line that doesn't
/// carry flow information (e.g. ICMP echo), `Err` for malformed
/// fields.
pub fn parse_line(line: &str) -> Result<Option<Flow>, ParseError> {
    // Strip the RFC 3164 frame if present: <PRI>TIMESTAMP HOSTNAME TAG: MESSAGE.
    // We don't need any of the frame fields — just locate the message body.
    let message = strip_syslog_frame(line);

    let mut src_ip: Option<IpAddr> = None;
    let mut dst_ip: Option<IpAddr> = None;
    let mut src_port: u16 = 0;
    let mut dst_port: u16 = 0;
    let mut protocol: u8 = 0;
    let mut bytes: u64 = 0;
    let mut saw_iptables_field = false;

    // iptables LOG uses space-separated KEY=value pairs. The first
    // LEN= is the total packet length; later LEN= (the L4 payload
    // length, present for UDP) is fine to overwrite or ignore — we
    // take the first one so we measure the IP packet not the
    // payload.
    let mut took_len = false;

    for tok in message.split_ascii_whitespace() {
        let Some((key, value)) = tok.split_once('=') else {
            continue;
        };
        saw_iptables_field = true;
        match key {
            "SRC" => src_ip = Some(parse_ip(value)?),
            "DST" => dst_ip = Some(parse_ip(value)?),
            "SPT" => src_port = parse_port(value)?,
            "DPT" => dst_port = parse_port(value)?,
            "PROTO" => protocol = protocol_to_iana(value),
            "LEN" if !took_len => {
                if let Ok(n) = value.parse::<u64>() {
                    bytes = n;
                    took_len = true;
                }
            }
            _ => {} // ignore IN, OUT, MAC, TOS, TTL, ID, etc.
        }
    }

    if !saw_iptables_field {
        return Err(ParseError::NoIptablesPayload);
    }
    let Some(src_ip) = src_ip else {
        return Ok(None);
    };
    let Some(dst_ip) = dst_ip else {
        return Ok(None);
    };

    // Per-packet log line — one packet, `bytes` worth of payload.
    let now = SystemTime::now();
    Ok(Some(Flow {
        source: FlowSource::Syslog,
        src: FlowEndpoint {
            ip: src_ip,
            port: src_port,
        },
        dst: FlowEndpoint {
            ip: dst_ip,
            port: dst_port,
        },
        protocol: Protocol(protocol),
        bytes,
        packets: 1,
        start: now,
        end: now,
    }))
}

/// Strip the `<PRI>TIMESTAMP HOSTNAME TAG: ` frame if present.
/// Falls back to returning the input as-is if the line doesn't look
/// like syslog (still parseable as raw iptables LOG output).
fn strip_syslog_frame(line: &str) -> &str {
    let mut s = line;
    // <PRI>
    if let Some(rest) = s.strip_prefix('<') {
        if let Some(end) = rest.find('>') {
            s = &rest[end + 1..];
        }
    }
    // Try to find the iptables payload by looking for the first
    // `[CHAIN]` marker OR the first KEY= token. Either way, return
    // from there.
    if let Some(bracket) = s.find('[') {
        if s[bracket..].contains(']') {
            return &s[bracket..];
        }
    }
    if let Some(eq) = s.find('=') {
        // Back up to the start of the token containing this `=`.
        let pre = &s[..eq];
        let start = pre
            .rfind(|c: char| c.is_ascii_whitespace())
            .map_or(0, |i| i + 1);
        return &s[start..];
    }
    s
}

fn parse_ip(v: &str) -> Result<IpAddr, ParseError> {
    v.parse().map_err(|_| ParseError::BadIp(v.to_string()))
}

fn parse_port(v: &str) -> Result<u16, ParseError> {
    v.parse().map_err(|_| ParseError::BadPort(v.to_string()))
}

fn protocol_to_iana(s: &str) -> u8 {
    match s.to_ascii_uppercase().as_str() {
        "ICMP" => 1,
        "IGMP" => 2,
        "TCP" => 6,
        "UDP" => 17,
        "GRE" => 47,
        "ESP" => 50,
        "AH" => 51,
        "ICMPV6" => 58,
        // Some exporters emit numeric proto directly.
        other => other.parse::<u8>().unwrap_or(0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::Ipv4Addr;

    #[test]
    fn parses_a_udm_tcp_line() {
        // Real-shaped UniFi UDM line for an HTTPS connection.
        let line = "<134>Nov 17 10:11:12 unifi kernel: [LAN_LOCAL-D]IN=br0 OUT=eth8 \
                    MAC=ff:ff:ff:ff:ff:ff:aa:bb:cc:dd:ee:ff:08:00 \
                    SRC=192.168.1.42 DST=140.82.121.4 LEN=4096 TOS=0x00 PREC=0x00 \
                    TTL=64 ID=12345 DF PROTO=TCP SPT=51234 DPT=443 \
                    WINDOW=65535 RES=0x00 SYN URGP=0";
        let flow = parse_line(line).unwrap().unwrap();
        assert_eq!(flow.src.ip, IpAddr::V4(Ipv4Addr::new(192, 168, 1, 42)));
        assert_eq!(flow.dst.ip, IpAddr::V4(Ipv4Addr::new(140, 82, 121, 4)));
        assert_eq!(flow.src.port, 51_234);
        assert_eq!(flow.dst.port, 443);
        assert_eq!(flow.protocol, Protocol::TCP);
        assert_eq!(flow.bytes, 4096);
        assert_eq!(flow.packets, 1);
        assert_eq!(flow.source, FlowSource::Syslog);
    }

    #[test]
    fn parses_a_udp_dns_line() {
        let line = "<134>Nov 17 10:11:12 unifi kernel: [WAN_OUT-A]IN=br0 OUT=eth8 \
                    SRC=192.168.1.42 DST=8.8.8.8 LEN=64 TOS=0x00 PREC=0x00 TTL=64 \
                    ID=999 DF PROTO=UDP SPT=51234 DPT=53 LEN=44";
        let flow = parse_line(line).unwrap().unwrap();
        assert_eq!(flow.protocol, Protocol::UDP);
        assert_eq!(flow.dst.port, 53);
        // First LEN (total packet length) wins.
        assert_eq!(flow.bytes, 64);
    }

    #[test]
    fn line_without_src_dst_returns_none() {
        // An iptables LOG line for ICMP echo doesn't have SRC/DST
        // fields in some configurations.
        let line = "<134>Nov 17 10:11:12 unifi kernel: [LAN]IN= OUT= PROTO=ICMP TYPE=8 CODE=0";
        let parsed = parse_line(line).unwrap();
        assert!(parsed.is_none());
    }

    #[test]
    fn unframed_line_still_parses() {
        // Some collectors strip the syslog frame before forwarding.
        let line = "SRC=10.0.0.1 DST=8.8.8.8 LEN=100 PROTO=TCP SPT=12345 DPT=443";
        let flow = parse_line(line).unwrap().unwrap();
        assert_eq!(flow.src.ip, IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1)));
        assert_eq!(flow.dst.port, 443);
    }

    #[test]
    fn non_iptables_kernel_message_errors() {
        let line = "<134>Nov 17 10:11:12 unifi kernel: EXT4-fs (sda1): mounted filesystem";
        // No key=value fields at all (no `=`).
        assert!(matches!(
            parse_line(line),
            Err(ParseError::NoIptablesPayload)
        ));
    }

    #[test]
    fn numeric_protocol_falls_through() {
        // Some less-common exporters send the protocol as a number.
        let line = "SRC=10.0.0.1 DST=10.0.0.2 PROTO=89 SPT=0 DPT=0 LEN=64";
        let flow = parse_line(line).unwrap().unwrap();
        assert_eq!(flow.protocol, Protocol(89)); // OSPF
    }
}
