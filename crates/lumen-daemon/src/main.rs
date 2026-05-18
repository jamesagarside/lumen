use anyhow::Context;
use std::sync::Arc;

use axum::middleware;
use axum::routing::{get, patch, post, put};
use axum::Router;
use tokio::net::TcpListener;
use tokio::signal;
use tower_http::services::ServeDir;
use tower_http::trace::TraceLayer;
use tracing::{error, info};

use crate::auth::{AuthStore, Role};
use crate::detections::DetectionBus;
use crate::ingest::{syslog, udp_listener, FlowBus};
use crate::integrations::supervisor::IntegrationSupervisor;
use crate::routes::AppState;
use crate::secret_store::SecretStore;
use crate::settings::SettingsStore;
use crate::state::LiveStateEngine;
use crate::topology_store::TopologyStore;

mod auth;
mod brand;
mod config;
mod detections;
mod ingest;
mod integrations;
mod metrics;
mod observability;
mod routes;
mod secret_store;
mod settings;
mod state;
mod topology_store;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Load .env if present — useful for dev (API keys, OTEL endpoint,
    // etc.) and harmless in container deployments where env vars
    // come from elsewhere. Errors are intentional — file not existing
    // is fine.
    let _ = dotenvy::dotenv();

    observability::init();
    metrics::install()?;

    let config = config::Config::from_env()?;

    info!(
        version = env!("CARGO_PKG_VERSION"),
        listen = %config.http_listen,
        ui_assets = ?config.ui_assets_dir,
        "lumen starting"
    );

    let bus = FlowBus::new();
    let detections = DetectionBus::new();
    let topology_store = open_topology_store(&config)?;
    let auth_store = AuthStore::new(topology_store.db()).context("opening auth store")?;
    bootstrap_admin(&auth_store, &config)?;
    let secrets =
        SecretStore::open(topology_store.db(), &config.data_dir).context("opening secret store")?;
    let settings = SettingsStore::new(topology_store.db(), secrets).context("opening settings")?;
    let engine = LiveStateEngine::with_store(Some(topology_store));
    let supervisor =
        IntegrationSupervisor::new(engine.clone(), detections.clone(), settings.clone());
    let state = AppState {
        bus: bus.clone(),
        detections: detections.clone(),
        engine: engine.clone(),
        auth: auth_store,
        ingest_api_key: config.ingest_api_key.as_deref().map(Arc::from),
        settings: settings.clone(),
        supervisor: supervisor.clone(),
    };

    // Engine consumes flows from the bus and maintains the topology.
    spawn_engine_ingest(bus.clone(), engine.clone());

    // Periodic eviction of stale edges and orphaned nodes.
    spawn_eviction(
        engine.clone(),
        config.edge_max_age,
        config.eviction_interval,
    );

    if let Some(addr) = config.netflow_v5_listen {
        let bus = bus.clone();
        tokio::spawn(async move {
            if let Err(e) = udp_listener::run(addr, bus).await {
                error!(listener = "netflow", error = %e, "ingest listener exited");
            }
        });
    }

    if let Some(addr) = config.syslog_listen {
        let bus = bus.clone();
        tokio::spawn(async move {
            if let Err(e) = syslog::run(addr, bus).await {
                error!(listener = "syslog", error = %e, "ingest listener exited");
            }
        });
    }

    // All third-party integrations (UniFi labels, UniFi IPS, outbound
    // webhook) are configured through the settings store now — admins
    // edit them via the Settings page in the UI, env vars are read as
    // a fallback so existing `.env` deployments keep working. The
    // supervisor reads the current config for each integration and
    // spawns it; later writes via the admin API trigger a respawn.
    supervisor.start_all();

    let app = build_router(&config, state);
    let listener = TcpListener::bind(config.http_listen)
        .await
        .with_context(|| format!("binding {}", config.http_listen))?;

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .context("serving HTTP")?;

    info!("lumen stopped");
    Ok(())
}

