//! Live State Engine: turns the flow stream into a maintained graph.
//!
//! Single source of truth for the current network topology. Consumes
//! `Flow` records, updates an in-memory graph keyed by interface IPs,
//! emits per-flow `Delta`s for downstream subscribers, and exposes
//! point-in-time `Snapshot`s on demand.
//!
//! Per CONTEXT.md §3, this is the in-RAM topology that gets snapshotted
//! to disk in #22 (Topology Store). For #7 we hold it purely in memory
//! — restart loses everything, which is the right scope for this slice.

pub mod diff;
pub mod rate;

use std::collections::BTreeMap;
use std::sync::{Arc, RwLock};
use std::time::{Duration, SystemTime};

use lumen_core::{is_internal_ip, Delta, Edge, EdgeId, Flow, Node, NodeId, Position, Snapshot};
use tokio::sync::broadcast;

use crate::brand;
use crate::metrics::{EVICTIONS, TOPOLOGY_EDGES, TOPOLOGY_NODES};
use crate::topology_store::TopologyStore;
use rate::RateMeter;

const DELTA_CHANNEL_CAPACITY: usize = 1024;
const DEFAULT_RATE_HALF_LIFE: Duration = Duration::from_secs(5);

#[derive(Clone)]
pub struct LiveStateEngine {
    inner: Arc<RwLock<EngineState>>,
    delta_tx: broadcast::Sender<Delta>,
    rate_half_life: Duration,
    topology_store: Option<TopologyStore>,
}

struct EngineState {
    nodes: BTreeMap<NodeId, Node>,
    edges: BTreeMap<EdgeId, EdgeRecord>,
}

/// Per-edge bookkeeping. Splits the wire-shape `Edge` from the
/// engine's internal state — the `RateMeter` can't be serialised
/// and shouldn't leak across the API boundary.
struct EdgeRecord {
    edge: Edge,
    rate: RateMeter,
}

impl LiveStateEngine {
    pub fn new() -> Self {
        Self::with_store(None)
    }

    pub fn with_store(topology_store: Option<TopologyStore>) -> Self {
        let (delta_tx, _) = broadcast::channel(DELTA_CHANNEL_CAPACITY);
        Self {
            inner: Arc::new(RwLock::new(EngineState {
                nodes: BTreeMap::new(),
                edges: BTreeMap::new(),
            })),
            delta_tx,
            rate_half_life: DEFAULT_RATE_HALF_LIFE,
            topology_store,
        }
    }

    /// Persist a user-supplied label for `id` and apply it to the
    /// in-memory node (if present). Returns the updated `Node` for
    /// the caller (HTTP handler) to send back as the response. An
    /// empty `label` removes the persisted entry.
    pub fn set_node_label(&self, id: &NodeId, label: &str) -> anyhow::Result<Option<Node>> {
        if let Some(store) = &self.topology_store {
            store.set_node_label(id, label)?;
        }
        let mut state = self.inner.write().expect("engine state poisoned");
        let label_opt = if label.is_empty() {
            None
        } else {
            Some(label.to_string())
        };
        Ok(state.nodes.get_mut(id).map(|n| {
            n.label = label_opt.clone();
            n.clone()
        }))
    }

    /// Persist a user-dragged position for `id` and apply it to the
    /// in-memory node (if present).
    pub fn set_node_position(
        &self,
        id: &NodeId,
        position: Position,
    ) -> anyhow::Result<Option<Node>> {
        if let Some(store) = &self.topology_store {
            store.set_node_position(id, position)?;
        }
        let mut state = self.inner.write().expect("engine state poisoned");
        Ok(state.nodes.get_mut(id).map(|n| {
            n.position = Some(position);
            n.clone()
        }))
    }

    /// Used by the wire protocol (#8) to push per-flow deltas to web
    /// clients without waiting for a snapshot tick.
    #[allow(dead_code)]
    pub fn subscribe(&self) -> broadcast::Receiver<Delta> {
        self.delta_tx.subscribe()
    }

