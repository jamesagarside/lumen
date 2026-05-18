use std::net::IpAddr;
use std::sync::Arc;

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, Request, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::Json;
use lumen_core::{Flow, FlowSource, Node, NodeId, Position, Snapshot};
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast::error::RecvError;
use tracing::{debug, warn};

use crate::auth::{AuthStore, User, UserSummary};
use crate::detections::DetectionBus;
use crate::ingest::FlowBus;
use crate::integrations::diagnostics::IntegrationDiagnostic;
use crate::integrations::supervisor::{self as supervisor, IntegrationSupervisor};
use crate::metrics::{self as app_metrics, DETECTIONS, FLOWS_INGESTED, WS_CLIENTS};
use crate::settings::{self as settings_mod, SettingsStore};
use crate::state::{LiveStateEngine, RollingBuffer};

const SESSION_COOKIE: &str = "lumen_session";

#[derive(Clone)]
pub struct AppState {
    pub bus: FlowBus,
    pub detections: DetectionBus,
    pub engine: LiveStateEngine,
    pub auth: AuthStore,
    /// If `Some`, every POST /ingest/flows must include a matching
    /// `X-Api-Key` header. If `None`, ingestion is open.
    pub ingest_api_key: Option<Arc<str>>,
    pub settings: SettingsStore,
    pub supervisor: IntegrationSupervisor,
    /// Time- and memory-bounded ring of raw flows (#20). Powers the
    /// time-scrubber (#26) and feeds the rollup engine (#21). Wired
    /// up now so producers feed it from day one; readers land with #26.
    #[allow(dead_code)]
    pub raw_buffer: RollingBuffer,
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

/// Prometheus / OpenMetrics scrape endpoint. Returns the current
/// recorder render — plain text in the Prometheus exposition format.
pub async fn metrics() -> impl IntoResponse {
    (
        [("content-type", "text/plain; version=0.0.4")],
        app_metrics::render(),
    )
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

// ── Detection events ────────────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct EventsQuery {
    /// Cap on the number of events returned. Default 100; max 500
    /// (the ring buffer size).
    #[serde(default)]
    pub limit: Option<usize>,
}

/// `GET /events` — recent detection events, newest first.
pub async fn list_events(
    State(state): State<AppState>,
    axum::extract::Query(q): axum::extract::Query<EventsQuery>,
) -> Json<Vec<lumen_core::DetectionEvent>> {
    let limit = q.limit.unwrap_or(100).min(500);
    Json(state.detections.recent(limit))
}

/// `POST /events` — push one or more detection events into lumen.
/// Producer plugins / integrations / curl users / scripts post here.
pub async fn ingest_events(
    State(state): State<AppState>,
    Json(events): Json<Vec<lumen_core::DetectionEvent>>,
) -> Result<(StatusCode, Json<IngestAccepted>), (StatusCode, Json<ApiError>)> {
    let accepted = events.len();
    for ev in events {
        // Clamp severity defensively — clients can send any u8.
        let mut ev = ev;
        ev.severity = lumen_core::Severity::clamped(ev.severity.0);
        let source = ev.agent.type_.clone();
        let severity_name = ev.severity.name();
        state.detections.publish(ev);
        metrics::counter!(DETECTIONS, "source" => source, "severity" => severity_name).increment(1);
    }
    Ok((StatusCode::ACCEPTED, Json(IngestAccepted { accepted })))
}

// ── Auth routes ─────────────────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct LoginRequest {
    pub email: String,
    pub password: String,
}

#[derive(Serialize)]
pub struct LoginResponse {
    pub user: UserSummary,
    pub capabilities: Vec<&'static str>,
}

pub async fn login(
    State(state): State<AppState>,
    Json(req): Json<LoginRequest>,
) -> Result<Response, (StatusCode, Json<ApiError>)> {
    let user = state.auth.find_by_email(&req.email).map_err(internal_err)?;
    let Some(user) = user else {
        return Err((
            StatusCode::UNAUTHORIZED,
            Json(ApiError {
                error: "invalid email or password".to_string(),
            }),
        ));
    };
    if !crate::auth::verify_password(&req.password, &user.password_hash) {
        return Err((
            StatusCode::UNAUTHORIZED,
            Json(ApiError {
                error: "invalid email or password".to_string(),
            }),
        ));
    }
    let token = state.auth.create_session(&user.id).map_err(internal_err)?;
    let cookie = session_cookie(&token, false);
    let body = LoginResponse {
        user: (&user).into(),
        capabilities: user.role.capabilities().to_vec(),
    };
    let mut response = Json(body).into_response();
    response
        .headers_mut()
        .insert(header::SET_COOKIE, cookie.parse().unwrap());
    Ok(response)
}

