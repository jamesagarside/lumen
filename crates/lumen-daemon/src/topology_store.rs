//! Persistent storage for the slow-changing parts of network state.
//!
//! Per CONTEXT.md §3, the topology — devices, interfaces, user
//! labels, integration-provided context, plugin permissions — is
//! exactly the thing we *do* persist (as opposed to raw flow records,
//! which are RAM-only). This module is the redb-backed home for it.
//!
//! v1 scope: just node labels. Other domains (plugin permissions,
//! integration metadata, etc.) land alongside their owning issues
//! using the same store and the same `Table` pattern.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result};
use lumen_core::NodeId;
use redb::{Database, ReadableTable, TableDefinition};

/// `node_labels`: NodeId-as-string → user-supplied display label.
const NODE_LABELS: TableDefinition<&str, &str> = TableDefinition::new("node_labels");

#[derive(Clone)]
pub struct TopologyStore {
    db: Arc<Database>,
    #[allow(dead_code)] // useful for logs + tests; not read in prod paths yet
    path: PathBuf,
}

impl TopologyStore {
    /// Open (or create) a topology DB at the given path. The parent
    /// directory must already exist.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref().to_path_buf();
        let db = Database::create(&path)
            .with_context(|| format!("opening topology store at {}", path.display()))?;
        // Ensure tables exist so subsequent reads don't error on
        // first run. A write txn that just opens the tables is the
        // cheapest way.
        let txn = db.begin_write().context("ensuring tables exist")?;
        {
            txn.open_table(NODE_LABELS)
                .context("opening node_labels table")?;
        }
        txn.commit().context("committing table init")?;

        Ok(Self {
            db: Arc::new(db),
            path,
        })
    }

    pub fn set_node_label(&self, id: &NodeId, label: &str) -> Result<()> {
        let txn = self.db.begin_write()?;
        {
            let mut table = txn.open_table(NODE_LABELS)?;
            let key = id.0.to_string();
            if label.is_empty() {
                table.remove(key.as_str())?;
            } else {
                table.insert(key.as_str(), label)?;
            }
        }
        txn.commit()?;
        Ok(())
    }

    /// Fast single-key lookup. Returns `None` if no label is stored,
    /// `None` on read errors (logged) — the engine never wants this
    /// to be fatal on the ingest hot path.
    pub fn lookup_label(&self, id: &NodeId) -> Option<String> {
        let txn = match self.db.begin_read() {
            Ok(t) => t,
            Err(e) => {
                tracing::warn!(error = %e, "topology store read failed");
                return None;
            }
        };
        let table = match txn.open_table(NODE_LABELS) {
            Ok(t) => t,
            Err(e) => {
                tracing::warn!(error = %e, "topology store table missing");
                return None;
            }
        };
        let key = id.0.to_string();
        table
            .get(key.as_str())
            .ok()
            .flatten()
            .map(|v| v.value().to_string())
    }

    /// Bulk-list everything currently labelled. Used by tests and by
    /// a future admin / export endpoint; not on the hot path.
    #[allow(dead_code)]
    pub fn list_node_labels(&self) -> Result<Vec<(NodeId, String)>> {
        let txn = self.db.begin_read()?;
        let table = txn.open_table(NODE_LABELS)?;
        let mut out = Vec::new();
        for entry in table.iter()? {
            let (k, v) = entry?;
            let id: std::net::IpAddr = k.value().parse().with_context(|| {
                format!("topology store has malformed NodeId key: {:?}", k.value())
            })?;
            out.push((NodeId(id), v.value().to_string()));
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{IpAddr, Ipv4Addr};
    use tempfile::tempdir;

    fn id(ip: &str) -> NodeId {
        NodeId(IpAddr::V4(ip.parse::<Ipv4Addr>().unwrap()))
    }

    #[test]
    fn round_trips_a_label() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("topology.redb");
        let store = TopologyStore::open(&path).unwrap();
        store.set_node_label(&id("10.0.0.1"), "router").unwrap();
        let labels = store.list_node_labels().unwrap();
        assert_eq!(labels, vec![(id("10.0.0.1"), "router".to_string())]);
    }

    #[test]
    fn survives_close_and_reopen() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("topology.redb");
        {
            let store = TopologyStore::open(&path).unwrap();
            store.set_node_label(&id("10.0.0.1"), "router").unwrap();
            store.set_node_label(&id("10.0.0.2"), "nas").unwrap();
        }
        // Drop everything, reopen.
        let store = TopologyStore::open(&path).unwrap();
        let mut labels = store.list_node_labels().unwrap();
        labels.sort_by(|a, b| a.0.cmp(&b.0));
        assert_eq!(labels.len(), 2);
        assert_eq!(labels[0].1, "router");
        assert_eq!(labels[1].1, "nas");
    }

    #[test]
    fn empty_label_removes_entry() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("topology.redb");
        let store = TopologyStore::open(&path).unwrap();
        store.set_node_label(&id("10.0.0.1"), "router").unwrap();
        store.set_node_label(&id("10.0.0.1"), "").unwrap();
        assert!(store.list_node_labels().unwrap().is_empty());
    }

    #[test]
    fn overwriting_replaces_the_label() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("topology.redb");
        let store = TopologyStore::open(&path).unwrap();
        store.set_node_label(&id("10.0.0.1"), "router").unwrap();
        store.set_node_label(&id("10.0.0.1"), "gateway").unwrap();
        let labels = store.list_node_labels().unwrap();
        assert_eq!(labels, vec![(id("10.0.0.1"), "gateway".to_string())]);
    }
}
