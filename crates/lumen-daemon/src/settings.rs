//! Admin-managed integration settings.
//!
//! Backs the admin UI's Settings page. Replaces what used to be a
//! pile of `UDM_URL` / `UDM_API_KEY` / `LUMEN_DETECTION_WEBHOOK_URL`
//! env vars at startup with a typed, hot-editable store:
//!
//! - **Plain fields** (URLs, site names, intervals) live in a redb
//!   `settings` table as JSON.
//! - **Secret fields** (passwords, API keys) live in the encrypted
//!   `secrets` table via `SecretStore`.
//! - **Env vars** still work as a *fallback*: if the DB has nothing
//!   for an integration, env vars are tried. This keeps existing
//!   `.env` deployments running unchanged after the upgrade, and
//!   gives operators a clean migration path (save once via the UI →
//!   delete from `.env`).
//!
//! When DB *and* env are both set, the DB wins and we emit a one-shot
//! warning so the operator knows their env value is dead weight.

use std::sync::Arc;

use anyhow::{Context, Result};
use redb::{Database, TableDefinition};
use serde::{Deserialize, Serialize};

use crate::auth::Role;
use crate::secret_store::SecretStore;

/// `settings`: integration-id → JSON-serialised plain-field struct.
/// (e.g. key `"unifi_ips"` → `{"controller_url": "...", "site": "..."}`)
const SETTINGS: TableDefinition<&str, &str> = TableDefinition::new("settings");

/// Stable identifiers for the integrations the admin UI can manage.
/// String constants rather than an enum so a new integration can land
/// in a single PR without rippling through pattern matches.
pub mod id {
    pub const UNIFI_LABELS: &str = "unifi_labels";
    pub const UNIFI_IPS: &str = "unifi_ips";
    pub const WEBHOOK: &str = "webhook";
    pub const OIDC: &str = "oidc";
}

/// Names of the secret-store entries. Stable across versions — these
/// are persisted into the encrypted DB.
mod secret_name {
    pub const UDM_API_KEY: &str = "unifi_labels.api_key";
    pub const UDM_PASSWORD: &str = "unifi_ips.password";
    pub const OIDC_CLIENT_SECRET: &str = "oidc.client_secret";
}

/// UniFi *labels* integration (Network Integration API). Plain
/// fields only — the API key is the secret half, stored separately.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct UnifiLabelsPlain {
    /// e.g. `https://192.168.0.1/proxy/network/integration/v1/sites`
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
}

/// Merged view: plain + whether the secret is set. The actual API
/// key value never leaves this module — callers fetch it via
/// `Settings::get_unifi_labels` which returns the full resolved
/// config including secrets, suitable for handing to the integration
/// at start-up.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnifiLabelsConfig {
    pub url: String,
    pub api_key: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct UnifiIpsPlain {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub controller_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub site: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub username: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnifiIpsConfig {
    pub controller_url: String,
    pub site: String,
    pub username: String,
    pub password: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WebhookPlain {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WebhookConfig {
    pub url: String,
}

// ── OIDC ────────────────────────────────────────────────────────────────────

/// IdP group claim → Lumen role mapping. When a user signs in via OIDC
/// (#14), the daemon looks up their group claim values and picks the
/// first matching mapping. The order matters — first match wins.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OidcGroupMapping {
    pub group: String,
    pub role: Role,
}

/// Plain (non-secret) OIDC config. Mirrors the shape stored under the
/// `oidc` settings key. The client_secret lives in the encrypted store
/// and is never serialised alongside this.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct OidcPlain {
    /// Issuer URL (no trailing slash). The daemon discovers endpoints
    /// via `${issuer}/.well-known/openid-configuration` when the login
    /// flow lands in #14.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub issuer_url: Option<String>,
    /// Client ID registered with the IdP.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_id: Option<String>,
    /// Ordered list of group → role mappings. First match wins; users
    /// whose groups don't match anything get rejected at login time
    /// rather than being silently created with no role.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub group_mappings: Vec<OidcGroupMapping>,
}

/// Fully-resolved OIDC config — issuer + client + secret + mappings.
/// Currently unused at runtime call sites; the discovery / login flow
/// that consumes this lands in #14. The admin UI configures it now so
/// admins can stage their IdP integration ahead of the flow shipping.
#[allow(dead_code)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OidcConfig {
    pub issuer_url: String,
    pub client_id: String,
    pub client_secret: String,
    pub group_mappings: Vec<OidcGroupMapping>,
}

