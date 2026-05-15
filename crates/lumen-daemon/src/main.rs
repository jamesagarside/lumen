use anyhow::Context;
use axum::{routing::get, Router};
use tokio::net::TcpListener;
use tokio::signal;
use tower_http::services::ServeDir;
use tower_http::trace::TraceLayer;
use tracing::info;

mod config;
mod observability;
mod routes;

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

    let app = build_router(&config);
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

fn build_router(config: &config::Config) -> Router {
    let mut router = Router::new()
        .route("/healthz", get(routes::healthz))
        .route("/version", get(routes::version));

    if let Some(dir) = &config.ui_assets_dir {
        router = router.fallback_service(ServeDir::new(dir));
    }

    router.layer(TraceLayer::new_for_http())
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
        }
    }

    #[tokio::test]
    async fn healthz_returns_200() {
        let app = build_router(&test_config());
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
        let app = build_router(&test_config());
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
        let app = build_router(&test_config());
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