fn build_router(config: &config::Config, state: AppState) -> Router {
    let mut router = Router::new()
        // Always-open routes (operational; pre-auth).
        .route("/healthz", get(routes::healthz))
        .route("/version", get(routes::version))
        .route("/metrics", get(routes::metrics))
        // Auth surface.
        .route("/auth/login", post(routes::login))
        .route("/auth/logout", post(routes::logout))
        .route("/auth/me", get(routes::me))
        // Authed read surface (no capability check; auth_extension
        // attaches the user if a session is valid, frontend uses
        // /auth/me to gate UI).
        .route("/snapshot", get(routes::snapshot))
        .route("/ws/flows", get(routes::ws_flows))
        // Mutating endpoints — gated.
        .route(
            "/nodes/:id",
            patch(routes::patch_node).route_layer(middleware::from_fn(routes::require_cap(
                crate::auth::cap::EDIT_DEVICE_LABELS,
            ))),
        )
        .route(
            "/ingest/flows",
            post(routes::ingest_flows).route_layer(middleware::from_fn(routes::require_cap(
                crate::auth::cap::INGEST_FLOWS,
            ))),
        )
        .route("/events", get(routes::list_events))
        .route(
            "/ingest/events",
            post(routes::ingest_events).route_layer(middleware::from_fn(routes::require_cap(
                crate::auth::cap::INGEST_DETECTIONS,
            ))),
        )
        // Admin settings — read + write integration config from the UI.
        .route(
            "/admin/settings",
            get(routes::list_settings).route_layer(middleware::from_fn(routes::require_cap(
                crate::auth::cap::MANAGE_SETTINGS,
            ))),
        )
        .route(
            "/admin/settings/unifi_labels",
            put(routes::put_unifi_labels)
                .delete(routes::delete_unifi_labels)
                .route_layer(middleware::from_fn(routes::require_cap(
                    crate::auth::cap::MANAGE_SETTINGS,
                ))),
        )
        .route(
            "/admin/settings/unifi_ips",
            put(routes::put_unifi_ips)
                .delete(routes::delete_unifi_ips)
                .route_layer(middleware::from_fn(routes::require_cap(
                    crate::auth::cap::MANAGE_SETTINGS,
                ))),
        )
        .route(
            "/admin/settings/webhook",
            put(routes::put_webhook)
                .delete(routes::delete_webhook)
                .route_layer(middleware::from_fn(routes::require_cap(
                    crate::auth::cap::MANAGE_SETTINGS,
                ))),
        )
        .layer(middleware::from_fn_with_state(
            state.clone(),
            routes::auth_extension,
        ))
        .with_state(state);

    if let Some(dir) = &config.ui_assets_dir {
        router = router.fallback_service(ServeDir::new(dir));
    }

    router.layer(TraceLayer::new_for_http())
}

fn bootstrap_admin(auth: &AuthStore, _config: &config::Config) -> anyhow::Result<()> {
    let email = std::env::var("LUMEN_INITIAL_ADMIN_EMAIL").ok();
    let password = std::env::var("LUMEN_INITIAL_ADMIN_PASSWORD").ok();
    let (Some(email), Some(password)) = (email, password) else {
        if auth.user_count()? == 0 {
            tracing::warn!(
                "no users exist and LUMEN_INITIAL_ADMIN_EMAIL / LUMEN_INITIAL_ADMIN_PASSWORD \
                 are unset — set both to bootstrap an admin user"
            );
        }
        return Ok(());
    };
    if auth.find_by_email(&email)?.is_some() {
        tracing::debug!(email = %email, "bootstrap admin already exists");
        return Ok(());
    }
    auth.create_user(&email, &password, Role::Admin)
        .with_context(|| format!("bootstrapping admin {email}"))?;
    tracing::info!(email = %email, "bootstrapped admin user");
    Ok(())
}

fn open_topology_store(config: &config::Config) -> anyhow::Result<TopologyStore> {
    std::fs::create_dir_all(&config.data_dir)
        .with_context(|| format!("creating data dir {}", config.data_dir.display()))?;
    let path = config.data_dir.join("topology.redb");
    let store = TopologyStore::open(&path)
        .with_context(|| format!("opening topology store at {}", path.display()))?;
    info!(path = %path.display(), "topology store ready");
    Ok(store)
}

