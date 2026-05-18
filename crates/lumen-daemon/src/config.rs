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
    /// Optional syslog listener for iptables-LOG style messages
    /// (UniFi UDM-Pro, OPNsense, pfSense, vanilla netfilter). Off
    /// by default — opt in with LUMEN_SYSLOG_LISTEN=0.0.0.0:5514
    /// (or :514 if the daemon has CAP_NET_BIND_SERVICE / root).
    pub syslog_listen: Option<SocketAddr>,
    /// Time bound on the rolling raw-flow buffer (#20). Defaults to
    /// 15 minutes per CONTEXT.md §3.
    pub raw_window_duration: Duration,
    /// Memory bound on the rolling raw-flow buffer (#20). Defaults to
    /// 512 MB per CONTEXT.md §3.
    pub raw_window_bytes: usize,
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
        let netflow_v5_listen = parse_optional_listen("LUMEN_NETFLOW_V5_LISTEN")?
            .unwrap_or_else(|| Some("0.0.0.0:2055".parse().unwrap()));

        // Syslog is off by default (UniFi etc. need explicit opt-in
        // since :514 is privileged on Linux; use 5514 on macOS/dev).
        let syslog_listen = parse_optional_listen("LUMEN_SYSLOG_LISTEN")?.flatten();

        let edge_max_age = parse_duration_secs("LUMEN_EDGE_MAX_AGE_SECS", 300)?;
        let eviction_interval = parse_duration_secs("LUMEN_EVICTION_INTERVAL_SECS", 30)?;

        let data_dir = std::env::var("LUMEN_DATA_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from("./data"));

        let ingest_api_key = std::env::var("LUMEN_INGEST_API_KEY")
            .ok()
            .filter(|s| !s.is_empty());

        let raw_window_duration = parse_duration_secs("LUMEN_RAW_WINDOW_DURATION_SECS", 15 * 60)?;
        let raw_window_bytes = parse_bytes("LUMEN_RAW_WINDOW_BYTES", 512 * 1024 * 1024)?;

        Ok(Self {
            http_listen,
            ui_assets_dir,
            netflow_v5_listen,
            edge_max_age,
            eviction_interval,
            data_dir,
            ingest_api_key,
            syslog_listen,
            raw_window_duration,
            raw_window_bytes,
        })
    }
}

/// Parse an "optional socket address" env var.
///
/// Returns:
/// - `Ok(None)` if the env var is unset (caller decides default)
/// - `Ok(Some(None))` if explicitly set to "off" or "" (disable)
/// - `Ok(Some(Some(addr)))` if set to a valid socket address
/// - `Err(_)` if set to an invalid value
fn parse_optional_listen(env_var: &str) -> Result<Option<Option<SocketAddr>>> {
    match std::env::var(env_var) {
        Err(_) => Ok(None),
        Ok(v) if v.is_empty() || v == "off" => Ok(Some(None)),
        Ok(v) => Ok(Some(Some(v.parse().with_context(|| {
            format!("{env_var}={v} is not a valid socket address")
        })?))),
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

/// Parse a byte-count env var. Accepts a plain integer or one of the
/// suffixed forms: `512MB`, `1GB`, `64KB` (powers of 1024). Case-insensitive,
/// trailing `B` optional.
fn parse_bytes(env_var: &str, default_bytes: usize) -> Result<usize> {
    let raw = match std::env::var(env_var) {
        Ok(v) => v,
        Err(_) => return Ok(default_bytes),
    };
    let trimmed = raw.trim();
    let (num, mult): (&str, usize) = if let Some(rest) = strip_suffix_ci(trimmed, "gb") {
        (rest, 1024 * 1024 * 1024)
    } else if let Some(rest) = strip_suffix_ci(trimmed, "mb") {
        (rest, 1024 * 1024)
    } else if let Some(rest) = strip_suffix_ci(trimmed, "kb") {
        (rest, 1024)
    } else if let Some(rest) = strip_suffix_ci(trimmed, "g") {
        (rest, 1024 * 1024 * 1024)
    } else if let Some(rest) = strip_suffix_ci(trimmed, "m") {
        (rest, 1024 * 1024)
    } else if let Some(rest) = strip_suffix_ci(trimmed, "k") {
        (rest, 1024)
    } else if let Some(rest) = strip_suffix_ci(trimmed, "b") {
        (rest, 1)
    } else {
        (trimmed, 1)
    };
    let n: usize = num.trim().parse().with_context(|| {
        format!("{env_var}={raw} is not a byte count (e.g. 512MB, 1GB, 1048576)")
    })?;
    Ok(n.saturating_mul(mult))
}

fn strip_suffix_ci<'a>(s: &'a str, suf: &str) -> Option<&'a str> {
    if s.len() < suf.len() {
        return None;
    }
    let (head, tail) = s.split_at(s.len() - suf.len());
    if tail.eq_ignore_ascii_case(suf) {
        Some(head)
    } else {
        None
    }
}
