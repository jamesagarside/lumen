use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::State;
use axum::response::IntoResponse;
use axum::Json;
use lumen_core::Snapshot;
use serde::Serialize;
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
