//! Lifecycle owner for the long-running integration pollers.
//!
//! Each integration runs as a background `tokio::spawn` task; the
//! supervisor holds an `AbortHandle` per integration so the admin
//! settings API can tear one down and replace it without restarting
//! the daemon. There's exactly one task per integration at a time —
//! a `reload_*` call aborts the existing one first.
//!
//! Concurrency: the supervisor is cheap to clone (`Arc<Inner>`) and
//! every mutating method takes `&self`. The internal mutex is held
//! only across the spawn/abort, which is non-async — no risk of
//! holding it across an await.

use std::sync::{Arc, Mutex};

use anyhow::Result;
use tokio::task::AbortHandle;
use tracing::{info, warn};

use crate::detections::DetectionBus;
use crate::settings::SettingsStore;
use crate::state::LiveStateEngine;

use super::diagnostics::Diagnostics;
use super::{unifi, unifi_ips, webhook};

/// Names match `settings::id::*` so the API and logs use the same
/// vocabulary as the settings store. Kept here too rather than
/// exporting from `settings::id` to keep the supervisor module
/// self-contained for future module-graph cleanups.
pub mod kind {
    pub const UNIFI_LABELS: &str = "unifi_labels";
    pub const UNIFI_IPS: &str = "unifi_ips";
    pub const WEBHOOK: &str = "webhook";
}

#[derive(Default)]
struct Slots {
    unifi_labels: Option<AbortHandle>,
    unifi_ips: Option<AbortHandle>,
    webhook: Option<AbortHandle>,
}

#[derive(Clone)]
pub struct IntegrationSupervisor {
    inner: Arc<Inner>,
}

struct Inner {
    slots: Mutex<Slots>,
    engine: LiveStateEngine,
    detections: DetectionBus,
    settings: SettingsStore,
    diagnostics: Diagnostics,
}

impl IntegrationSupervisor {
    pub fn new(engine: LiveStateEngine, detections: DetectionBus, settings: SettingsStore) -> Self {
        Self {
            inner: Arc::new(Inner {
                slots: Mutex::new(Slots::default()),
                engine,
                detections,
                settings,
                diagnostics: Diagnostics::new(),
            }),
        }
    }

    /// Diagnostics handle for the admin API to read most-recent-poll
    /// outcomes from. Each integration's spawn() is wired up here
    /// with a private recorder.
    pub fn diagnostics(&self) -> Diagnostics {
        self.inner.diagnostics.clone()
    }

    /// Boot every integration that currently has a complete config.
    /// Called once at startup, after the daemon is fully wired but
    /// before HTTP traffic starts.
    pub fn start_all(&self) {
        // Each reload is independent — log + continue on failure so a
        // bad config for one integration doesn't sink the others.
        if let Err(e) = self.reload_unifi_labels() {
            warn!(error = %e, "supervisor: unifi_labels start failed");
        }
        if let Err(e) = self.reload_unifi_ips() {
            warn!(error = %e, "supervisor: unifi_ips start failed");
        }
        if let Err(e) = self.reload_webhook() {
            warn!(error = %e, "supervisor: webhook start failed");
        }
    }

    /// Restart UniFi labels integration from the current settings.
    /// Aborts the running task first (if any). If no config is set,
    /// leaves the slot empty.
    pub fn reload_unifi_labels(&self) -> Result<()> {
        self.stop(kind::UNIFI_LABELS);
        let Some(cfg) = self.inner.settings.get_unifi_labels()? else {
            info!(
                kind = kind::UNIFI_LABELS,
                "supervisor: no config — integration stays stopped"
            );
            return Ok(());
        };
        match unifi::UnifiClient::new(cfg.url.clone(), cfg.api_key.clone()) {
            Ok(client) => {
                let recorder = self.inner.diagnostics.recorder_for(kind::UNIFI_LABELS);
                let handle = unifi::spawn(client, self.inner.engine.clone(), recorder);
                info!(kind = kind::UNIFI_LABELS, url = %cfg.url, "supervisor: started");
                self.inner.slots.lock().unwrap().unifi_labels = Some(handle);
            }
            Err(e) => {
                warn!(kind = kind::UNIFI_LABELS, error = %e, "supervisor: init failed");
            }
        }
        Ok(())
    }

    pub fn reload_unifi_ips(&self) -> Result<()> {
        self.stop(kind::UNIFI_IPS);
        let Some(cfg) = self.inner.settings.get_unifi_ips()? else {
            info!(
                kind = kind::UNIFI_IPS,
                "supervisor: no config — integration stays stopped"
            );
            return Ok(());
        };
        match unifi_ips::UnifiIpsClient::new(
            cfg.controller_url.clone(),
            cfg.site.clone(),
            cfg.username.clone(),
            cfg.password.clone(),
        ) {
            Ok(client) => {
                let recorder = self.inner.diagnostics.recorder_for(kind::UNIFI_IPS);
                let handle = unifi_ips::spawn(client, self.inner.detections.clone(), recorder);
                info!(
                    kind = kind::UNIFI_IPS,
                    url = %cfg.controller_url,
                    site = %cfg.site,
                    "supervisor: started"
                );
                self.inner.slots.lock().unwrap().unifi_ips = Some(handle);
            }
            Err(e) => {
                warn!(kind = kind::UNIFI_IPS, error = %e, "supervisor: init failed");
            }
        }
        Ok(())
    }