fn spawn_engine_ingest(bus: FlowBus, engine: LiveStateEngine) {
    let mut rx = bus.subscribe();
    tokio::spawn(async move {
        loop {
            match rx.recv().await {
                Ok(flow) => engine.ingest(&flow),
                Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                    tracing::warn!(missed = n, "engine lagged on flow stream");
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            }
        }
    });
}

fn spawn_eviction(
    engine: LiveStateEngine,
    max_age: std::time::Duration,
    interval: std::time::Duration,
) {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(interval);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            ticker.tick().await;
            let dropped = engine.evict_stale(max_age);
            if dropped > 0 {
                tracing::info!(
                    dropped,
                    max_age_secs = max_age.as_secs(),
                    "evicted stale entities"
                );
            }
        }
    });
}

async fn shutdown_signal() {
    let ctrl_c = async {
        signal::ctrl_c()
            .await
            .expect("failed to install ctrl-c handler");
    };

    #[cfg(unix)]
    let terminate = async {
        signal::unix::signal(signal::unix::SignalKind::terminate())
            .expect("failed to install SIGTERM handler")
            .recv()
            .await;
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => info!("received SIGINT, shutting down"),
        _ = terminate => info!("received SIGTERM, shutting down"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt;

    fn test_config() -> config::Config {
        config::Config {
            http_listen: "127.0.0.1:0".parse().unwrap(),
            ui_assets_dir: None,
            netflow_v5_listen: None,
            edge_max_age: std::time::Duration::from_secs(300),
            eviction_interval: std::time::Duration::from_secs(30),
            data_dir: std::path::PathBuf::from("./data"),
            ingest_api_key: None,
            syslog_listen: None,
        }
    }

    fn test_state() -> AppState {
        let tmp = tempfile::tempdir().unwrap();
        let db = std::sync::Arc::new(redb::Database::create(tmp.path().join("t.redb")).unwrap());
        let detections = DetectionBus::new();
        let engine = LiveStateEngine::new();
        let secrets = SecretStore::with_key(db.clone(), &[0u8; 32]).unwrap();
        let settings = SettingsStore::new(db.clone(), secrets).unwrap();
        let supervisor =
            IntegrationSupervisor::new(engine.clone(), detections.clone(), settings.clone());
        AppState {
            bus: FlowBus::new(),
            detections,
            engine,
            auth: AuthStore::new(db).unwrap(),
            ingest_api_key: None,
            settings,
            supervisor,
        }
    }

    #[tokio::test]
    async fn healthz_returns_200() {
        let app = build_router(&test_config(), test_state());
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/healthz")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn version_returns_200_with_json() {
        let app = build_router(&test_config(), test_state());
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/version")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let content_type = response
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default();
        assert!(content_type.starts_with("application/json"));
    }

    #[tokio::test]
    async fn unknown_route_returns_404_when_no_ui_assets() {
        let app = build_router(&test_config(), test_state());
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/does-not-exist")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    // ── Admin settings end-to-end ───────────────────────────────────────

    /// Establish an Admin user, log in, return (router, session cookie).
    /// We can't reuse the app between requests because oneshot consumes
    /// it, so we rebuild it for each call against the same state.
    async fn admin_session() -> (AppState, String) {
        // Clear env vars that the settings store would otherwise pick
        // up — these tests assert "no DB config => returns None", so
        // an ambient `.env` on the dev box would otherwise corrupt
        // results.
        for k in [
            "UDM_URL",
            "UDM_API_KEY",
            "UDM_CONTROLLER_URL",
            "UDM_USERNAME",
            "UDM_PASSWORD",
            "UDM_SITE",
            "LUMEN_DETECTION_WEBHOOK_URL",
        ] {
            unsafe { std::env::remove_var(k) };
        }
        let state = test_state();
        state
            .auth
            .create_user("admin@test", "hunter2!", Role::Admin)
            .unwrap();
        let app = build_router(&test_config(), state.clone());
        let body = serde_json::json!({"email": "admin@test", "password": "hunter2!"}).to_string();
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/auth/login")
                    .header("content-type", "application/json")
                    .body(Body::from(body))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK, "login should succeed");
        let cookie = response
            .headers()
            .get("set-cookie")
            .expect("login should set a cookie")
            .to_str()
            .unwrap()
            .split(';')
            .next()
            .unwrap()
            .to_string();
        (state, cookie)
    }

    async fn read_body_json(response: axum::response::Response) -> serde_json::Value {
        let body = axum::body::to_bytes(response.into_body(), 1 << 20)
            .await
            .unwrap();
        serde_json::from_slice(&body).unwrap()
    }

    #[tokio::test]
    async fn admin_settings_requires_capability() {
        let app = build_router(&test_config(), test_state());
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/admin/settings")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn admin_settings_lists_three_unconfigured_integrations() {
        let (state, cookie) = admin_session().await;
        let app = build_router(&test_config(), state);
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/admin/settings")
                    .header("cookie", &cookie)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = read_body_json(response).await;
        let arr = body.as_array().expect("array");
        let ids: Vec<&str> = arr.iter().map(|s| s["id"].as_str().unwrap()).collect();
        assert_eq!(ids, vec!["unifi_labels", "unifi_ips", "webhook"]);
        for s in arr {
            assert_eq!(s["secret_configured"], false);
            assert_eq!(s["running"], false);
            assert!(s["plain_source"].is_null());
        }
    }

    #[tokio::test]
    async fn put_unifi_ips_persists_and_starts_integration() {
        let (state, cookie) = admin_session().await;
        let payload = serde_json::json!({
            "controller_url": "https://udm.invalid",
            "username": "lumen-reader",
            "password": "p@ssw0rd!",
            "site": "default"
        })
        .to_string();
        let app = build_router(&test_config(), state.clone());
        let response = app
            .oneshot(
                Request::builder()
                    .method("PUT")
                    .uri("/admin/settings/unifi_ips")
                    .header("cookie", &cookie)
                    .header("content-type", "application/json")
                    .body(Body::from(payload))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NO_CONTENT);

        // GET shows the integration as configured + running, without
        // ever leaking the password.
        let app = build_router(&test_config(), state.clone());
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/admin/settings")
                    .header("cookie", &cookie)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let body = read_body_json(response).await;
        let serialized = body.to_string();
        assert!(
            !serialized.contains("p@ssw0rd!"),
            "secret value should never appear in GET response: {serialized}"
        );
        let ips = body
            .as_array()
            .unwrap()
            .iter()
            .find(|s| s["id"] == "unifi_ips")
            .unwrap();
        assert_eq!(ips["plain_source"], "db");
        assert_eq!(ips["secret_source"], "db");
        assert_eq!(ips["secret_configured"], true);
        assert_eq!(ips["running"], true);
        assert_eq!(ips["plain"]["controller_url"], "https://udm.invalid");
        assert_eq!(ips["plain"]["site"], "default");
        assert_eq!(ips["plain"]["username"], "lumen-reader");
        // Stop the running task so the test runtime can shut down
        // cleanly without waiting for the poll loop.
        state.supervisor.stop("unifi_ips");
    }

    #[tokio::test]
    async fn delete_unifi_ips_clears_and_stops() {
        let (state, cookie) = admin_session().await;
        // Configure first.
        let payload = serde_json::json!({
            "controller_url": "https://udm.invalid",
            "username": "lumen-reader",
            "password": "p",
            "site": "default"
        })
        .to_string();
        build_router(&test_config(), state.clone())
            .oneshot(
                Request::builder()
                    .method("PUT")
                    .uri("/admin/settings/unifi_ips")
                    .header("cookie", &cookie)
                    .header("content-type", "application/json")
                    .body(Body::from(payload))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert!(state.supervisor.is_running("unifi_ips"));
        // Then clear.
        let response = build_router(&test_config(), state.clone())
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri("/admin/settings/unifi_ips")
                    .header("cookie", &cookie)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NO_CONTENT);
        assert!(!state.supervisor.is_running("unifi_ips"));
    }
}