    /// Ingest one flow. Updates internal state and emits one or more
    /// deltas (NodeAdded for new endpoints, EdgeAdded or EdgeUpdated
    /// for the flow itself).
    ///
    /// Note: `last_seen` tracks when *Lumen* saw the flow, not when
    /// the flow ended on the exporter. Eviction is about "what
    /// disappeared from our view", and exporter clock skew shouldn't
    /// drive that decision.
    pub fn ingest(&self, flow: &Flow) {
        let now = SystemTime::now();
        let mut emitted: Vec<Delta> = Vec::with_capacity(3);
        {
            let mut state = self.inner.write().expect("engine state poisoned");
            // Both endpoints — emit NodeAdded only on first sight.
            for ip in [flow.src.ip, flow.dst.ip] {
                let id = NodeId(ip);
                let entry = state.nodes.entry(id.clone());
                use std::collections::btree_map::Entry;
                match entry {
                    Entry::Vacant(v) => {
                        let (label, position) =
                            self.topology_store.as_ref().map_or((None, None), |s| {
                                (s.lookup_label(&id), s.lookup_position(&id))
                            });
                        let internal = is_internal_ip(ip);
                        // Only classify externals — internal IPs by
                        // definition don't have a meaningful brand.
                        let brand = if internal {
                            None
                        } else {
                            brand::classify(ip).map(|s| s.to_string())
                        };
                        let node = Node {
                            id: id.clone(),
                            is_internal: internal,
                            first_seen: now,
                            last_seen: now,
                            label,
                            position,
                            brand,
                        };
                        v.insert(node.clone());
                        emitted.push(Delta::NodeAdded(node));
                    }
                    Entry::Occupied(mut o) => {
                        o.get_mut().last_seen = now;
                    }
                }
            }

            let edge_id = EdgeId {
                src: NodeId(flow.src.ip),
                dst: NodeId(flow.dst.ip),
            };
            let flow_dur = flow
                .end
                .duration_since(flow.start)
                .unwrap_or(Duration::ZERO);

            use std::collections::btree_map::Entry;
            match state.edges.entry(edge_id.clone()) {
                Entry::Vacant(v) => {
                    let mut rate = RateMeter::new(self.rate_half_life, now);
                    rate.observe(flow.bytes, flow_dur, now);
                    let edge = Edge {
                        id: edge_id.clone(),
                        first_seen: now,
                        last_seen: now,
                        bytes_total: flow.bytes,
                        packets_total: flow.packets,
                        flows_seen: 1,
                        bytes_per_sec: rate.current(),
                    };
                    v.insert(EdgeRecord {
                        edge: edge.clone(),
                        rate,
                    });
                    emitted.push(Delta::EdgeAdded(edge));
                }
                Entry::Occupied(mut o) => {
                    let record = o.get_mut();
                    record.rate.observe(flow.bytes, flow_dur, now);
                    record.edge.bytes_total = record.edge.bytes_total.saturating_add(flow.bytes);
                    record.edge.packets_total =
                        record.edge.packets_total.saturating_add(flow.packets);
                    record.edge.flows_seen += 1;
                    record.edge.last_seen = now;
                    record.edge.bytes_per_sec = record.rate.current();
                    emitted.push(Delta::EdgeUpdated {
                        id: edge_id,
                        last_seen: record.edge.last_seen,
                        bytes_total: record.edge.bytes_total,
                        packets_total: record.edge.packets_total,
                        flows_seen: record.edge.flows_seen,
                        bytes_per_sec: record.edge.bytes_per_sec,
                    });
                }
            }
        }

        for delta in emitted {
            // Best-effort: send returns Err only when there are no
            // subscribers, which is fine — the state is still updated.
            let _ = self.delta_tx.send(delta);
        }
    }

    /// Build a wire-format snapshot of current state. Decays edge
    /// rates first so an edge that stopped sending shows its true
    /// (lower) current rate, not its peak.
    pub fn snapshot(&self) -> Snapshot {
        let now = SystemTime::now();
        let mut state = self.inner.write().expect("engine state poisoned");
        for record in state.edges.values_mut() {
            record.rate.decay_to(now);
            record.edge.bytes_per_sec = record.rate.current();
        }
        metrics::gauge!(TOPOLOGY_NODES).set(state.nodes.len() as f64);
        metrics::gauge!(TOPOLOGY_EDGES).set(state.edges.len() as f64);
        Snapshot {
            generated_at: now,
            nodes: state.nodes.values().cloned().collect(),
            edges: state.edges.values().map(|r| r.edge.clone()).collect(),
        }
    }

