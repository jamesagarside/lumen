use std::net::IpAddr;
use std::time::SystemTime;

use serde::{Deserialize, Serialize};

/// IP transport protocol number (IANA).
///
/// Stored as the raw protocol byte to preserve fidelity for less-common
/// protocols; convenience constants cover what the UI actually renders.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Protocol(pub u8);

impl Protocol {
    pub const ICMP: Self = Self(1);
    pub const TCP: Self = Self(6);
    pub const UDP: Self = Self(17);
    pub const ICMPV6: Self = Self(58);

    pub fn name(self) -> &'static str {
        match self.0 {
            1 => "icmp",
            6 => "tcp",
            17 => "udp",
            58 => "icmpv6",
            _ => "other",
        }
    }
}

/// One endpoint of a flow.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct FlowEndpoint {
    pub ip: IpAddr,
    pub port: u16,
}

/// Where this flow record came from. Useful for debugging and routing
/// decisions in later pipeline stages.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FlowSource {
    NetflowV5,
    NetflowV9,
    Ipfix,
    Syslog,
    JsonHttp,
    Otlp,
}

/// Normalised representation of a single network flow.
///
/// Every ingestion path produces this. The Live State Engine consumes a
/// stream of `Flow` and updates the topology graph from it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Flow {
    pub source: FlowSource,
    pub src: FlowEndpoint,
    pub dst: FlowEndpoint,
    pub protocol: Protocol,
    pub bytes: u64,
    pub packets: u64,
    /// When the first packet of this flow was observed.
    pub start: SystemTime,
    /// When the last packet of this flow was observed.
    pub end: SystemTime,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::Ipv4Addr;
    use std::time::UNIX_EPOCH;

    #[test]
    fn protocol_names() {
        assert_eq!(Protocol::TCP.name(), "tcp");
        assert_eq!(Protocol::UDP.name(), "udp");
        assert_eq!(Protocol(99).name(), "other");
    }

    #[test]
    fn flow_round_trips_through_json() {
        let flow = Flow {
            source: FlowSource::NetflowV5,
            src: FlowEndpoint {
                ip: IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1)),
                port: 12345,
            },
            dst: FlowEndpoint {
                ip: IpAddr::V4(Ipv4Addr::new(8, 8, 8, 8)),
                port: 53,
            },
            protocol: Protocol::UDP,
            bytes: 256,
            packets: 2,
            start: UNIX_EPOCH,
            end: UNIX_EPOCH,
        };
        let json = serde_json::to_string(&flow).unwrap();
        let back: Flow = serde_json::from_str(&json).unwrap();
        assert_eq!(flow, back);
    }
}