pub async fn logout(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Some(token) = session_token_from(&headers) {
        let _ = state.auth.delete_session(&token);
    }
    let mut response = (StatusCode::NO_CONTENT, ()).into_response();
    response.headers_mut().insert(
        header::SET_COOKIE,
        session_cookie("", true).parse().unwrap(),
    );
    response
}

/// `GET /auth/me` — returns the current user + capabilities, or 401.
/// The UI polls this on load to decide whether to show the login
/// screen or the app.
pub async fn me(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<LoginResponse>, (StatusCode, Json<ApiError>)> {
    let Some(token) = session_token_from(&headers) else {
        return Err((
            StatusCode::UNAUTHORIZED,
            Json(ApiError {
                error: "not authenticated".to_string(),
            }),
        ));
    };
    let user = state.auth.lookup_session(&token).map_err(internal_err)?;
    let Some(user) = user else {
        return Err((
            StatusCode::UNAUTHORIZED,
            Json(ApiError {
                error: "not authenticated".to_string(),
            }),
        ));
    };
    Ok(Json(LoginResponse {
        user: (&user).into(),
        capabilities: user.role.capabilities().to_vec(),
    }))
}

fn internal_err<E: std::fmt::Display>(e: E) -> (StatusCode, Json<ApiError>) {
    warn!(error = %e, "internal error");
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(ApiError {
            error: "internal error".to_string(),
        }),
    )
}

fn session_cookie(value: &str, expire: bool) -> String {
    let max_age = if expire { 0 } else { 7 * 24 * 60 * 60 };
    format!("{SESSION_COOKIE}={value}; Path=/; HttpOnly; SameSite=Lax; Max-Age={max_age}")
}

fn session_token_from(headers: &HeaderMap) -> Option<String> {
    let header = headers.get(header::COOKIE)?.to_str().ok()?;
    for part in header.split(';') {
        let part = part.trim();
        if let Some(rest) = part.strip_prefix(&format!("{SESSION_COOKIE}=")) {
            return Some(rest.to_string());
        }
    }
    None
}

// ── Auth middleware ─────────────────────────────────────────────────────────

/// Resolve the request's session cookie to a User and attach it as
/// a request extension. Always allows the request through — gating
/// is done by `require_capability` on individual routes.
pub async fn auth_extension(
    State(state): State<AppState>,
    headers: HeaderMap,
    mut req: Request,
    next: Next,
) -> Response {
    if let Some(token) = session_token_from(&headers) {
        if let Ok(Some(user)) = state.auth.lookup_session(&token) {
            req.extensions_mut().insert(user);
        }
    }
    next.run(req).await
}

/// Capability-gating middleware factory. Used per-route:
/// `.route_layer(middleware::from_fn(require_cap(cap::EDIT_DEVICE_LABELS)))`.
pub fn require_cap(
    capability: &'static str,
) -> impl Fn(Request, Next) -> std::pin::Pin<Box<dyn Future<Output = Response> + Send>> + Clone {
    move |req: Request, next: Next| {
        Box::pin(async move {
            let Some(user) = req.extensions().get::<User>() else {
                return (
                    StatusCode::UNAUTHORIZED,
                    Json(ApiError {
                        error: "not authenticated".to_string(),
                    }),
                )
                    .into_response();
            };
            if !user.role.has(capability) {
                return (
                    StatusCode::FORBIDDEN,
                    Json(ApiError {
                        error: format!("missing required capability: {capability}"),
                    }),
                )
                    .into_response();
            }
            next.run(req).await
        })
    }
}

use std::future::Future;

#[derive(Serialize)]
pub struct IngestAccepted {
    pub accepted: usize,
}

