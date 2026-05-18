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
}

/// Names of the secret-store entries. Stable across versions — these
/// are persisted into the encrypted DB.
mod secret_name {
    pub const UDM_API_KEY: &str = "unifi_labels.api_key";
    pub const UDM_PASSWORD: &str = "unifi_ips.password";
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

    /// One-stop status fetch for the admin UI.
    pub fn all_statuses(&self) -> Result<Vec<IntegrationStatus>> {
        Ok(vec![
            self.unifi_labels_status()?,
            self.unifi_ips_status()?,
            self.webhook_status()?,
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

    /// Wipe any env vars the tests might collide with. Tests touch
    /// real process env so we keep the mutations narrow.
    fn clear_env() {
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
    }

    #[test]
    fn unifi_labels_round_trip() {
        clear_env();
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
        clear_env();
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
        clear_env();
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
        clear_env();
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
        clear_env();
    }

    #[test]
    fn db_overrides_env_when_both_set() {
        clear_env();
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
        clear_env();
    }

    #[test]
    fn clear_removes_both_plain_and_secret() {
        clear_env();
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
        clear_env();
        let (_dir, store) = open_store();
        store
            .set_webhook(Some("https://hooks.slack.com/services/x".to_string()))
            .unwrap();
        let cfg = store.get_webhook().unwrap().unwrap();
        assert!(cfg.url.contains("slack.com"));
    }

    #[test]
    fn all_statuses_covers_three_integrations() {
        clear_env();
        let (_dir, store) = open_store();
        let statuses = store.all_statuses().unwrap();
        let ids: Vec<&str> = statuses.iter().map(|s| s.id).collect();
        assert_eq!(ids, vec![id::UNIFI_LABELS, id::UNIFI_IPS, id::WEBHOOK]);
    }

    #[test]
    fn status_never_contains_secret_value() {
        clear_env();
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
        clear_env();
        let (_dir, store) = open_store();
        store
            .set_unifi_labels(Some("".to_string()), Some("".to_string()))
            .unwrap();
        assert!(store.get_unifi_labels().unwrap().is_none());
    }
}
