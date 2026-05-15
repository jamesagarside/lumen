use std::net::SocketAddr;
use std::path::PathBuf;

use anyhow::{Context, Result};

#[derive(Debug, Clone)]
pub struct Config {
    pub http_listen: SocketAddr,
    pub ui_assets_dir: Option<PathBuf>,
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

        Ok(Self {
            http_listen,
            ui_assets_dir,
        })
    }
}
