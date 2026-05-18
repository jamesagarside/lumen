//! Time-scrubber: reconstruct a topology snapshot at an arbitrary historical
//! moment from the rolling raw-flow buffer (#26, built on #20).
//!
//! ## Two timelines in one snapshot
//!
//! At a playhead `t` the user expects two things from the graph:
//!
//! 1. The *topology* that existed at `t` — every node/edge that had been seen
//!    at least once up to that moment, with cumulative totals (`bytes_total`,
//!    `packets_total`, `flows_seen`) accurate as of `t`.
//! 2. The *animation rate* — how fast bytes were flowing **around** `t`. The
//!    spec calls for a "5-second window centred on the playhead, on repeat",
//!    which we express as a flat-window average rather than the live
//!    engine's EMA. Flat windows are deterministic against time-direction
//!    scrubbing; EMA would give different rates depending on which way the
//!    user dragged.
//!
//! We make a single pass over the rolling buffer per scrub request, since
//! the buffer is bounded (default 15 min × 512 MB ≈ a few hundred k flows)
//! and reconstruction stays well under a frame.

use std::collections::BTreeMap;
use std::time::{Duration, SystemTime};

use lumen_core::{is_internal_ip, Edge, EdgeId, Node, NodeId, Snapshot};

use crate::brand;
use crate::state::RollingBuffer;
use crate::topology_store::TopologyStore;

/// Width of the rate window centred on the playhead. Five seconds matches
/// the issue spec and lines up with the live engine's 5 s EMA half-life so
/// "now" and "scrubbed to now" produce comparable rate magnitudes.
pub const DEFAULT_RATE_WINDOW: Duration = Duration::from_secs(5);

/// Build a `Snapshot` representing the topology as of `t`. Nodes and edges
/// are present iff at least one flow involving them arrived at or before
/// `t`. Per-edge `bytes_per_sec` is averaged over `[t - rate_window/2,
/// t + rate_window/2]`; flows that arrived after `t` contribute *only* to
/// the rate, never to topology presence or cumulative totals.
///
/// `store` is consulted (when supplied) to merge in persisted node labels
/// and positions, the same way the live engine does at first sight.
pub fn reconstruct_at(
    buffer: &RollingBuffer,
    t: SystemTime,
    rate_window: Duration,
    store: Option<&TopologyStore>,
) -> Snapshot {
    let half = rate_window / 2;
    let rate_start = t.checked_sub(half).unwrap_or(SystemTime::UNIX_EPOCH);
    let rate_end = t.checked_add(half).unwrap_or(t);

    // Pull the union of "everything that contributes to topology at t" and
    // "everything that contributes to the rate window". We then split per
    // flow on whether `arrived <= t` (topology) vs in the rate window.
    let earliest = buffer.earliest().unwrap_or(SystemTime::UNIX_EPOCH);
    let scan_start = earliest.min(rate_start);
    let scan_end = t.max(rate_end);

    let mut nodes: BTreeMap<NodeId, NodeAccum> = BTreeMap::new();
    let mut edges: BTreeMap<EdgeId, EdgeAccum> = BTreeMap::new();

    for (flow, arrived) in buffer.query_stamped(scan_start, scan_end) {
        let in_topology = arrived <= t;
        let in_rate_window = arrived >= rate_start && arrived <= rate_end;
        if !in_topology && !in_rate_window {
            continue;
        }

        // Endpoint presence + first/last seen are topology concerns.
        if in_topology {
            for ip in [flow.src.ip, flow.dst.ip] {
                let id = NodeId(ip);
                nodes
                    .entry(id)
                    .and_modify(|n| {
                        n.first_seen = n.first_seen.min(arrived);
                        n.last_seen = n.last_seen.max(arrived);
                    })
                    .or_insert(NodeAccum {
                        first_seen: arrived,
                        last_seen: arrived,
                    });
            }
        }

        let edge_id = EdgeId {
            src: NodeId(flow.src.ip),
            dst: NodeId(flow.dst.ip),
        };
        let acc = edges.entry(edge_id).or_default();
        if in_topology {
            acc.first_seen = acc.first_seen.map(|s| s.min(arrived)).or(Some(arrived));
            acc.last_seen = acc.last_seen.map(|s| s.max(arrived)).or(Some(arrived));
            acc.bytes_total = acc.bytes_total.saturating_add(flow.bytes);
            acc.packets_total = acc.packets_total.saturating_add(flow.packets);
            acc.flows_seen += 1;
        }
        if in_rate_window {
            acc.window_bytes = acc.window_bytes.saturating_add(flow.bytes);
        }
    }

    // Drop edges that contributed only to the rate window — without
    // topology presence they aren't part of the graph at `t`.
    edges.retain(|_, acc| acc.first_seen.is_some());

    let rate_secs = rate_window.as_secs_f64().max(0.001);
    let mut out_nodes: Vec<Node> = nodes
        .into_iter()
        .map(|(id, acc)| {
            let internal = is_internal_ip(id.0);
            let brand = if internal {
                None
            } else {
                brand::classify(id.0).map(|s| s.to_string())
            };
            let (label, position) = store
                .map(|s| (s.lookup_label(&id), s.lookup_position(&id)))
                .unwrap_or((None, None));
            Node {
                id,
                is_internal: internal,
                first_seen: acc.first_seen,
                last_seen: acc.last_seen,
                label,
                position,
                brand,
            }
        })
        .collect();
    out_nodes.sort_by(|a, b| a.id.cmp(&b.id));

    let mut out_edges: Vec<Edge> = edges
        .into_iter()
        .map(|(id, acc)| Edge {
            id,
            first_seen: acc.first_seen.expect("retained -> Some"),
            last_seen: acc.last_seen.expect("retained -> Some"),
            bytes_total: acc.bytes_total,
            packets_total: acc.packets_total,
            flows_seen: acc.flows_seen,
            bytes_per_sec: acc.window_bytes as f64 / rate_secs,
        })
        .collect();
    out_edges.sort_by(|a, b| a.id.cmp(&b.id));

    Snapshot {
        generated_at: t,
        nodes: out_nodes,
        edges: out_edges,
    }
}

