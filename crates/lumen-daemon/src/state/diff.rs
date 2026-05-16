//! Pure-function snapshot diff.
//!
//! Used by the wire protocol (#8) to rebase a slow client: the server
//! holds the client's last-acknowledged snapshot, and on resume sends
//! `diff(last, current)` instead of the full snapshot.
//!
//! Stateless by design — no per-client server state beyond the
//! reference snapshot itself, which means N clients = N references but
//! one diff implementation. Relies on the
//! `Snapshot.{nodes,edges}-are-sorted-by-id` invariant for O(n+m)
//! merging.

// Used by #8 wire protocol; tests cover the public function. Suppress
// dead-code warnings until the wire protocol module wires it in.
#![allow(dead_code)]

use std::cmp::Ordering;

use lumen_core::{Delta, Snapshot};

/// Compute the deltas needed to transform `prev` into `next`.
///
/// Output ordering: removals first, then additions, then updates.
/// Apply in order to mutate a snapshot in place.
pub fn diff(prev: &Snapshot, next: &Snapshot) -> Vec<Delta> {
    let mut out = Vec::new();

    // Edges first because applying NodeRemoved that still has edges
    // is awkward. Compute edge deltas, then node deltas; emit edge
    // removals before node removals so the client can apply
    // sequentially without intermediate inconsistency.
    let (edges_added, edges_updated, edges_removed) = merge_edges(&prev.edges, &next.edges);
    let (nodes_added, nodes_removed) = merge_nodes(&prev.nodes, &next.nodes);

    for d in edges_removed {
        out.push(d);
    }
    for d in nodes_removed {
        out.push(d);
    }
    for d in nodes_added {
        out.push(d);
    }
    for d in edges_added {
        out.push(d);
    }
    for d in edges_updated {
        out.push(d);
    }

    out
}

fn merge_nodes(prev: &[lumen_core::Node], next: &[lumen_core::Node]) -> (Vec<Delta>, Vec<Delta>) {
    let mut added = Vec::new();
    let mut removed = Vec::new();
    let mut i = 0;
    let mut j = 0;
    while i < prev.len() && j < next.len() {
        match prev[i].id.cmp(&next[j].id) {
            Ordering::Less => {
                removed.push(Delta::NodeRemoved(prev[i].id.clone()));
                i += 1;
            }
            Ordering::Greater => {
                added.push(Delta::NodeAdded(next[j].clone()));
                j += 1;
            }
            Ordering::Equal => {
                // Node updates aren't a delta type in v1 (last_seen
                // changes don't materially affect the graph; that's
                // an intentional choice to keep the wire quiet).
                i += 1;
                j += 1;
            }
        }
    }
    while i < prev.len() {
        removed.push(Delta::NodeRemoved(prev[i].id.clone()));
        i += 1;
    }
    while j < next.len() {
        added.push(Delta::NodeAdded(next[j].clone()));
        j += 1;
    }
    (added, removed)
}

fn merge_edges(
    prev: &[lumen_core::Edge],
    next: &[lumen_core::Edge],
) -> (Vec<Delta>, Vec<Delta>, Vec<Delta>) {
    let mut added = Vec::new();
    let mut updated = Vec::new();
    let mut removed = Vec::new();
    let mut i = 0;
    let mut j = 0;
    while i < prev.len() && j < next.len() {
        match prev[i].id.cmp(&next[j].id) {
            Ordering::Less => {
                removed.push(Delta::EdgeRemoved(prev[i].id.clone()));
                i += 1;
            }
            Ordering::Greater => {
                added.push(Delta::EdgeAdded(next[j].clone()));
                j += 1;
            }
            Ordering::Equal => {
                let p = &prev[i];
                let n = &next[j];
                if p.bytes_total != n.bytes_total
                    || p.packets_total != n.packets_total
                    || p.flows_seen != n.flows_seen
                    || p.bytes_per_sec != n.bytes_per_sec
                    || p.last_seen != n.last_seen
                {
                    updated.push(Delta::EdgeUpdated {
                        id: n.id.clone(),
                        last_seen: n.last_seen,
                        bytes_total: n.bytes_total,
                        packets_total: n.packets_total,
                        flows_seen: n.flows_seen,
                        bytes_per_sec: n.bytes_per_sec,
                    });
                }
                i += 1;
                j += 1;
            }
        }
    }
    while i < prev.len() {
        removed.push(Delta::EdgeRemoved(prev[i].id.clone()));
        i += 1;
    }
    while j < next.len() {
        added.push(Delta::EdgeAdded(next[j].clone()));
        j += 1;
    }
    (added, updated, removed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::LiveStateEngine;
    use lumen_core::{Flow, FlowEndpoint, FlowSource, Protocol};
    use std::net::{IpAddr, Ipv4Addr};
    use std::time::{Duration, UNIX_EPOCH};

    fn flow(src: &str, dst: &str, bytes: u64) -> Flow {
        Flow {
            source: FlowSource::NetflowV5,
            src: FlowEndpoint {
                ip: IpAddr::V4(src.parse::<Ipv4Addr>().unwrap()),
                port: 1,
            },
            dst: FlowEndpoint {
                ip: IpAddr::V4(dst.parse::<Ipv4Addr>().unwrap()),
                port: 2,
            },
            protocol: Protocol::TCP,
            bytes,
            packets: 1,
            start: UNIX_EPOCH,
            end: UNIX_EPOCH + Duration::from_millis(100),
        }
    }

    #[test]
    fn empty_to_empty_yields_no_deltas() {
        let prev = Snapshot::default();
        let next = Snapshot::default();
        assert!(diff(&prev, &next).is_empty());
    }

    #[test]
    fn applying_diff_recreates_next() {
        let e = LiveStateEngine::new();
        let prev = e.snapshot();
        e.ingest(&flow("10.0.0.1", "8.8.8.8", 100));
        e.ingest(&flow("10.0.0.2", "1.1.1.1", 200));
        let next = e.snapshot();

        let mut applied = prev;
        for delta in diff(&applied.clone(), &next) {
            delta.apply_to(&mut applied);
        }
        // The synthesised "applied" snapshot won't have the same
        // generated_at as `next` — compare just the contents.
        assert_eq!(applied.nodes, next.nodes);
        assert_eq!(applied.edges, next.edges);
    }

    #[test]
    fn applying_diff_handles_removals() {
        let e = LiveStateEngine::new();
        e.ingest(&flow("10.0.0.1", "8.8.8.8", 100));
        e.ingest(&flow("10.0.0.2", "1.1.1.1", 200));
        let prev = e.snapshot();
        // Force one edge stale.
        e.evict_stale(Duration::from_secs(0));
        let next = e.snapshot();

        let mut applied = prev.clone();
        for delta in diff(&prev, &next) {
            delta.apply_to(&mut applied);
        }
        assert_eq!(applied.nodes, next.nodes);
        assert_eq!(applied.edges, next.edges);
    }

    #[test]
    fn diff_detects_byte_growth_as_update() {
        let e = LiveStateEngine::new();
        e.ingest(&flow("10.0.0.1", "8.8.8.8", 100));
        let prev = e.snapshot();
        e.ingest(&flow("10.0.0.1", "8.8.8.8", 100));
        let next = e.snapshot();
        let deltas = diff(&prev, &next);
        assert_eq!(deltas.len(), 1);
        assert!(matches!(deltas[0], Delta::EdgeUpdated { .. }));
    }
}
