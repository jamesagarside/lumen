//! UniFi Network Integration API client.
//!
//! Reads device + client inventory from a UniFi controller (UDM, UDM
//! Pro, UDM SE, UCG, Cloud Key, self-hosted Network Application)
//! via the *Network Integration API* added in UniFi Network 9.0.
//! Auth is a single header `X-API-KEY` (generated under
//! Settings → System → API → Create API Key).
//!
//! For each known client we auto-label the matching node in the
//! engine — but only if the user hasn't already labelled it (we
//! never clobber a user edit).
//!
//! Built-in for now; eventually migrates to a WASM plugin once the
//! plugin runtime lands (CONTEXT.md §5). The contract stays the
//! same — only the runtime changes.

use std::time::Duration;

use anyhow::{Context, Result};
use lumen_core::NodeId;
use serde::Deserialize;
use tracing::{debug, info, instrument, warn};

use crate::state::LiveStateEngine;

const POLL_INTERVAL: Duration = Duration::from_secs(60);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

/// The shape of a UniFi site response. The Network Integration API
/// envelopes results in `{ data: [...], offset, limit, count, total }`.
#[derive(Debug, Deserialize)]
struct PaginatedEnvelope<T> {
    data: Vec<T>,
    #[serde(default)]
    #[allow(dead_code)]
    total: Option<u32>,
}

/// A site within a UniFi controller. The integration API uses these
/// to scope every other query.
#[derive(Debug, Deserialize)]
struct Site {
    id: String,
    #[serde(default)]
    name: Option<String>,
}

/// A connected client (wired or wireless device). UniFi names this
/// schema loosely — `name` may be missing for unknown devices,
/// `display_name` may be present in firmware ≥ 9.0, `ip_address`
/// is the v4 address as a string.
#[derive(Debug, Deserialize)]
struct Client {
    #[serde(default)]
    name: Option<String>,
    #[serde(default, rename = "displayName", alias = "display_name")]
    display_name: Option<String>,
    #[serde(default, rename = "ipAddress", alias = "ip_address", alias = "ip")]
    ip_address: Option<String>,
    #[serde(default, rename = "macAddress", alias = "mac_address", alias = "mac")]
    #[allow(dead_code)] // Used when MAC-aware ingestion lands
    mac_address: Option<String>,
}

impl Client {
    /// Pick the best display name. Priority: explicit `name` (user
    /// has typed it in the UniFi UI) > `display_name` (UniFi-guessed
    /// from hostname) > nothing.
    fn best_name(&self) -> Option<&str> {
        self.name
            .as_deref()
            .or(self.display_name.as_deref())
            .map(str::trim)
            .filter(|s| !s.is_empty())
    }
}

#[derive(Clone)]
pub struct UnifiClient {
    http: reqwest::Client,
    sites_url: String,
    api_key: String,
}

impl UnifiClient {
    pub fn new(sites_url: String, api_key: String) -> Result<Self> {
        let http = reqwest::Client::builder()
            // UniFi controllers ship a self-signed TLS cert by
            // default. Accept it — homelab users shouldn't need to
            // install their cert chain just to use this integration.
            .danger_accept_invalid_certs(true)
            .timeout(REQUEST_TIMEOUT)
            .build()
            .context("building UniFi HTTP client")?;
        Ok(Self {
            http,
            sites_url,
            api_key,
        })
    }

    async fn get_json<T: for<'de> Deserialize<'de>>(&self, url: &str) -> Result<T> {
        let response = self
            .http
            .get(url)
            .header("X-API-KEY", &self.api_key)
            .header("Accept", "application/json")
            .send()
            .await
            .with_context(|| format!("GET {url}"))?
            .error_for_status()
            .with_context(|| format!("GET {url} (non-2xx)"))?;
        let body = response
            .text()
            .await
            .with_context(|| format!("GET {url}: read body"))?;
        serde_json::from_str(&body).with_context(|| {
            format!(
                "GET {url}: parse JSON (body starts with {:?})",
                body.chars().take(80).collect::<String>()
            )
        })
    }