/// Whether a piece of admin-managed config was sourced from the DB
/// (live, editable) or from an env var (immutable until removed).
/// The admin UI uses this to badge env-sourced fields as read-only.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Source {
    Db,
    Env,
}

/// What the admin UI is allowed to see for a single integration —
/// never the secret value, only whether one is set and where it
/// came from.
#[derive(Debug, Clone, Serialize)]
pub struct IntegrationStatus {
    pub id: &'static str,
    pub plain: serde_json::Value,
    pub plain_source: Option<Source>,
    pub secret_configured: bool,
    pub secret_source: Option<Source>,
}

#[derive(Clone)]
pub struct SettingsStore {
    db: Arc<Database>,
    secrets: SecretStore,
}

impl SettingsStore {
    pub fn new(db: Arc<Database>, secrets: SecretStore) -> Result<Self> {
        let txn = db.begin_write().context("settings: open write txn")?;
        {
            txn.open_table(SETTINGS)
                .context("settings: open settings table")?;
        }
        txn.commit().context("settings: commit init")?;
        Ok(Self { db, secrets })
    }

    fn read_plain<T: for<'de> Deserialize<'de>>(&self, key: &str) -> Result<Option<T>> {
        let txn = self.db.begin_read()?;
        let table = txn.open_table(SETTINGS)?;
        let Some(raw) = table.get(key)? else {
            return Ok(None);
        };
        let value: T = serde_json::from_str(raw.value())
            .with_context(|| format!("settings: parse stored {key}"))?;
        Ok(Some(value))
    }

    fn write_plain<T: Serialize>(&self, key: &str, value: &T) -> Result<()> {
        let json =
            serde_json::to_string(value).with_context(|| format!("settings: serialise {key}"))?;
        let txn = self.db.begin_write()?;
        {
            let mut table = txn.open_table(SETTINGS)?;
            table.insert(key, json.as_str())?;
        }
        txn.commit()?;
        Ok(())
    }

    fn delete_plain(&self, key: &str) -> Result<()> {
        let txn = self.db.begin_write()?;
        {
            let mut table = txn.open_table(SETTINGS)?;
            table.remove(key)?;
        }
        txn.commit()?;
        Ok(())
    }

    // ── UniFi labels ──────────────────────────────────────────────────────

    /// Resolve the full config (URL + API key). Returns `None` when
    /// either half is missing — the integration only starts when
    /// both are present.
    pub fn get_unifi_labels(&self) -> Result<Option<UnifiLabelsConfig>> {
        let plain = self.read_plain::<UnifiLabelsPlain>(id::UNIFI_LABELS)?;
        let url = plain
            .as_ref()
            .and_then(|p| p.url.clone())
            .or_else(|| env_nonempty("UDM_URL"));
        let api_key = self
            .secrets
            .get(secret_name::UDM_API_KEY)?
            .or_else(|| env_nonempty("UDM_API_KEY"));
        match (url, api_key) {
            (Some(url), Some(api_key)) => Ok(Some(UnifiLabelsConfig { url, api_key })),
            _ => Ok(None),
        }
    }

    pub fn set_unifi_labels(&self, url: Option<String>, api_key: Option<String>) -> Result<()> {
        let url = url.map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
        self.write_plain(id::UNIFI_LABELS, &UnifiLabelsPlain { url })?;
        if let Some(key) = api_key {
            self.secrets.put(secret_name::UDM_API_KEY, key.trim())?;
        }
        Ok(())
    }

    pub fn clear_unifi_labels(&self) -> Result<()> {
        self.delete_plain(id::UNIFI_LABELS)?;
        self.secrets.delete(secret_name::UDM_API_KEY)?;
        Ok(())
    }

