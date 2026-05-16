use anyhow::Context;
use axum::{routing::get, Router};
use tokio::net::TcpListener;
use tokio::signal;
use tower_http::services::ServeDir;
use tower_http::trace::TraceLayer;
use tracing::{error, info};

use crate::ingest::{udp_listener, FlowBus};
use crate::routes::AppState;
use crate::state::LiveStateEngine;

mod config;
mod ingest;
mod observability;
mod routes;
mod state;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    observability::init();

    let config = config::Config::from_env()?;

    info!(
        version = env!("CARGO_PKG_VERSION"),
        listen = %config.http_listen,
        ui_assets = ?config.ui_assets_dir,
        "lumen starting"
    );

    let bus = FlowBus::new();
    let engine = LiveStateEngine::new();
    let state = AppState {
        bus: bus.clone(),
        engine: engine.clone(),
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
            if let Err(e) = udp_listener::run(addr, udp_listener::NetflowV5Parser, bus).await {
                error!(listener = "netflow_v5", error = %e, "ingest listener exited");
            }
        });
    }

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
        .route("/healthz", get(routes::healthz))
        .route("/version", get(routes::version))
        .route("/snapshot", get(routes::snapshot))
        .route("/ws/flows", get(routes::ws_flows))
        .with_state(state);

    if let Some(dir) = &config.ui_assets_dir {
        router = router.fallback_service(ServeDir::new(dir));
    }

    router.layer(TraceLayer::new_for_http())
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
        }
    }

    fn test_state() -> AppState {
        AppState {
            bus: FlowBus::new(),
            engine: LiveStateEngine::new(),
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
}