    async fn list_sites(&self) -> Result<Vec<Site>> {
        let envelope: PaginatedEnvelope<Site> = self.get_json(&self.sites_url).await?;
        Ok(envelope.data)
    }

    async fn list_clients(&self, site_id: &str) -> Result<Vec<Client>> {
        // The sites_url already ends with /sites. Build the per-site
        // clients URL by appending the site id and /clients.
        let url = format!(
            "{}/{}/clients",
            self.sites_url.trim_end_matches('/'),
            site_id
        );
        let envelope: PaginatedEnvelope<Client> = self.get_json(&url).await?;
        Ok(envelope.data)
    }
}

/// Spawn a background task that polls UniFi and auto-labels nodes.
/// The task runs forever; if a poll fails we log and try again on
/// the next tick.
pub fn spawn(client: UnifiClient, engine: LiveStateEngine) {
    tokio::spawn(async move {
        info!(
            interval_secs = POLL_INTERVAL.as_secs(),
            "unifi integration: poller started"
        );
        let mut ticker = tokio::time::interval(POLL_INTERVAL);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        // Tick once immediately so labels appear on startup without
        // waiting a full interval.
        loop {
            ticker.tick().await;
            match poll_once(&client, &engine).await {
                Ok(stats) if stats.labelled > 0 => info!(
                    sites = stats.sites,
                    clients = stats.clients,
                    labelled = stats.labelled,
                    "unifi: labels refreshed"
                ),
                Ok(stats) => debug!(
                    sites = stats.sites,
                    clients = stats.clients,
                    "unifi: nothing to label this tick"
                ),
                Err(e) => warn!(error = %e, "unifi: poll failed"),
            }
        }
    });
}

#[derive(Debug, Default)]
struct PollStats {
    sites: usize,
    clients: usize,
    labelled: usize,
}

#[instrument(skip_all)]
async fn poll_once(client: &UnifiClient, engine: &LiveStateEngine) -> Result<PollStats> {
    let mut stats = PollStats::default();
    let sites = client.list_sites().await?;
    stats.sites = sites.len();
    for site in sites {
        let label_for_site = site.name.as_deref().unwrap_or(&site.id);
        let clients = match client.list_clients(&site.id).await {
            Ok(c) => c,
            Err(e) => {
                warn!(site = label_for_site, error = %e, "unifi: per-site fetch failed");
                continue;
            }
        };
        stats.clients += clients.len();
        for c in clients {
            let Some(ip_str) = c.ip_address.as_deref() else {
                continue;
            };
            let Ok(ip) = ip_str.parse() else {
                continue;
            };
            let Some(name) = c.best_name() else {
                continue;
            };
            match engine.set_node_label_if_unset(&NodeId(ip), name) {
                Ok(true) => stats.labelled += 1,
                Ok(false) => {} // already labelled or not in topology
                Err(e) => warn!(error = %e, ip = %ip, "unifi: label write failed"),
            }
        }
    }
    Ok(stats)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn client_best_name_prefers_name_over_display() {
        let c = Client {
            name: Some("James MacBook".to_string()),
            display_name: Some("MacBook-Pro".to_string()),
            ip_address: None,
            mac_address: None,
        };
        assert_eq!(c.best_name(), Some("James MacBook"));
    }

    #[test]
    fn client_best_name_falls_back_to_display() {
        let c = Client {
            name: None,
            display_name: Some("MacBook-Pro".to_string()),
            ip_address: None,
            mac_address: None,
        };
        assert_eq!(c.best_name(), Some("MacBook-Pro"));
    }

    #[test]
    fn client_best_name_ignores_blank() {
        let c = Client {
            name: Some("   ".to_string()),
            display_name: Some("".to_string()),
            ip_address: None,
            mac_address: None,
        };
        assert_eq!(c.best_name(), None);
    }
}