/// Accept a JSON array of pre-parsed `Flow` records and publish each
/// onto the bus. The escape hatch for any data source we don't have
/// a native parser for: eBPF flow generators, custom collectors,
/// Suricata's eve.json (with a small adapter), etc.
///
/// Behaviour:
/// - If `LUMEN_INGEST_API_KEY` is set, an `X-Api-Key` header that
///   matches it is required. 401 otherwise.
/// - Body must be a JSON array of `Flow`. Single objects, NDJSON,
///   etc. are not supported (use multiple POSTs).
/// - Each accepted flow's `source` is normalised to
///   `FlowSource::JsonHttp` so downstream consumers can distinguish
///   it from native-protocol-parsed flows.
pub async fn ingest_flows(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(flows): Json<Vec<Flow>>,
) -> Result<(StatusCode, Json<IngestAccepted>), (StatusCode, Json<ApiError>)> {
    if let Some(expected) = &state.ingest_api_key {
        let supplied = headers
            .get("x-api-key")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        if supplied != expected.as_ref() {
            return Err((
                StatusCode::UNAUTHORIZED,
                Json(ApiError {
                    error: "missing or invalid X-Api-Key".to_string(),
                }),
            ));
        }
    }
    let accepted = flows.len();
    for mut f in flows {
        f.source = FlowSource::JsonHttp;
        state.bus.publish(f);
    }
    metrics::counter!(FLOWS_INGESTED, "protocol" => "json_http").increment(accepted as u64);
    Ok((StatusCode::ACCEPTED, Json(IngestAccepted { accepted })))
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

/// RAII gauge: bumps `lumen_ws_clients` on construction, decrements
/// on drop. Guarantees the gauge is balanced even if the WS handler
/// panics or returns early.
struct WsClientGauge;

impl WsClientGauge {
    fn new() -> Self {
        metrics::gauge!(WS_CLIENTS).increment(1.0);
        Self
    }
}

impl Drop for WsClientGauge {
    fn drop(&mut self) {
        metrics::gauge!(WS_CLIENTS).decrement(1.0);
    }
}

// ── Admin: integration settings ─────────────────────────────────────────────

#[derive(Serialize)]
pub struct IntegrationStatusResponse {
    /// Stable id, matches `crate::settings::id::*` / supervisor kind.
    pub id: &'static str,
    /// Non-secret fields (URL, site, username, etc.) as JSON.
    pub plain: serde_json::Value,
    /// Where the plain config was sourced from (db, env, or none).
    pub plain_source: Option<settings_mod::Source>,
    /// True if a secret value is configured. The value itself is
    /// never returned.
    pub secret_configured: bool,
    pub secret_source: Option<settings_mod::Source>,
    /// True if the supervisor currently has a running task for this
    /// integration. False = either no config or the task exited.
    pub running: bool,
    /// Most recent poll outcome — when, success/failure, item counts,
    /// optional one-line summary of the latest item seen. Used by the
    /// admin UI's diagnostics panel.
    pub diagnostics: IntegrationDiagnostic,
}

/// `GET /admin/settings` — full snapshot of integration config (plain
/// values + secret-set flags + runtime status + diagnostics). The
/// admin UI polls this on page load and after every mutation.
pub async fn list_settings(
    State(state): State<AppState>,
) -> Result<Json<Vec<IntegrationStatusResponse>>, (StatusCode, Json<ApiError>)> {
    let statuses = state.settings.all_statuses().map_err(internal_err)?;
    let diagnostics = state.supervisor.diagnostics();
    let resp: Vec<IntegrationStatusResponse> = statuses
        .into_iter()
        .map(|s| IntegrationStatusResponse {
            running: state.supervisor.is_running(s.id),
            diagnostics: diagnostics.get(s.id),
            id: s.id,
            plain: s.plain,
            plain_source: s.plain_source,
            secret_configured: s.secret_configured,
            secret_source: s.secret_source,
        })
        .collect();
    Ok(Json(resp))
}

#[derive(Deserialize)]
pub struct UnifiLabelsPayload {
    /// Network Integration API sites URL. Empty string = clear.
    #[serde(default)]
    pub url: Option<String>,
    /// API key. Omitted = leave existing value in place; empty string
    /// = clear.
    #[serde(default)]
    pub api_key: Option<String>,
}

#[derive(Deserialize)]
pub struct UnifiIpsPayload {
    #[serde(default)]
    pub controller_url: Option<String>,
    #[serde(default)]
    pub username: Option<String>,
    #[serde(default)]
    pub password: Option<String>,
    #[serde(default)]
    pub site: Option<String>,
}

#[derive(Deserialize)]
pub struct WebhookPayload {
    #[serde(default)]
    pub url: Option<String>,
}

/// `PUT /admin/settings/unifi_labels`
pub async fn put_unifi_labels(
    State(state): State<AppState>,
    user_ext: Option<axum::extract::Extension<User>>,
    Json(p): Json<UnifiLabelsPayload>,
) -> Result<StatusCode, (StatusCode, Json<ApiError>)> {
    state
        .settings
        .set_unifi_labels(p.url, p.api_key)
        .map_err(internal_err)?;
    audit_settings_write(user_ext.as_deref(), "unifi_labels", "update");
    state
        .supervisor
        .reload_unifi_labels()
        .map_err(internal_err)?;
    Ok(StatusCode::NO_CONTENT)
}

/// `DELETE /admin/settings/unifi_labels`
pub async fn delete_unifi_labels(
    State(state): State<AppState>,
    user_ext: Option<axum::extract::Extension<User>>,
) -> Result<StatusCode, (StatusCode, Json<ApiError>)> {
    state.settings.clear_unifi_labels().map_err(internal_err)?;
    audit_settings_write(user_ext.as_deref(), "unifi_labels", "clear");
    state
        .supervisor
        .reload_unifi_labels()
        .map_err(internal_err)?;
    Ok(StatusCode::NO_CONTENT)
}

/// `PUT /admin/settings/unifi_ips`
pub async fn put_unifi_ips(
    State(state): State<AppState>,
    user_ext: Option<axum::extract::Extension<User>>,
    Json(p): Json<UnifiIpsPayload>,
) -> Result<StatusCode, (StatusCode, Json<ApiError>)> {
    state
        .settings
        .set_unifi_ips(p.controller_url, p.username, p.password, p.site)
        .map_err(internal_err)?;
    audit_settings_write(user_ext.as_deref(), "unifi_ips", "update");
    state.supervisor.reload_unifi_ips().map_err(internal_err)?;
    Ok(StatusCode::NO_CONTENT)
}

/// `DELETE /admin/settings/unifi_ips`
pub async fn delete_unifi_ips(
    State(state): State<AppState>,
    user_ext: Option<axum::extract::Extension<User>>,
) -> Result<StatusCode, (StatusCode, Json<ApiError>)> {
    state.settings.clear_unifi_ips().map_err(internal_err)?;
    audit_settings_write(user_ext.as_deref(), "unifi_ips", "clear");
    state.supervisor.reload_unifi_ips().map_err(internal_err)?;
    Ok(StatusCode::NO_CONTENT)
}

/// `PUT /admin/settings/webhook`
pub async fn put_webhook(
    State(state): State<AppState>,
    user_ext: Option<axum::extract::Extension<User>>,
    Json(p): Json<WebhookPayload>,
) -> Result<StatusCode, (StatusCode, Json<ApiError>)> {
    state.settings.set_webhook(p.url).map_err(internal_err)?;
    audit_settings_write(user_ext.as_deref(), "webhook", "update");
    state.supervisor.reload_webhook().map_err(internal_err)?;
    Ok(StatusCode::NO_CONTENT)
}

/// `DELETE /admin/settings/webhook`
pub async fn delete_webhook(
    State(state): State<AppState>,
    user_ext: Option<axum::extract::Extension<User>>,
) -> Result<StatusCode, (StatusCode, Json<ApiError>)> {
    state.settings.clear_webhook().map_err(internal_err)?;
    audit_settings_write(user_ext.as_deref(), "webhook", "clear");
    state.supervisor.reload_webhook().map_err(internal_err)?;
    Ok(StatusCode::NO_CONTENT)
}

/// One-line structured audit log for every settings mutation. Never
/// logs secret values — `kind` is the integration id, `action` is
/// "update" or "clear", and the acting user_id is included if known.
fn audit_settings_write(user: Option<&User>, kind: &str, action: &str) {
    let user_id = user.map(|u| u.id.as_str()).unwrap_or("unknown");
    let email = user.map(|u| u.email.as_str()).unwrap_or("");
    tracing::info!(
        target: "lumen::audit",
        user_id,
        user_email = email,
        integration = kind,
        action,
        "settings change"
    );
    // Touch supervisor kind names so the const lookup table stays
    // exercised even when the audit log is the only consumer.
    let _ = (
        supervisor::kind::UNIFI_LABELS,
        supervisor::kind::UNIFI_IPS,
        supervisor::kind::WEBHOOK,
    );
}

async fn handle_ws_flows(mut socket: WebSocket, bus: FlowBus) {
    let mut rx = bus.subscribe();
    let _ws_client_guard = WsClientGauge::new();
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
