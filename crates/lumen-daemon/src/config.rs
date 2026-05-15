use std::net::SocketAddr;
use std::path::PathBuf;

use anyhow::{Context, Result};

#[derive(Debug, Clone)]
pub struct Config {
    pub http_listen: SocketAddr,
    pub ui_assets_dir: Option<PathBuf>,
    pub netflow_v5_listen: Option<SocketAddr>,
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

        let netflow_v5_listen = parse_optional_socket_addr("LUMEN_NETFLOW_V5_LISTEN")?
            .or_else(|| Some("0.0.0.0:2055".parse().unwrap()));

        Ok(Self {
            http_listen,
            ui_assets_dir,
            netflow_v5_listen,
        })
    }
}

fn parse_optional_socket_addr(env_var: &str) -> Result<Option<SocketAddr>> {
    match std::env::var(env_var) {
        Ok(v) if v.is_empty() || v == "off" => Ok(None),
        Ok(v) => v
            .parse()
            .map(Some)
            .with_context(|| format!("{env_var}={v} is not a valid socket address")),
        Err(_) => Ok(None),
    }
}
