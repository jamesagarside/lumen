//! Per-integration diagnostic recorder.
//!
//! Surfaces "is this integration actually working?" in the admin UI.
//! Each integration's poll loop records the outcome of its most
//! recent tick (when, how many items it saw, whether anything went
//! wrong); the admin API reads this back and renders it next to the
//! integration's config.
//!
//! In-memory only — diagnostics aren't worth persisting across
//! restarts. If the daemon restarts, the next poll repopulates.
//!
//! Concurrency: cheap to clone (`Arc<Inner>`). Each integration gets
//! a private `Recorder` handle scoped to its own integration id; the
//! supervisor holds the read side and exposes it through the API.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use serde::Serialize;

/// What happened on the last poll.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum PollOutcome {
    /// Poll completed; integration knows how many items it saw and
    /// how many of those were new (i.e. published as events / labels).
    Ok { observed: u32, new: u32 },
    /// Poll failed. `message` is the user-facing error.
    Err { message: String },
}

/// Snapshot of an integration's most recent activity. Returned by
/// the admin API and rendered in the Settings UI.
#[derive(Debug, Clone, Default, Serialize)]
pub struct IntegrationDiagnostic {
    /// When the most recent poll completed. `None` = never polled
    /// since this daemon started.
    #[serde(serialize_with = "serialize_systemtime_millis")]
    pub last_poll_at: Option<SystemTime>,
    pub last_outcome: Option<PollOutcome>,
    /// Optional one-line teaser for the most recent item seen — e.g.
    /// "ET MALWARE Cobalt Strike at 2025-03-04 12:34:56". Lets the
    /// admin confirm the integration is reading real data even when
    /// `new == 0` because everything's already been observed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_item_summary: Option<String>,
}

fn serialize_systemtime_millis<S: serde::Serializer>(
    t: &Option<SystemTime>,
    s: S,
) -> Result<S::Ok, S::Error> {
    match t {
        None => s.serialize_none(),
        Some(t) => {
            let millis = t
                .duration_since(SystemTime::UNIX_EPOCH)
                .map(|d| d.as_millis() as i64)
                .unwrap_or(0);
            s.serialize_i64(millis)
        }
    }
}

/// Shared diagnostic table. The supervisor owns one; each integration
/// gets a `Recorder` scoped to its own id via `recorder_for`.
#[derive(Clone, Default)]
pub struct Diagnostics {
    inner: Arc<Mutex<HashMap<&'static str, IntegrationDiagnostic>>>,
}

impl Diagnostics {
    pub fn new() -> Self {
        Self::default()
    }

    /// A per-integration write handle. Cloning is cheap.
    pub fn recorder_for(&self, integration: &'static str) -> Recorder {
        Recorder {
            integration,
            inner: self.inner.clone(),
        }
    }

    /// Read the current diagnostic for an integration, returning the
    /// default (everything `None`) if nothing's been recorded yet.
    pub fn get(&self, integration: &str) -> IntegrationDiagnostic {
        self.inner
            .lock()
            .expect("diagnostics mutex poisoned")
            .get(integration)
            .cloned()
            .unwrap_or_default()
    }

    /// Forget everything for an integration — used when the supervisor
    /// stops it, so the UI doesn't keep showing stale state next to a
    /// now-deconfigured integration.
    pub fn clear(&self, integration: &str) {
        self.inner
            .lock()
            .expect("diagnostics mutex poisoned")
            .remove(integration);
    }
}

/// Write handle for a single integration. Each integration's poll
/// loop holds one and calls `record_*` after each tick.
#[derive(Clone)]
pub struct Recorder {
    integration: &'static str,
    inner: Arc<Mutex<HashMap<&'static str, IntegrationDiagnostic>>>,
}

impl Recorder {
    pub fn record_ok(&self, observed: u32, new: u32, last_item_summary: Option<String>) {
        let mut map = self.inner.lock().expect("diagnostics mutex poisoned");
        let entry = map.entry(self.integration).or_default();
        entry.last_poll_at = Some(SystemTime::now());
        entry.last_outcome = Some(PollOutcome::Ok { observed, new });
        if last_item_summary.is_some() {
            entry.last_item_summary = last_item_summary;
        }
    }

    pub fn record_err(&self, message: impl Into<String>) {
        let mut map = self.inner.lock().expect("diagnostics mutex poisoned");
        let entry = map.entry(self.integration).or_default();
        entry.last_poll_at = Some(SystemTime::now());
        entry.last_outcome = Some(PollOutcome::Err {
            message: message.into(),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_diagnostic_serialises_with_all_nones() {
        let d = IntegrationDiagnostic::default();
        let json = serde_json::to_string(&d).unwrap();
        assert!(json.contains("\"last_poll_at\":null"));
        assert!(json.contains("\"last_outcome\":null"));
        // last_item_summary is skip_serializing_if = None
        assert!(!json.contains("last_item_summary"));
    }

    #[test]
    fn record_ok_sets_observed_and_new() {
        let d = Diagnostics::new();
        let r = d.recorder_for("test");
        r.record_ok(10, 3, None);
        let got = d.get("test");
        assert!(got.last_poll_at.is_some());
        match got.last_outcome.unwrap() {
            PollOutcome::Ok { observed, new } => {
                assert_eq!(observed, 10);
                assert_eq!(new, 3);
            }
            _ => panic!("expected Ok outcome"),
        }
    }

    #[test]
    fn record_err_carries_message() {
        let d = Diagnostics::new();
        let r = d.recorder_for("test");
        r.record_err("401 Unauthorized");
        let got = d.get("test");
        match got.last_outcome.unwrap() {
            PollOutcome::Err { message } => assert_eq!(message, "401 Unauthorized"),
            _ => panic!("expected Err outcome"),
        }
    }

    #[test]
    fn last_item_summary_persists_across_polls_that_pass_none() {
        let d = Diagnostics::new();
        let r = d.recorder_for("test");
        r.record_ok(10, 3, Some("first alarm".to_string()));
        r.record_ok(11, 0, None); // newer poll, but no new items to summarise
        let got = d.get("test");
        // We still want to render the most recent *known* alarm even
        // if this poll didn't see anything new.
        assert_eq!(got.last_item_summary.as_deref(), Some("first alarm"));
    }

    #[test]
    fn clear_removes_the_entry() {
        let d = Diagnostics::new();
        let r = d.recorder_for("test");
        r.record_ok(1, 1, None);
        d.clear("test");
        let got = d.get("test");
        assert!(got.last_poll_at.is_none());
        assert!(got.last_outcome.is_none());
    }

    #[test]
    fn get_for_unknown_integration_returns_default() {
        let d = Diagnostics::new();
        let got = d.get("never-recorded");
        assert!(got.last_poll_at.is_none());
        assert!(got.last_outcome.is_none());
    }

    #[test]
    fn poll_outcome_serialises_with_tag() {
        let ok = PollOutcome::Ok {
            observed: 5,
            new: 2,
        };
        let s = serde_json::to_string(&ok).unwrap();
        assert!(s.contains("\"status\":\"ok\""));
        assert!(s.contains("\"observed\":5"));
        let err = PollOutcome::Err {
            message: "bad".to_string(),
        };
        let s = serde_json::to_string(&err).unwrap();
        assert!(s.contains("\"status\":\"err\""));
        assert!(s.contains("\"message\":\"bad\""));
    }
}
