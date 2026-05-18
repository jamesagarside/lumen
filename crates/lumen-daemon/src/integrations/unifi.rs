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

use crate::integrations::diagnostics::Recorder;
use crate::state::LiveStateEngine;

const POLL_INTERVAL: Duration = Duration::from_secs(60);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
/// UniFi's documented max page size for the integration API.
const PAGE_LIMIT: u32 = 200;

/// Shape of UniFi's paginated responses: the integration API wraps
/// list results in `{ data, offset, limit, count, totalCount }`.
/// We follow pages until we've seen `totalCount` items.
#[derive(Debug, Deserialize)]
struct PaginatedEnvelope<T> {
    data: Vec<T>,
    #[serde(default, rename = "totalCount", alias = "total")]
    total_count: Option<u32>,
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

    /// Fetch every page of `base_url`. UniFi's integration API uses
    /// `offset` + `limit` query params and reports `totalCount` in
    /// the envelope. Defends against bad servers with a safety cap
    /// (50 pages * 200 = 10k items) so a misbehaving controller
    /// can't trap us forever.
    async fn list_paginated<T: for<'de> Deserialize<'de>>(&self, base_url: &str) -> Result<Vec<T>> {
        let mut out: Vec<T> = Vec::new();
        let mut offset: u32 = 0;
        for _ in 0..50 {
            let sep = if base_url.contains('?') { '&' } else { '?' };
            let url = format!("{base_url}{sep}offset={offset}&limit={PAGE_LIMIT}");
            let env: PaginatedEnvelope<T> = self.get_json(&url).await?;
            let got = env.data.len();
            out.extend(env.data);
            offset += got as u32;
            let done = env.total_count.is_none_or(|t| out.len() as u32 >= t) || got == 0;
            if done {
                break;
            }
        }
        Ok(out)
    }

    async fn list_sites(&self) -> Result<Vec<Site>> {
        self.list_paginated(&self.sites_url).await
    }

    async fn list_clients(&self, site_id: &str) -> Result<Vec<Client>> {
        // The sites_url already ends with /sites. Build the per-site
        // clients URL by appending the site id and /clients.
        let url = format!(
            "{}/{}/clients",
            self.sites_url.trim_end_matches('/'),
            site_id
        );
        self.list_paginated(&url).await
    }
}

/// Spawn a background task that polls UniFi and auto-labels nodes.
/// The task runs forever; if a poll fails we log and try again on
/// the next tick. Returns an `AbortHandle` so the supervisor can
/// tear the task down when settings change.
pub fn spawn(
    client: UnifiClient,
    engine: LiveStateEngine,
    diag: Recorder,
) -> tokio::task::AbortHandle {
    let handle = tokio::spawn(async move {
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
                Ok(stats) => {
                    if stats.labelled > 0 {
                        info!(
                            sites = stats.sites,
                            clients = stats.clients,
                            labelled = stats.labelled,
                            "unifi: labels refreshed"
                        );
                    } else {
                        debug!(
                            sites = stats.sites,
                            clients = stats.clients,
                            "unifi: nothing to label this tick"
                        );
                    }
                    diag.record_ok(stats.clients as u32, stats.labelled as u32, None);
                }
                Err(e) => {
                    warn!(error = %e, "unifi: poll failed");
                    diag.record_err(format!("{e:#}"));
                }
            }
        }
    });
    handle.abort_handle()
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