    pub fn unifi_labels_status(&self) -> Result<IntegrationStatus> {
        let plain = self.read_plain::<UnifiLabelsPlain>(id::UNIFI_LABELS)?;
        let plain_source = plain_source(plain.is_some(), env_nonempty("UDM_URL").is_some());
        let resolved = match (&plain, env_nonempty("UDM_URL")) {
            (Some(p), _) => p.clone(),
            (None, Some(url)) => UnifiLabelsPlain { url: Some(url) },
            (None, None) => UnifiLabelsPlain::default(),
        };
        let secret_in_db = self.secrets.has(secret_name::UDM_API_KEY)?;
        let secret_in_env = env_nonempty("UDM_API_KEY").is_some();
        Ok(IntegrationStatus {
            id: id::UNIFI_LABELS,
            plain: serde_json::to_value(resolved)?,
            plain_source,
            secret_configured: secret_in_db || secret_in_env,
            secret_source: plain_source_for(secret_in_db, secret_in_env),
        })
    }

    // ── UniFi IPS ─────────────────────────────────────────────────────────

    pub fn get_unifi_ips(&self) -> Result<Option<UnifiIpsConfig>> {
        let plain = self.read_plain::<UnifiIpsPlain>(id::UNIFI_IPS)?;
        let controller_url = plain
            .as_ref()
            .and_then(|p| p.controller_url.clone())
            .or_else(|| env_nonempty("UDM_CONTROLLER_URL"));
        let site = plain
            .as_ref()
            .and_then(|p| p.site.clone())
            .or_else(|| env_nonempty("UDM_SITE"))
            .unwrap_or_else(|| "default".to_string());
        let username = plain
            .as_ref()
            .and_then(|p| p.username.clone())
            .or_else(|| env_nonempty("UDM_USERNAME"));
        let password = self
            .secrets
            .get(secret_name::UDM_PASSWORD)?
            .or_else(|| env_nonempty("UDM_PASSWORD"));
        match (controller_url, username, password) {
            (Some(controller_url), Some(username), Some(password)) => Ok(Some(UnifiIpsConfig {
                controller_url,
                site,
                username,
                password,
            })),
            _ => Ok(None),
        }
    }

