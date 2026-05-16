//! Graph state vocabulary: nodes, edges, snapshots, and the deltas
//! between them.
//!
//! These types are the contract between the Live State Engine (which
//! produces them) and downstream consumers (wire protocol, UI, plugins
//! eventually). They live in `lumen-core` rather than the daemon
//! because the wire protocol crate and plugin SDK both reference them.

use std::net::IpAddr;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

/// Identity of a graph node. v1 = IP address. When MAC-aware ingestion
/// (#16 syslog, integrations) lands, this becomes a sum type. Wrap it
/// in a newtype so adding variants doesn't break the public surface.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct NodeId(pub IpAddr);

impl From<IpAddr> for NodeId {
    fn from(ip: IpAddr) -> Self {
        Self(ip)
    }
}

/// User-supplied or layout-computed 2D coordinate. In Sigma's
/// normalised graph space (~ -1.0 to 1.0 with some overflow).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Position {
    pub x: f32,
    pub y: f32,
}

/// One observed network interface.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Node {
    pub id: NodeId,
    /// True if the IP is in an RFC1918 / loopback / link-local range.
    /// Hint for layout (`internal` clusters centre, `external` arc
    /// the perimeter — CONTEXT.md §8). Plugins will refine this later.
    pub is_internal: bool,
    pub first_seen: SystemTime,
    pub last_seen: SystemTime,
    /// User-supplied display label. Merged in from the topology
    /// store when present; falls back to the IP for display.
    /// `None` distinguishes "not labelled" from `Some("")`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// Persisted position from the topology store, if any. Empty for
    /// freshly-observed nodes; populated once the user drags them or
    /// an integration provides authoritative placement.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub position: Option<Position>,
    /// Recognised brand for external IPs ("Google", "Netflix",
    /// "Cloudflare"…) derived from a bundled CIDR table. None for
    /// internal IPs and for unrecognised externals. Display priority
    /// in the UI: `label` > `brand` > `id`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub brand: Option<String>,
}

/// Identity of an edge: directed `(src, dst)` pair. Two edges between
/// the same pair in opposite directions are distinct — that's what
/// powers the asymmetric two-line edge rendering (CONTEXT.md §8).
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct EdgeId {
    pub src: NodeId,
    pub dst: NodeId,
}

/// One observed flow direction between two interfaces, with cumulative
/// totals and the current EMA-smoothed rate.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Edge {
    pub id: EdgeId,
    pub first_seen: SystemTime,
    pub last_seen: SystemTime,
    pub bytes_total: u64,
    pub packets_total: u64,
    pub flows_seen: u64,
    /// Smoothed bytes/sec via EMA. See `state::rate` in the daemon.
    pub bytes_per_sec: f64,
}

/// Point-in-time view of the engine's state.
///
/// Invariant: `nodes` is sorted by `NodeId`, `edges` is sorted by
/// `EdgeId`. Producers (the Live State Engine) maintain this; the
/// `diff` function relies on it to merge in O(n+m).
///
/// Vec-of-records (rather than map-keyed-by-id) is the wire-friendly
/// shape — JSON can't key objects on structs, and downstream consumers
/// (UI, plugins) always rebuild their own indexed view to render.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Snapshot {
    pub generated_at: SystemTime,
    pub nodes: Vec<Node>,
    pub edges: Vec<Edge>,
}

impl Default for Snapshot {
    fn default() -> Self {
        Self {
            generated_at: UNIX_EPOCH,
            nodes: Vec::new(),
            edges: Vec::new(),
        }
    }
}

/// Incremental change between two snapshots. Wire protocol (#8) sends
/// these in batches; downstream consumers apply them to maintain a
/// mirror of the snapshot.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Delta {
    NodeAdded(Node),
    NodeRemoved(NodeId),
    EdgeAdded(Edge),
    EdgeUpdated {
        id: EdgeId,
        last_seen: SystemTime,
        bytes_total: u64,
        packets_total: u64,
        flows_seen: u64,
        bytes_per_sec: f64,
    },
    EdgeRemoved(EdgeId),
}

impl Delta {
    /// Apply this delta to a snapshot. Used for testing the
    /// `apply(snapshot_t, delta) == snapshot_t+1` invariant; downstream
    /// consumers (UI mirror) use the same logic.
    pub fn apply_to(&self, snapshot: &mut Snapshot) {
        match self {
            Delta::NodeAdded(node) => {
                match snapshot.nodes.binary_search_by(|n| n.id.cmp(&node.id)) {
                    Ok(i) => snapshot.nodes[i] = node.clone(),
                    Err(i) => snapshot.nodes.insert(i, node.clone()),
                }
            }
            Delta::NodeRemoved(id) => {
                if let Ok(i) = snapshot.nodes.binary_search_by(|n| n.id.cmp(id)) {
                    snapshot.nodes.remove(i);
                }
            }
            Delta::EdgeAdded(edge) => {
                match snapshot.edges.binary_search_by(|e| e.id.cmp(&edge.id)) {
                    Ok(i) => snapshot.edges[i] = edge.clone(),
                    Err(i) => snapshot.edges.insert(i, edge.clone()),
                }
            }
            Delta::EdgeUpdated {
                id,
                last_seen,
                bytes_total,
                packets_total,
                flows_seen,
                bytes_per_sec,
            } => {
                if let Ok(i) = snapshot.edges.binary_search_by(|e| e.id.cmp(id)) {
                    let edge = &mut snapshot.edges[i];
                    edge.last_seen = *last_seen;
                    edge.bytes_total = *bytes_total;
                    edge.packets_total = *packets_total;
                    edge.flows_seen = *flows_seen;
                    edge.bytes_per_sec = *bytes_per_sec;
                }
            }
            Delta::EdgeRemoved(id) => {
                if let Ok(i) = snapshot.edges.binary_search_by(|e| e.id.cmp(id)) {
                    snapshot.edges.remove(i);
                }
            }
        }
    }
}

