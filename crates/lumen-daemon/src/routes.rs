use std::net::IpAddr;

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::Json;
use lumen_core::{Node, NodeId, Position, Snapshot};
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast::error::RecvError;
use tracing::{debug, warn};

use crate::ingest::FlowBus;
use crate::state::LiveStateEngine;

#[derive(Clone)]
pub struct AppState {
    pub bus: FlowBus,
    pub engine: LiveStateEngine,
}

#[derive(Serialize)]
pub struct VersionInfo {
    pub name: &'static str,
    pub version: &'static str,
    pub abi_version: &'static str,
}

pub async fn healthz() -> &'static str {
    "ok"
}

pub async fn version() -> Json<VersionInfo> {
    Json(VersionInfo {
        name: "lumen",
        version: env!("CARGO_PKG_VERSION"),
        abi_version: lumen_core::ABI_VERSION,
    })
}

/// Current topology snapshot. JSON for v1; replaced by binary
/// snapshot+delta WebSocket in #8.
pub async fn snapshot(State(state): State<AppState>) -> Json<Snapshot> {
    Json(state.engine.snapshot())
}

#[derive(Deserialize, Default)]
pub struct NodePatch {
    /// New label; an empty string removes any persisted label.
    /// Omitted entirely if the caller only wants to update position.
    #[serde(default)]
    pub label: Option<String>,
    /// New position in Sigma graph coordinates. Both fields required
    /// together. Omitted if the caller only wants to update label.
    #[serde(default)]
    pub position: Option<Position>,
}

#[derive(Serialize)]
pub struct ApiError {
    pub error: String,
}

/// Patch a node's user-editable fields. Any subset of {label,
/// position} can be supplied; updates apply atomically per field
/// against the topology store and the in-memory engine state.
/// 404 if the node isn't currently known — labels and positions for
/// never-seen nodes would just accumulate as orphans.
pub async fn patch_node(
    Path(id): Path<String>,
    State(state): State<AppState>,
    Json(patch): Json<NodePatch>,
) -> Result<Json<Node>, (StatusCode, Json<ApiError>)> {
    let ip: IpAddr = id.parse().map_err(|_| {
        (
            StatusCode::BAD_REQUEST,
            Json(ApiError {
                error: format!("'{id}' is not a valid IP address"),
            }),
        )
    })?;
    let node_id = NodeId(ip);

    if patch.label.is_none() && patch.position.is_none() {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(ApiError {
                error: "request body must include at least one of label or position".to_string(),
            }),
        ));
    }

    let mut updated: Option<Node> = None;

    if let Some(raw_label) = patch.label.as_deref() {
        let trimmed = raw_label.trim();
        if trimmed.len() > 128 {
            return Err((
                StatusCode::BAD_REQUEST,
                Json(ApiError {
                    error: "label must be 128 characters or fewer".to_string(),
                }),
            ));
        }
        match state.engine.set_node_label(&node_id, trimmed) {
            Ok(Some(node)) => updated = Some(node),
            Ok(None) => {
                return Err((
                    StatusCode::NOT_FOUND,
                    Json(ApiError {
                        error: format!("node {ip} is not currently in the topology"),
                    }),
                ));
            }
            Err(e) => {
                warn!(error = %e, ip = %ip, "failed to set node label");
                return Err((
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(ApiError {
                        error: "failed to persist label".to_string(),
                    }),
                ));
            }
        }
    }

    if let Some(position) = patch.position {
        match state.engine.set_node_position(&node_id, position) {
            Ok(Some(node)) => updated = Some(node),
            Ok(None) => {
                return Err((
                    StatusCode::NOT_FOUND,
                    Json(ApiError {
                        error: format!("node {ip} is not currently in the topology"),
                    }),
                ));
            }
            Err(e) => {
                warn!(error = %e, ip = %ip, "failed to set node position");
                return Err((
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(ApiError {
                        error: "failed to persist position".to_string(),
                    }),
                ));
            }
        }
    }

    // updated is always Some at this point — either label or position
    // succeeded, both checks return early otherwise.
    Ok(Json(updated.expect("at least one field updated")))
}

/// Live flow stream over WebSocket. Each message is a JSON-encoded
/// `lumen_core::Flow`. This is the deliberately-simple v1 wire format;
/// the binary snapshot+delta protocol lands in #8.
pub async fn ws_flows(ws: WebSocketUpgrade, State(state): State<AppState>) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_ws_flows(socket, state.bus))
}

async fn handle_ws_flows(mut socket: WebSocket, bus: FlowBus) {
    let mut rx = bus.subscribe();
    debug!("ws client connected");

    loop {
        tokio::select! {
            biased;

            // Drain any incoming frames so we notice client disconnects.
            // We don't act on client messages yet — that's #8.
            client_msg = socket.recv() => {
                match client_msg {
                    Some(Ok(Message::Close(_))) | None => {
                        debug!("ws client closed");
                        return;
                    }
                    Some(Err(e)) => {
                        debug!(error = %e, "ws client error");
                        return;
                    }
                    Some(Ok(_)) => continue,
                }
            }

            flow = rx.recv() => {
                match flow {
                    Ok(flow) => {
                        let json = match serde_json::to_string(&flow) {
                            Ok(s) => s,
                            Err(e) => {
                                warn!(error = %e, "flow serialise failed");
                                continue;
                            }
                        };
                        if socket.send(Message::Text(json)).await.is_err() {
                            return;
                        }
                    }
                    Err(RecvError::Lagged(n)) => {
                        warn!(missed = n, "ws client lagged on flow stream");
                    }
                    Err(RecvError::Closed) => {
                        return;
                    }
                }
            }
        }
    }
}