    pub fn set_unifi_ips(
        &self,
        controller_url: Option<String>,
        username: Option<String>,
        password: Option<String>,
        site: Option<String>,
    ) -> Result<()> {
        let plain = UnifiIpsPlain {
            controller_url: controller_url
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty()),
            username: username
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty()),
            site: site.map(|s| s.trim().to_string()).filter(|s| !s.is_empty()),
        };
        self.write_plain(id::UNIFI_IPS, &plain)?;
        if let Some(p) = password {
            self.secrets.put(secret_name::UDM_PASSWORD, p.trim())?;
        }
        Ok(())
    }

    pub fn clear_unifi_ips(&self) -> Result<()> {
        self.delete_plain(id::UNIFI_IPS)?;
        self.secrets.delete(secret_name::UDM_PASSWORD)?;
        Ok(())
    }

    pub fn unifi_ips_status(&self) -> Result<IntegrationStatus> {
        let plain = self.read_plain::<UnifiIpsPlain>(id::UNIFI_IPS)?;
        let plain_source = plain_source(plain.is_some(), any_unifi_ips_env_set());
        let resolved = UnifiIpsPlain {
            controller_url: plain
                .as_ref()
                .and_then(|p| p.controller_url.clone())
                .or_else(|| env_nonempty("UDM_CONTROLLER_URL")),
            username: plain
                .as_ref()
                .and_then(|p| p.username.clone())
                .or_else(|| env_nonempty("UDM_USERNAME")),
            site: plain
                .as_ref()
                .and_then(|p| p.site.clone())
                .or_else(|| env_nonempty("UDM_SITE")),
        };
        let secret_in_db = self.secrets.has(secret_name::UDM_PASSWORD)?;
        let secret_in_env = env_nonempty("UDM_PASSWORD").is_some();
        Ok(IntegrationStatus {
            id: id::UNIFI_IPS,
            plain: serde_json::to_value(resolved)?,
            plain_source,
            secret_configured: secret_in_db || secret_in_env,
            secret_source: plain_source_for(secret_in_db, secret_in_env),
        })
    }

    // ── Webhook ───────────────────────────────────────────────────────────

    pub fn get_webhook(&self) -> Result<Option<WebhookConfig>> {
        let plain = self.read_plain::<WebhookPlain>(id::WEBHOOK)?;
        let url = plain
            .as_ref()
            .and_then(|p| p.url.clone())
            .or_else(|| env_nonempty("LUMEN_DETECTION_WEBHOOK_URL"));
        Ok(url.map(|url| WebhookConfig { url }))
    }

    pub fn set_webhook(&self, url: Option<String>) -> Result<()> {
        let plain = WebhookPlain {
            url: url.map(|s| s.trim().to_string()).filter(|s| !s.is_empty()),
        };
        self.write_plain(id::WEBHOOK, &plain)
    }

    pub fn clear_webhook(&self) -> Result<()> {
        self.delete_plain(id::WEBHOOK)
    }

    pub fn webhook_status(&self) -> Result<IntegrationStatus> {
        let plain = self.read_plain::<WebhookPlain>(id::WEBHOOK)?;
        let env_present = env_nonempty("LUMEN_DETECTION_WEBHOOK_URL").is_some();
        let plain_source = plain_source(plain.is_some(), env_present);
        let resolved = WebhookPlain {
            url: plain
                .as_ref()
                .and_then(|p| p.url.clone())
                .or_else(|| env_nonempty("LUMEN_DETECTION_WEBHOOK_URL")),
        };
        Ok(IntegrationStatus {
            id: id::WEBHOOK,
            plain: serde_json::to_value(resolved)?,
            plain_source,
            // Webhook has no separate secret slot — the URL itself
            // carries the secret token. For consistency with the
            // other integrations we still report the same shape.
            secret_configured: false,
            secret_source: None,
        })
    }

    // ── OIDC ──────────────────────────────────────────────────────────────

    /// Resolve the full OIDC config. Returns `None` when issuer URL or
    /// client_id is missing — those are the two pieces the login flow
    /// can't proceed without. Group mappings are allowed to be empty
    /// (in which case every authenticated user gets rejected; useful
    /// for testing the discovery half before role-mapping is set up).
    #[allow(dead_code)] // Consumed by the OIDC login flow in #14.
    pub fn get_oidc(&self) -> Result<Option<OidcConfig>> {
        let plain = self.read_plain::<OidcPlain>(id::OIDC)?;
        let issuer_url = plain.as_ref().and_then(|p| p.issuer_url.clone());
        let client_id = plain.as_ref().and_then(|p| p.client_id.clone());
        let client_secret = self.secrets.get(secret_name::OIDC_CLIENT_SECRET)?;
        let group_mappings = plain
            .as_ref()
            .map(|p| p.group_mappings.clone())
            .unwrap_or_default();
        match (issuer_url, client_id, client_secret) {
            (Some(issuer_url), Some(client_id), Some(client_secret)) => Ok(Some(OidcConfig {
                issuer_url,
                client_id,
                client_secret,
                group_mappings,
            })),
            _ => Ok(None),
        }
    }

    pub fn set_oidc(
        &self,
        issuer_url: Option<String>,
        client_id: Option<String>,
        client_secret: Option<String>,
        group_mappings: Option<Vec<OidcGroupMapping>>,
    ) -> Result<()> {
        // Read existing → patch → write. Lets callers update one field
        // at a time without having to re-send the whole thing.
        let mut existing = self.read_plain::<OidcPlain>(id::OIDC)?.unwrap_or_default();
        if let Some(url) = issuer_url {
            let trimmed = url.trim();
            existing.issuer_url = if trimmed.is_empty() {
                None
            } else {
                Some(trimmed.trim_end_matches('/').to_string())
            };
        }
        if let Some(cid) = client_id {
            let trimmed = cid.trim();
            existing.client_id = if trimmed.is_empty() {
                None
            } else {
                Some(trimmed.to_string())
            };
        }
        if let Some(mappings) = group_mappings {
            // Reject duplicate group names — first-match-wins makes
            // duplicates silently dead, surface the mistake at edit time.
            let mut seen = std::collections::HashSet::new();
            for m in &mappings {
                let g = m.group.trim();
                if g.is_empty() {
                    anyhow::bail!("group mapping has an empty group name");
                }
                if !seen.insert(g.to_lowercase()) {
                    anyhow::bail!("duplicate group mapping for '{g}'");
                }
            }
            existing.group_mappings = mappings
                .into_iter()
                .map(|m| OidcGroupMapping {
                    group: m.group.trim().to_string(),
                    role: m.role,
                })
                .collect();
        }
        self.write_plain(id::OIDC, &existing)?;
        if let Some(secret) = client_secret {
            let trimmed = secret.trim();
            if trimmed.is_empty() {
                self.secrets.delete(secret_name::OIDC_CLIENT_SECRET)?;
            } else {
                self.secrets.put(secret_name::OIDC_CLIENT_SECRET, trimmed)?;
            }
        }
        Ok(())
    }

    pub fn clear_oidc(&self) -> Result<()> {
        self.delete_plain(id::OIDC)?;
        self.secrets.delete(secret_name::OIDC_CLIENT_SECRET)?;
        Ok(())
    }

    pub fn oidc_status(&self) -> Result<IntegrationStatus> {
        let plain = self.read_plain::<OidcPlain>(id::OIDC)?;
        let plain_source = plain_source(plain.is_some(), false);
        let resolved = plain.unwrap_or_default();
        let secret_in_db = self.secrets.has(secret_name::OIDC_CLIENT_SECRET)?;
        Ok(IntegrationStatus {
            id: id::OIDC,
            plain: serde_json::to_value(resolved)?,
            plain_source,
            secret_configured: secret_in_db,
            secret_source: plain_source_for(secret_in_db, false),
        })
    }

    /// One-stop status fetch for the admin UI.
    pub fn all_statuses(&self) -> Result<Vec<IntegrationStatus>> {
        Ok(vec![
            self.unifi_labels_status()?,
            self.unifi_ips_status()?,
            self.webhook_status()?,
            self.oidc_status()?,
        ])
    }
}