    /// Drop edges whose `last_seen` is older than `max_age`. Then drop
    /// any nodes that no longer participate in any edge AND whose own
    /// `last_seen` is past the threshold. Emits removal deltas for
    /// every dropped entity.
    pub fn evict_stale(&self, max_age: Duration) -> usize {
        let cutoff = SystemTime::now()
            .checked_sub(max_age)
            .unwrap_or(SystemTime::UNIX_EPOCH);
        let mut emitted = 0usize;
        let mut deltas: Vec<Delta> = Vec::new();

        {
            let mut state = self.inner.write().expect("engine state poisoned");
            let stale_edges: Vec<EdgeId> = state
                .edges
                .iter()
                .filter(|(_, r)| r.edge.last_seen < cutoff)
                .map(|(id, _)| id.clone())
                .collect();
            for id in stale_edges {
                state.edges.remove(&id);
                deltas.push(Delta::EdgeRemoved(id));
                emitted += 1;
            }

            let mut nodes_with_edges = std::collections::HashSet::new();
            for id in state.edges.keys() {
                nodes_with_edges.insert(id.src.clone());
                nodes_with_edges.insert(id.dst.clone());
            }
            let stale_nodes: Vec<NodeId> = state
                .nodes
                .iter()
                .filter(|(id, n)| n.last_seen < cutoff && !nodes_with_edges.contains(id))
                .map(|(id, _)| id.clone())
                .collect();
            for id in stale_nodes {
                state.nodes.remove(&id);
                deltas.push(Delta::NodeRemoved(id));
                emitted += 1;
            }
        }

        for d in deltas {
            let _ = self.delta_tx.send(d);
        }
        if emitted > 0 {
            metrics::counter!(EVICTIONS).increment(emitted as u64);
        }
        emitted
    }

    /// Number of `(nodes, edges)`. For tests and the `/metrics`
    /// endpoint when #11 lands.
    #[allow(dead_code)]
    pub fn size(&self) -> (usize, usize) {
        let state = self.inner.read().expect("engine state poisoned");
        (state.nodes.len(), state.edges.len())
    }
}

impl Default for LiveStateEngine {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lumen_core::{FlowEndpoint, FlowSource, Protocol};
    use std::net::{IpAddr, Ipv4Addr};
    use std::time::UNIX_EPOCH;

    fn flow(src: &str, dst: &str, bytes: u64) -> Flow {
        Flow {
            source: FlowSource::NetflowV5,
            src: FlowEndpoint {
                ip: IpAddr::V4(src.parse::<Ipv4Addr>().unwrap()),
                port: 12345,
            },
            dst: FlowEndpoint {
                ip: IpAddr::V4(dst.parse::<Ipv4Addr>().unwrap()),
                port: 443,
            },
            protocol: Protocol::TCP,
            bytes,
            packets: 1,
            start: UNIX_EPOCH,
            end: UNIX_EPOCH + Duration::from_millis(100),
        }
    }

    #[test]
    fn first_flow_creates_two_nodes_and_one_edge() {
        let e = LiveStateEngine::new();
        e.ingest(&flow("10.0.0.1", "8.8.8.8", 100));
        let s = e.snapshot();
        assert_eq!(s.nodes.len(), 2);
        assert_eq!(s.edges.len(), 1);
    }

    #[test]
    fn second_flow_same_pair_updates_edge_not_creates() {
        let e = LiveStateEngine::new();
        e.ingest(&flow("10.0.0.1", "8.8.8.8", 100));
        e.ingest(&flow("10.0.0.1", "8.8.8.8", 200));
        let s = e.snapshot();
        assert_eq!(s.edges.len(), 1);
        let edge = &s.edges[0];
        assert_eq!(edge.bytes_total, 300);
        assert_eq!(edge.flows_seen, 2);
    }