struct NodeAccum {
    first_seen: SystemTime,
    last_seen: SystemTime,
}

#[derive(Default)]
struct EdgeAccum {
    first_seen: Option<SystemTime>,
    last_seen: Option<SystemTime>,
    bytes_total: u64,
    packets_total: u64,
    flows_seen: u64,
    /// Bytes that arrived inside the rate window centred on the playhead.
    /// Divided by the window width to give `bytes_per_sec`.
    window_bytes: u64,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::RollingBufferBounds;
    use lumen_core::{Flow, FlowEndpoint, FlowSource, Protocol};
    use std::net::{IpAddr, Ipv4Addr};

    fn flow(src: &str, dst: &str, bytes: u64) -> Flow {
        Flow {
            source: FlowSource::NetflowV5,
            src: FlowEndpoint {
                ip: src.parse::<Ipv4Addr>().unwrap().into(),
                port: 12345,
            },
            dst: FlowEndpoint {
                ip: dst.parse::<Ipv4Addr>().unwrap().into(),
                port: 443,
            },
            protocol: Protocol::TCP,
            bytes,
            packets: 1,
            start: SystemTime::UNIX_EPOCH,
            end: SystemTime::UNIX_EPOCH,
        }
    }

    fn t(secs: u64) -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(secs)
    }

    #[test]
    fn empty_buffer_yields_empty_snapshot() {
        let buf = RollingBuffer::new(RollingBufferBounds::DEFAULT);
        let snap = reconstruct_at(&buf, t(100), DEFAULT_RATE_WINDOW, None);
        assert!(snap.nodes.is_empty());
        assert!(snap.edges.is_empty());
        assert_eq!(snap.generated_at, t(100));
    }

    #[test]
    fn topology_only_includes_flows_at_or_before_playhead() {
        let buf = RollingBuffer::new(RollingBufferBounds::DEFAULT);
        buf.append_at(flow("10.0.0.1", "8.8.8.8", 100), t(100));
        buf.append_at(flow("10.0.0.2", "1.1.1.1", 200), t(200));
        // Scrub to t=150 — only the first edge should be present.
        let snap = reconstruct_at(&buf, t(150), DEFAULT_RATE_WINDOW, None);
        assert_eq!(snap.nodes.len(), 2);
        assert_eq!(snap.edges.len(), 1);
        assert_eq!(snap.edges[0].id.src.0.to_string(), "10.0.0.1");
    }

    #[test]
    fn cumulative_totals_aggregate_up_to_playhead() {
        let buf = RollingBuffer::new(RollingBufferBounds::DEFAULT);
        for i in 0..5 {
            buf.append_at(flow("10.0.0.1", "8.8.8.8", 100), t(100 + i));
        }
        let snap = reconstruct_at(&buf, t(103), DEFAULT_RATE_WINDOW, None);
        let e = &snap.edges[0];
        // Flows at t=100,101,102,103 = 4 flows × 100B
        assert_eq!(e.flows_seen, 4);
        assert_eq!(e.bytes_total, 400);
    }

    #[test]
    fn rate_window_averages_bytes_around_playhead() {
        let buf = RollingBuffer::new(RollingBufferBounds::DEFAULT);
        // 1000B at t=98 (inside [97.5, 102.5]) and 1000B at t=101 (inside).
        // A flow at t=90 should not contribute to the rate.
        buf.append_at(flow("10.0.0.1", "8.8.8.8", 1000), t(90));
        buf.append_at(flow("10.0.0.1", "8.8.8.8", 1000), t(98));
        buf.append_at(flow("10.0.0.1", "8.8.8.8", 1000), t(101));
        let snap = reconstruct_at(&buf, t(100), DEFAULT_RATE_WINDOW, None);
        let bps = snap.edges[0].bytes_per_sec;
        // 2000 bytes / 5 s = 400 B/s
        assert!((bps - 400.0).abs() < 1e-6, "bps was {bps}");
    }

    #[test]
    fn rate_window_can_include_future_flows() {
        // A flow that arrives after the playhead but within the rate
        // window still drives the animation — the user is meant to see
        // "what's happening around this moment", not strictly before.
        let buf = RollingBuffer::new(RollingBufferBounds::DEFAULT);
        buf.append_at(flow("10.0.0.1", "8.8.8.8", 500), t(100)); // topology + rate
        buf.append_at(flow("10.0.0.1", "8.8.8.8", 500), t(101)); // rate only
        let snap = reconstruct_at(&buf, t(100), DEFAULT_RATE_WINDOW, None);
        assert_eq!(snap.edges[0].flows_seen, 1, "cumulative is pre-playhead");
        assert_eq!(snap.edges[0].bytes_total, 500);
        // Rate sees both: 1000 / 5 = 200
        assert!((snap.edges[0].bytes_per_sec - 200.0).abs() < 1e-6);
    }

    #[test]
    fn future_only_flow_does_not_create_topology() {
        let buf = RollingBuffer::new(RollingBufferBounds::DEFAULT);
        buf.append_at(flow("10.0.0.1", "8.8.8.8", 500), t(101));
        // Playhead at t=100, flow arrives at t=101 (inside rate window but
        // after playhead). The edge has never been seen "at or before t",
        // so it isn't part of the topology and shouldn't appear.
        let snap = reconstruct_at(&buf, t(100), DEFAULT_RATE_WINDOW, None);
        assert!(snap.edges.is_empty());
        assert!(snap.nodes.is_empty());
    }

    #[test]
    fn snapshot_is_sorted() {
        let buf = RollingBuffer::new(RollingBufferBounds::DEFAULT);
        buf.append_at(flow("10.0.0.5", "8.8.8.8", 100), t(100));
        buf.append_at(flow("10.0.0.1", "1.1.1.1", 100), t(101));
        buf.append_at(flow("10.0.0.3", "8.8.4.4", 100), t(102));
        let snap = reconstruct_at(&buf, t(200), DEFAULT_RATE_WINDOW, None);
        let ids: Vec<_> = snap.nodes.iter().map(|n| n.id.clone()).collect();
        let mut sorted = ids.clone();
        sorted.sort();
        assert_eq!(ids, sorted, "nodes must be sorted by NodeId");
    }

    #[test]
    fn internal_external_classification_matches_live_engine() {
        let buf = RollingBuffer::new(RollingBufferBounds::DEFAULT);
        buf.append_at(flow("192.168.1.10", "1.1.1.1", 100), t(100));
        let snap = reconstruct_at(&buf, t(100), DEFAULT_RATE_WINDOW, None);
        let internal = snap
            .nodes
            .iter()
            .find(|n| n.id.0 == IpAddr::from([192, 168, 1, 10]))
            .unwrap();
        let external = snap
            .nodes
            .iter()
            .find(|n| n.id.0 == IpAddr::from([1, 1, 1, 1]))
            .unwrap();
        assert!(internal.is_internal);
        assert!(!external.is_internal);
    }
}
