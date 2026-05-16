//! Operational metrics — Prometheus / OpenMetrics format.
//!
//! Counter/gauge naming follows the Prometheus convention: snake_case,
//! `_total` suffix for counters, base unit in the metric name where
//! relevant. Per CONTEXT.md §12, this is one of the two observability
//! surfaces we commit to (the other being structured logs via tracing).

use std::sync::OnceLock;

use anyhow::Result;
use metrics_exporter_prometheus::{PrometheusBuilder, PrometheusHandle};

// Metric names — kept as constants so the same identifier is used at
// the call site and in any future test that needs to assert on the
// rendered output.
pub const FLOWS_INGESTED: &str = "lumen_flows_ingested_total";
pub const TOPOLOGY_NODES: &str = "lumen_topology_nodes";
pub const TOPOLOGY_EDGES: &str = "lumen_topology_edges";
pub const EVICTIONS: &str = "lumen_evictions_total";
pub const WS_CLIENTS: &str = "lumen_ws_clients";
pub const HTTP_REQUESTS: &str = "lumen_http_requests_total";

static HANDLE: OnceLock<PrometheusHandle> = OnceLock::new();

/// Install the Prometheus recorder. Safe to call once at startup;
/// subsequent calls are a no-op (and warn).
pub fn install() -> Result<()> {
    let builder = PrometheusBuilder::new();
    let recorder = builder.build_recorder();
    let handle = recorder.handle();

    metrics::set_global_recorder(recorder)
        .map_err(|e| anyhow::anyhow!("failed to install global metrics recorder: {e}"))?;

    if HANDLE.set(handle).is_err() {
        tracing::warn!("metrics::install called more than once");
    }

    // Register metric descriptions up front so they appear in the
    // exposition output before they've been incremented.
    metrics::describe_counter!(
        FLOWS_INGESTED,
        "Total flow records ingested, by source protocol"
    );
    metrics::describe_gauge!(
        TOPOLOGY_NODES,
        "Nodes currently held by the Live State Engine"
    );
    metrics::describe_gauge!(
        TOPOLOGY_EDGES,
        "Edges currently held by the Live State Engine"
    );
    metrics::describe_counter!(
        EVICTIONS,
        "Total entities evicted by the stale-eviction sweep"
    );
    metrics::describe_gauge!(
        WS_CLIENTS,
        "Number of WebSocket clients currently connected"
    );
    metrics::describe_counter!(
        HTTP_REQUESTS,
        "Total HTTP requests served, by route + status"
    );

    Ok(())
}

/// Render the current metrics in the Prometheus / OpenMetrics text
/// format. Returns an empty string if `install()` wasn't called
/// (which shouldn't happen in production but keeps the route safe).
pub fn render() -> String {
    HANDLE.get().map(|h| h.render()).unwrap_or_default()
}