/// `std::env::var` but returns `None` for empty strings — UniFi UDMs
/// in particular tend to leave blanks in `.env` rather than removing
/// the line, and an empty URL is no URL.
fn env_nonempty(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

fn any_unifi_ips_env_set() -> bool {
    env_nonempty("UDM_CONTROLLER_URL").is_some()
        || env_nonempty("UDM_USERNAME").is_some()
        || env_nonempty("UDM_SITE").is_some()
}

/// Map (db_set, env_set) → source label, preferring DB if both.
fn plain_source(db_set: bool, env_set: bool) -> Option<Source> {
    match (db_set, env_set) {
        (true, _) => Some(Source::Db),
        (false, true) => Some(Source::Env),
        (false, false) => None,
    }
}

fn plain_source_for(db_set: bool, env_set: bool) -> Option<Source> {
    plain_source(db_set, env_set)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn open_store() -> (tempfile::TempDir, SettingsStore) {
        let dir = tempdir().unwrap();
        let db = Arc::new(Database::create(dir.path().join("t.redb")).unwrap());
        let secrets = SecretStore::with_key(db.clone(), &[7u8; 32]).unwrap();
        let store = SettingsStore::new(db, secrets).unwrap();
        (dir, store)
    }

    use crate::test_env::clear_env;

    #[test]
    fn unifi_labels_round_trip() {
        let _g = clear_env();
        let (_dir, store) = open_store();
        assert!(store.get_unifi_labels().unwrap().is_none());
        store
            .set_unifi_labels(
                Some("https://192.168.0.1/proxy/network/integration/v1/sites".to_string()),
                Some("api-key-xyz".to_string()),
            )
            .unwrap();
        let cfg = store.get_unifi_labels().unwrap().unwrap();
        assert_eq!(cfg.api_key, "api-key-xyz");
        assert!(cfg.url.contains("192.168.0.1"));
    }

    #[test]
    fn unifi_ips_round_trip_with_default_site() {
        let _g = clear_env();
        let (_dir, store) = open_store();
        store
            .set_unifi_ips(
                Some("https://192.168.0.1".to_string()),
                Some("lumen".to_string()),
                Some("p@ss".to_string()),
                None,
            )
            .unwrap();
        let cfg = store.get_unifi_ips().unwrap().unwrap();
        assert_eq!(cfg.controller_url, "https://192.168.0.1");
        assert_eq!(cfg.username, "lumen");
        assert_eq!(cfg.password, "p@ss");
        assert_eq!(cfg.site, "default"); // implicit
    }

    #[test]
    fn unifi_ips_returns_none_when_password_missing() {
        let _g = clear_env();
        let (_dir, store) = open_store();
        store
            .set_unifi_ips(
                Some("https://192.168.0.1".to_string()),
                Some("lumen".to_string()),
                None,
                None,
            )
            .unwrap();
        assert!(store.get_unifi_ips().unwrap().is_none());
    }

    #[test]
    fn env_fills_in_for_unconfigured_db() {
        let _g = clear_env();
        let (_dir, store) = open_store();
        unsafe {
            std::env::set_var("UDM_URL", "https://from-env/sites");
            std::env::set_var("UDM_API_KEY", "env-key");
        }
        let cfg = store.get_unifi_labels().unwrap().unwrap();
        assert_eq!(cfg.api_key, "env-key");
        assert!(cfg.url.contains("from-env"));
        let status = store.unifi_labels_status().unwrap();
        assert_eq!(status.plain_source, Some(Source::Env));
        assert_eq!(status.secret_source, Some(Source::Env));
        assert!(status.secret_configured);
    }

    #[test]
    fn db_overrides_env_when_both_set() {
        let _g = clear_env();
        let (_dir, store) = open_store();
        unsafe {
            std::env::set_var("UDM_URL", "https://from-env/sites");
            std::env::set_var("UDM_API_KEY", "env-key");
        }
        store
            .set_unifi_labels(
                Some("https://from-db/sites".to_string()),
                Some("db-key".to_string()),
            )
            .unwrap();
        let cfg = store.get_unifi_labels().unwrap().unwrap();
        assert_eq!(cfg.api_key, "db-key");
        assert!(cfg.url.contains("from-db"));
        let status = store.unifi_labels_status().unwrap();
        assert_eq!(status.plain_source, Some(Source::Db));
        assert_eq!(status.secret_source, Some(Source::Db));
    }

    #[test]
    fn clear_removes_both_plain_and_secret() {
        let _g = clear_env();
        let (_dir, store) = open_store();
        store
            .set_unifi_labels(Some("https://x/sites".to_string()), Some("k".to_string()))
            .unwrap();
        store.clear_unifi_labels().unwrap();
        let status = store.unifi_labels_status().unwrap();
        assert_eq!(status.plain_source, None);
        assert_eq!(status.secret_source, None);
        assert!(!status.secret_configured);
    }

    #[test]
    fn webhook_url_round_trip() {
        let _g = clear_env();
        let (_dir, store) = open_store();
        store
            .set_webhook(Some("https://hooks.slack.com/services/x".to_string()))
            .unwrap();
        let cfg = store.get_webhook().unwrap().unwrap();
        assert!(cfg.url.contains("slack.com"));
    }

    #[test]
    fn all_statuses_covers_every_integration() {
        let _g = clear_env();
        let (_dir, store) = open_store();
        let statuses = store.all_statuses().unwrap();
        let ids: Vec<&str> = statuses.iter().map(|s| s.id).collect();
        assert_eq!(
            ids,
            vec![id::UNIFI_LABELS, id::UNIFI_IPS, id::WEBHOOK, id::OIDC]
        );
    }

    // ── OIDC ──────────────────────────────────────────────────────────────

    #[test]
    fn oidc_round_trip() {
        let _g = clear_env();
        let (_dir, store) = open_store();
        assert!(store.get_oidc().unwrap().is_none());
        store
            .set_oidc(
                Some("https://idp.example.com/".to_string()),
                Some("lumen-client".to_string()),
                Some("super-secret".to_string()),
                Some(vec![
                    OidcGroupMapping {
                        group: "Lumen Admins".to_string(),
                        role: Role::Admin,
                    },
                    OidcGroupMapping {
                        group: "Lumen Viewers".to_string(),
                        role: Role::Viewer,
                    },
                ]),
            )
            .unwrap();
        let cfg = store.get_oidc().unwrap().unwrap();
        assert_eq!(cfg.issuer_url, "https://idp.example.com"); // trailing / stripped
        assert_eq!(cfg.client_id, "lumen-client");
        assert_eq!(cfg.client_secret, "super-secret");
        assert_eq!(cfg.group_mappings.len(), 2);
        assert_eq!(cfg.group_mappings[0].role, Role::Admin);
    }

    #[test]
    fn oidc_secret_never_in_status() {
        let _g = clear_env();
        let (_dir, store) = open_store();
        store
            .set_oidc(
                Some("https://idp.example.com".to_string()),
                Some("client".to_string()),
                Some("plaintext-secret".to_string()),
                None,
            )
            .unwrap();
        let status = store.oidc_status().unwrap();
        let json = serde_json::to_string(&status).unwrap();
        assert!(!json.contains("plaintext-secret"));
        assert!(json.contains("\"secret_configured\":true"));
    }

    #[test]
    fn oidc_duplicate_group_rejected() {
        let _g = clear_env();
        let (_dir, store) = open_store();
        let err = store
            .set_oidc(
                Some("https://idp.example.com".to_string()),
                Some("c".to_string()),
                Some("s".to_string()),
                Some(vec![
                    OidcGroupMapping {
                        group: "team".to_string(),
                        role: Role::Admin,
                    },
                    OidcGroupMapping {
                        group: "Team".to_string(), // case-insensitive collision
                        role: Role::Viewer,
                    },
                ]),
            )
            .unwrap_err();
        assert!(err.to_string().to_lowercase().contains("duplicate"));
    }

    #[test]
    fn oidc_clear_removes_all_fields() {
        let _g = clear_env();
        let (_dir, store) = open_store();
        store
            .set_oidc(
                Some("https://idp.example.com".to_string()),
                Some("c".to_string()),
                Some("s".to_string()),
                None,
            )
            .unwrap();
        store.clear_oidc().unwrap();
        let status = store.oidc_status().unwrap();
        assert_eq!(status.plain_source, None);
        assert!(!status.secret_configured);
    }

    #[test]
    fn oidc_partial_update_preserves_secret() {
        let _g = clear_env();
        let (_dir, store) = open_store();
        store
            .set_oidc(
                Some("https://idp.example.com".to_string()),
                Some("c1".to_string()),
                Some("the-secret".to_string()),
                None,
            )
            .unwrap();
        // Change just the client_id; don't touch the secret.
        store
            .set_oidc(None, Some("c2".to_string()), None, None)
            .unwrap();
        let cfg = store.get_oidc().unwrap().unwrap();
        assert_eq!(cfg.client_id, "c2");
        assert_eq!(cfg.client_secret, "the-secret");
    }

    #[test]
    fn status_never_contains_secret_value() {
        let _g = clear_env();
        let (_dir, store) = open_store();
        store
            .set_unifi_labels(
                Some("https://x/sites".to_string()),
                Some("super-secret-key-do-not-leak".to_string()),
            )
            .unwrap();
        let statuses = store.all_statuses().unwrap();
        let serialized = serde_json::to_string(&statuses).unwrap();
        assert!(
            !serialized.contains("super-secret-key-do-not-leak"),
            "status response leaked the secret: {serialized}"
        );
        assert!(serialized.contains("\"secret_configured\":true"));
    }

    #[test]
    fn empty_strings_are_treated_as_unset() {
        let _g = clear_env();
        let (_dir, store) = open_store();
        store
            .set_unifi_labels(Some("".to_string()), Some("".to_string()))
            .unwrap();
        assert!(store.get_unifi_labels().unwrap().is_none());
    }
}