/// True for RFC1918, loopback, link-local, IPv6 ULA, IPv6 link-local.
pub fn is_internal_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            v4.is_private() || v4.is_loopback() || v4.is_link_local() || v4.is_unspecified()
        }
        IpAddr::V6(v6) => {
            v6.is_loopback()
                || v6.is_unspecified()
                // ULA: fc00::/7
                || (v6.octets()[0] & 0xfe) == 0xfc
                // Link-local: fe80::/10
                || (v6.octets()[0] == 0xfe && (v6.octets()[1] & 0xc0) == 0x80)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{Ipv4Addr, Ipv6Addr};
    use std::time::UNIX_EPOCH;

    fn node(ip: &str, internal: bool) -> Node {
        Node {
            id: NodeId(ip.parse().unwrap()),
            is_internal: internal,
            first_seen: UNIX_EPOCH,
            last_seen: UNIX_EPOCH,
            label: None,
            position: None,
            brand: None,
        }
    }

    fn edge(src: &str, dst: &str) -> Edge {
        Edge {
            id: EdgeId {
                src: NodeId(src.parse().unwrap()),
                dst: NodeId(dst.parse().unwrap()),
            },
            first_seen: UNIX_EPOCH,
            last_seen: UNIX_EPOCH,
            bytes_total: 0,
            packets_total: 0,
            flows_seen: 0,
            bytes_per_sec: 0.0,
        }
    }

    #[test]
    fn classify_internal_ips() {
        assert!(is_internal_ip(IpAddr::V4(Ipv4Addr::new(192, 168, 1, 1))));
        assert!(is_internal_ip(IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1))));
        assert!(is_internal_ip(IpAddr::V4(Ipv4Addr::new(172, 16, 0, 1))));
        assert!(is_internal_ip(IpAddr::V4(Ipv4Addr::LOCALHOST)));
        assert!(!is_internal_ip(IpAddr::V4(Ipv4Addr::new(8, 8, 8, 8))));
        assert!(!is_internal_ip(IpAddr::V4(Ipv4Addr::new(140, 82, 121, 4))));

        assert!(is_internal_ip(IpAddr::V6(Ipv6Addr::LOCALHOST)));
        assert!(is_internal_ip(IpAddr::V6("fe80::1".parse().unwrap())));
        assert!(is_internal_ip(IpAddr::V6("fd00::1".parse().unwrap())));
        assert!(!is_internal_ip(IpAddr::V6("2606:4700::1".parse().unwrap())));
    }

    #[test]
    fn delta_apply_node_added() {
        let mut s = Snapshot::default();
        Delta::NodeAdded(node("10.0.0.1", true)).apply_to(&mut s);
        assert_eq!(s.nodes.len(), 1);
    }

    #[test]
    fn delta_apply_edge_added_then_updated() {
        let mut s = Snapshot::default();
        Delta::EdgeAdded(edge("10.0.0.1", "8.8.8.8")).apply_to(&mut s);
        assert_eq!(s.edges.len(), 1);

        Delta::EdgeUpdated {
            id: EdgeId {
                src: NodeId("10.0.0.1".parse().unwrap()),
                dst: NodeId("8.8.8.8".parse().unwrap()),
            },
            last_seen: UNIX_EPOCH,
            bytes_total: 1234,
            packets_total: 7,
            flows_seen: 2,
            bytes_per_sec: 100.0,
        }
        .apply_to(&mut s);

        let e = &s.edges[0];
        assert_eq!(e.bytes_total, 1234);
        assert_eq!(e.flows_seen, 2);
    }

    #[test]
    fn delta_apply_keeps_nodes_sorted() {
        let mut s = Snapshot::default();
        Delta::NodeAdded(node("10.0.0.5", true)).apply_to(&mut s);
        Delta::NodeAdded(node("10.0.0.1", true)).apply_to(&mut s);
        Delta::NodeAdded(node("10.0.0.3", true)).apply_to(&mut s);
        let ids: Vec<_> = s.nodes.iter().map(|n| n.id.clone()).collect();
        let mut sorted = ids.clone();
        sorted.sort();
        assert_eq!(ids, sorted);
    }

    #[test]
    fn delta_apply_edge_update_on_missing_is_noop() {
        let mut s = Snapshot::default();
        Delta::EdgeUpdated {
            id: EdgeId {
                src: NodeId("10.0.0.1".parse().unwrap()),
                dst: NodeId("8.8.8.8".parse().unwrap()),
            },
            last_seen: UNIX_EPOCH,
            bytes_total: 1234,
            packets_total: 7,
            flows_seen: 2,
            bytes_per_sec: 100.0,
        }
        .apply_to(&mut s);
        assert!(s.edges.is_empty());
    }

    #[test]
    fn round_trip_through_json() {
        let mut s = Snapshot::default();
        Delta::NodeAdded(node("10.0.0.1", true)).apply_to(&mut s);
        Delta::EdgeAdded(edge("10.0.0.1", "8.8.8.8")).apply_to(&mut s);
        let json = serde_json::to_string(&s).unwrap();
        let back: Snapshot = serde_json::from_str(&json).unwrap();
        assert_eq!(s, back);
    }
}