    #[test]
    fn opposite_direction_is_a_separate_edge() {
        let e = LiveStateEngine::new();
        e.ingest(&flow("10.0.0.1", "8.8.8.8", 100));
        e.ingest(&flow("8.8.8.8", "10.0.0.1", 50));
        let s = e.snapshot();
        assert_eq!(s.nodes.len(), 2);
        assert_eq!(s.edges.len(), 2);
    }

    #[test]
    fn ingest_emits_deltas() {
        let e = LiveStateEngine::new();
        let mut rx = e.subscribe();
        e.ingest(&flow("10.0.0.1", "8.8.8.8", 100));
        let mut received = Vec::new();
        while let Ok(d) = rx.try_recv() {
            received.push(d);
        }
        // Expect 2 NodeAdded + 1 EdgeAdded
        assert_eq!(received.len(), 3);
        assert!(matches!(received[0], Delta::NodeAdded(_)));
        assert!(matches!(received[1], Delta::NodeAdded(_)));
        assert!(matches!(received[2], Delta::EdgeAdded(_)));
    }

    #[test]
    fn nodes_classified_internal_or_external() {
        let e = LiveStateEngine::new();
        e.ingest(&flow("192.168.1.10", "1.1.1.1", 100));
        let s = e.snapshot();
        let internal = s
            .nodes
            .iter()
            .find(|n| n.id.0.to_string() == "192.168.1.10");
        let external = s.nodes.iter().find(|n| n.id.0.to_string() == "1.1.1.1");
        assert!(internal.unwrap().is_internal);
        assert!(!external.unwrap().is_internal);
    }

    #[test]
    fn snapshot_yields_sorted_collections() {
        let e = LiveStateEngine::new();
        // ingest in non-sorted order
        e.ingest(&flow("10.0.0.5", "8.8.8.8", 100));
        e.ingest(&flow("10.0.0.1", "8.8.4.4", 100));
        e.ingest(&flow("10.0.0.3", "1.1.1.1", 100));
        let s = e.snapshot();
        let ids: Vec<_> = s.nodes.iter().map(|n| n.id.clone()).collect();
        let mut sorted = ids.clone();
        sorted.sort();
        assert_eq!(ids, sorted, "snapshot.nodes must be sorted by NodeId");
    }

    #[test]
    fn evict_drops_stale_edges_and_orphaned_nodes() {
        let e = LiveStateEngine::new();
        e.ingest(&flow("10.0.0.1", "8.8.8.8", 100));
        // Sleep would slow tests; jam last_seen back manually instead.
        {
            let mut state = e.inner.write().unwrap();
            for record in state.edges.values_mut() {
                record.edge.last_seen = SystemTime::UNIX_EPOCH;
            }
            for n in state.nodes.values_mut() {
                n.last_seen = SystemTime::UNIX_EPOCH;
            }
        }
        let dropped = e.evict_stale(Duration::from_secs(1));
        // 1 edge + 2 nodes
        assert_eq!(dropped, 3);
        assert_eq!(e.size(), (0, 0));
    }

    #[test]
    fn evict_keeps_node_that_still_has_edges() {
        let e = LiveStateEngine::new();
        e.ingest(&flow("10.0.0.1", "8.8.8.8", 100));
        e.ingest(&flow("10.0.0.1", "1.1.1.1", 100));
        // Make one edge stale, leave the other fresh.
        {
            let mut state = e.inner.write().unwrap();
            let stale_id = EdgeId {
                src: NodeId("10.0.0.1".parse().unwrap()),
                dst: NodeId("8.8.8.8".parse().unwrap()),
            };
            state.edges.get_mut(&stale_id).unwrap().edge.last_seen = SystemTime::UNIX_EPOCH;
            state
                .nodes
                .get_mut(&NodeId("8.8.8.8".parse().unwrap()))
                .unwrap()
                .last_seen = SystemTime::UNIX_EPOCH;
        }
        let dropped = e.evict_stale(Duration::from_secs(1));
        // Stale edge dropped, 8.8.8.8 dropped (no other edges), but
        // 10.0.0.1 stays because it still has the 1.1.1.1 edge.
        assert_eq!(dropped, 2);
        let (n, ed) = e.size();
        assert_eq!(ed, 1);
        assert_eq!(n, 2);
    }
}