    pub fn reload_webhook(&self) -> Result<()> {
        self.stop(kind::WEBHOOK);
        let Some(cfg) = self.inner.settings.get_webhook()? else {
            info!(
                kind = kind::WEBHOOK,
                "supervisor: no config — integration stays stopped"
            );
            return Ok(());
        };
        let recorder = self.inner.diagnostics.recorder_for(kind::WEBHOOK);
        if let Some(handle) =
            webhook::spawn(cfg.url.clone(), self.inner.detections.clone(), recorder)
        {
            info!(kind = kind::WEBHOOK, url = %cfg.url, "supervisor: started");
            self.inner.slots.lock().unwrap().webhook = Some(handle);
        }
        Ok(())
    }

    /// Abort an integration without reloading it. Idempotent.
    pub fn stop(&self, name: &str) {
        let mut slots = self.inner.slots.lock().unwrap();
        let slot = match name {
            kind::UNIFI_LABELS => &mut slots.unifi_labels,
            kind::UNIFI_IPS => &mut slots.unifi_ips,
            kind::WEBHOOK => &mut slots.webhook,
            other => {
                warn!(
                    kind = other,
                    "supervisor: unknown integration name; ignored"
                );
                return;
            }
        };
        if let Some(handle) = slot.take() {
            handle.abort();
            info!(kind = name, "supervisor: stopped");
            // Clear stale diagnostics so the UI doesn't keep showing
            // "last poll: 5min ago" for an integration that's not
            // running anymore.
            self.inner.diagnostics.clear(name);
        }
    }

    /// True if the named integration currently has a running task.
    /// Used by the admin API's GET to report runtime status.
    pub fn is_running(&self, name: &str) -> bool {
        let slots = self.inner.slots.lock().unwrap();
        let slot = match name {
            kind::UNIFI_LABELS => &slots.unifi_labels,
            kind::UNIFI_IPS => &slots.unifi_ips,
            kind::WEBHOOK => &slots.webhook,
            _ => return false,
        };
        slot.as_ref().is_some_and(|h| !h.is_finished())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::secret_store::SecretStore;
    use redb::Database;
    use std::sync::Arc;
    use tempfile::tempdir;

    fn open_supervisor() -> (tempfile::TempDir, IntegrationSupervisor) {
        let dir = tempdir().unwrap();
        let db = Arc::new(Database::create(dir.path().join("t.redb")).unwrap());
        let secrets = SecretStore::with_key(db.clone(), &[3u8; 32]).unwrap();
        let settings = SettingsStore::new(db, secrets).unwrap();
        let sup = IntegrationSupervisor::new(LiveStateEngine::new(), DetectionBus::new(), settings);
        (dir, sup)
    }

    #[tokio::test]
    async fn unconfigured_integrations_stay_stopped() {
        let (_dir, sup) = open_supervisor();
        // Clear any ambient env vars that might leak from the host.
        for k in [
            "UDM_URL",
            "UDM_API_KEY",
            "UDM_CONTROLLER_URL",
            "UDM_USERNAME",
            "UDM_PASSWORD",
            "LUMEN_DETECTION_WEBHOOK_URL",
        ] {
            unsafe { std::env::remove_var(k) };
        }
        sup.start_all();
        assert!(!sup.is_running(kind::UNIFI_LABELS));
        assert!(!sup.is_running(kind::UNIFI_IPS));
        assert!(!sup.is_running(kind::WEBHOOK));
    }

    #[tokio::test]
    async fn stop_is_idempotent_on_empty_slot() {
        let (_dir, sup) = open_supervisor();
        // Should not panic / error.
        sup.stop(kind::UNIFI_LABELS);
        sup.stop(kind::UNIFI_LABELS);
        sup.stop("unknown-kind");
    }

    #[tokio::test]
    async fn reloading_replaces_the_running_task() {
        // Configure webhook so we get an actual running task, then
        // reload and confirm the slot is still populated (the abort
        // handle is fresh — old one aborted).
        let (_dir, sup) = open_supervisor();
        sup.inner
            .settings
            .set_webhook(Some("https://example.invalid/hook".to_string()))
            .unwrap();
        sup.reload_webhook().unwrap();
        assert!(sup.is_running(kind::WEBHOOK));
        sup.reload_webhook().unwrap();
        assert!(sup.is_running(kind::WEBHOOK));
        sup.stop(kind::WEBHOOK);
        // After abort, the task may take a moment to drop; just check
        // the slot was cleared.
        let slots = sup.inner.slots.lock().unwrap();
        assert!(slots.webhook.is_none());
    }
}
