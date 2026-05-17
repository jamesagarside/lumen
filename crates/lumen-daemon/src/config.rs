use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result};

#[derive(Debug, Clone)]
pub struct Config {
    pub http_listen: SocketAddr,
    pub ui_assets_dir: Option<PathBuf>,
    pub netflow_v5_listen: Option<SocketAddr>,
    /// How long an edge can go without traffic before being evicted.
    pub edge_max_age: Duration,
    /// How often the eviction sweep runs.
    pub eviction_interval: Duration,
    /// Persistent state (topology DB, future plugin permissions, etc.)
    pub data_dir: PathBuf,
    /// Optional shared secret required to POST to /ingest/flows.
    /// Unset = open ingestion (fine on localhost or trusted networks;
    /// not recommended on the open internet).
    pub ingest_api_key: Option<String>,
}

impl Config {
    pub fn from_env() -> Result<Self> {
        let http_listen = std::env::var("LUMEN_HTTP_LISTEN")
            .unwrap_or_else(|_| "0.0.0.0:3000".to_string())
            .parse()
            .context("LUMEN_HTTP_LISTEN must be a valid socket address (e.g. 0.0.0.0:3000)")?;

        let ui_assets_dir = std::env::var("LUMEN_UI_ASSETS_DIR")
            .ok()
            .map(PathBuf::from)
            .filter(|p| p.exists());

        // Three cases: unset → default; "off" → None (disabled);
        // anything else → must parse as a socket address.
        let netflow_v5_listen = match std::env::var("LUMEN_NETFLOW_V5_LISTEN") {
            Err(_) => Some("0.0.0.0:2055".parse().unwrap()),
            Ok(v) if v.is_empty() || v == "off" => None,
            Ok(v) => Some(v.parse().with_context(|| {
                format!("LUMEN_NETFLOW_V5_LISTEN={v} is not a valid socket address")
            })?),
        };

        let edge_max_age = parse_duration_secs("LUMEN_EDGE_MAX_AGE_SECS", 300)?;
        let eviction_interval = parse_duration_secs("LUMEN_EVICTION_INTERVAL_SECS", 30)?;

        let data_dir = std::env::var("LUMEN_DATA_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from("./data"));

        let ingest_api_key = std::env::var("LUMEN_INGEST_API_KEY")
            .ok()
            .filter(|s| !s.is_empty());

        Ok(Self {
            http_listen,
            ui_assets_dir,
            netflow_v5_listen,
            edge_max_age,
            eviction_interval,
            data_dir,
            ingest_api_key,
        })
    }
}

fn parse_duration_secs(env_var: &str, default_secs: u64) -> Result<Duration> {
    match std::env::var(env_var) {
        Ok(v) => {
            let secs: u64 = v.parse().with_context(|| {
                format!("{env_var}={v} is not a non-negative integer (seconds)")
            })?;
            Ok(Duration::from_secs(secs))
        }
        Err(_) => Ok(Duration::from_secs(default_secs)),
    }
}
