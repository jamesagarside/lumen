//! UniFi IPS / IDS detection provider (legacy controller API).
//!
//! The UniFi *Network Integration API* (used by `integrations::unifi`
//! for client labelling) does not yet expose IPS/IDS alarms. So for
//! detection events we fall back to the original controller API,
//! which has lived under `/api/...` on classic Network Application
//! installs and `/proxy/network/api/...` on UDM-class hardware
//! (UDM, UDM Pro, UDM SE, UDR, UCG) since the Unified OS shipped.
//!
//! This module targets the UDM-class path because that's the bullseye
//! homelab gateway. The auth flow is cookie-based:
//!
//! 1. `POST /api/auth/login` with `{username,password}` → controller
//!    sets a `TOKEN` cookie and returns an `X-CSRF-Token` response
//!    header.
//! 2. Subsequent requests carry the cookie automatically (we use
//!    reqwest's cookie jar) and echo the CSRF token back in the
//!    `X-CSRF-Token` request header.
//! 3. On 401 we drop the CSRF token and re-login on the next poll.
//!
//! Alarms come from `/proxy/network/api/s/{site}/stat/alarm`. The
//! controller returns the most recent ~1000 entries. We dedupe by
//! the alarm `_id` and emit only *new* alarms to the detection bus,
//! so a restart followed by a poll doesn't replay history. On the
//! very first successful poll we still suppress old alarms — the
//! product surfaces "what's happening now", not a backfill.

use std::collections::{HashSet, VecDeque};
use std::net::IpAddr;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, Context, Result};
use lumen_core::{Agent, DetectionEvent, EventKind, Rule, Severity};
use reqwest::{header, StatusCode};
use serde::Deserialize;
use serde_json::Value;
use tracing::{debug, info, instrument, warn};

use crate::detections::DetectionBus;
use crate::integrations::diagnostics::Recorder;

const POLL_INTERVAL: Duration = Duration::from_secs(30);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(15);
/// Cap on the dedupe set. Alarms older than the oldest tracked id
/// can re-fire, but in practice the controller only returns ~1000
/// per page so this is plenty of headroom.
const DEDUPE_CAPACITY: usize = 4096;
/// On a freshly-configured integration we publish the N most recent
/// alarms immediately (even though they're "historical" from the
/// controller's POV) so the admin sees signal in the sidebar within
/// seconds of saving credentials. Without this they'd configure the
/// integration and then stare at nothing until a brand-new alarm
/// fires. The full 30-day backfill + risk-score model lands in a
/// follow-up PR; this is the smallest possible immediate-signal fix.
const INITIAL_BACKFILL: usize = 5;

#[derive(Clone)]
pub struct UnifiIpsClient {
    http: reqwest::Client,
    /// Base controller URL, e.g. `https://192.168.0.1`. No trailing
    /// slash, no path — we build endpoints on top.
    base_url: String,
    site: String,
    username: String,
    password: String,
    /// CSRF token from the last successful login. UDM controllers
    /// require it on every mutating request; for safety we always
    /// send it on GETs too when we have one (the controller ignores
    /// it on GETs, but sending it costs nothing).
    csrf: Arc<Mutex<Option<String>>>,
}

impl UnifiIpsClient {
    pub fn new(base_url: String, site: String, username: String, password: String) -> Result<Self> {
        let base_url = base_url.trim_end_matches('/').to_string();
        let http = reqwest::Client::builder()
            // UDMs ship self-signed certs by default; same posture as
            // the integration-API client.
            .danger_accept_invalid_certs(true)
            .cookie_store(true)
            .timeout(REQUEST_TIMEOUT)
            .build()
            .context("building UniFi IPS HTTP client")?;
        Ok(Self {
            http,
            base_url,
            site,
            username,
            password,
            csrf: Arc::new(Mutex::new(None)),
        })
    }

    fn csrf_token(&self) -> Option<String> {
        self.csrf.lock().expect("csrf mutex poisoned").clone()
    }

    fn store_csrf(&self, token: Option<String>) {
        *self.csrf.lock().expect("csrf mutex poisoned") = token;
    }

    /// Authenticate against the controller. Stores the CSRF token on
    /// success; the cookie jar handles the session cookie.
    async fn login(&self) -> Result<()> {
        let url = format!("{}/api/auth/login", self.base_url);
        let body = serde_json::json!({
            "username": self.username,
            "password": self.password,
            // The Web UI sends `remember: true`. The session TTL is
            // controlled by the controller; we re-login on 401 anyway.
            "remember": true,
        });
        let response = self
            .http
            .post(&url)
            .header(header::ACCEPT, "application/json")
            .json(&body)
            .send()
            .await
            .with_context(|| format!("POST {url}"))?;
        let status = response.status();
        let csrf = response
            .headers()
            .get("x-csrf-token")
            .and_then(|v| v.to_str().ok())
            .map(str::to_string);
        if !status.is_success() {
            return Err(anyhow!(
                "UniFi IPS login failed: {status} (check UDM_USERNAME / UDM_PASSWORD)"
            ));
        }
        self.store_csrf(csrf);
        Ok(())
    }

    /// Fetch all current alarms for the configured site. Re-auths
    /// transparently on 401.
    async fn fetch_alarms(&self) -> Result<Vec<Alarm>> {
        // First attempt; if 401, login and retry once. Two attempts
        // max — if it still fails after a fresh login, the creds are
        // wrong and we should surface that on the next tick rather
        // than loop.
        for attempt in 0..2u8 {
            let url = format!(
                "{}/proxy/network/api/s/{}/stat/alarm",
                self.base_url, self.site
            );
            let mut req = self
                .http
                .get(&url)
                .header(header::ACCEPT, "application/json");
            if let Some(token) = self.csrf_token() {
                req = req.header("X-CSRF-Token", token);
            }
            let resp = req.send().await.with_context(|| format!("GET {url}"))?;
            let status = resp.status();
            // Keep the CSRF token fresh — the controller rotates it.
            if let Some(t) = resp
                .headers()
                .get("x-csrf-token")
                .and_then(|v| v.to_str().ok())
            {
                self.store_csrf(Some(t.to_string()));
            }
            if status == StatusCode::UNAUTHORIZED && attempt == 0 {
                debug!("unifi-ips: 401 on alarms fetch, re-authenticating");
                self.login().await?;
                continue;
            }
            if !status.is_success() {
                return Err(anyhow!("GET {url} returned {status}"));
            }
            let body = resp
                .text()
                .await
                .with_context(|| format!("GET {url}: read body"))?;
            let env: AlarmEnvelope = serde_json::from_str(&body).with_context(|| {
                format!(
                    "GET {url}: parse JSON (body starts with {:?})",
                    body.chars().take(80).collect::<String>()
                )
            })?;
            return Ok(env.data);
        }
        Err(anyhow!("unifi-ips: exhausted retries fetching alarms"))
    }
}

/// `{"meta":{...},"data":[...]}` — classic UniFi envelope. We only
/// care about `data`.
#[derive(Debug, Deserialize)]
struct AlarmEnvelope {
    #[serde(default)]
    data: Vec<Alarm>,
}

/// A single IPS alarm as returned by the controller. UniFi sprinkles
/// camelCase and snake_case fairly liberally across firmware versions;
/// `Value` for `extra` keeps anything we don't model accessible
/// downstream.
#[derive(Debug, Deserialize, Clone)]
struct Alarm {
    #[serde(rename = "_id")]
    id: String,
    /// Milliseconds since epoch, per the controller's convention.
    #[serde(default)]
    timestamp: Option<i64>,
    /// 1 = highest, 5 = lowest — the inverse of ECS.
    #[serde(default)]
    severity: Option<u8>,
    /// Some firmwares ship a string severity (`"Critical"` etc.)
    /// instead of (or alongside) the numeric one.
    #[serde(default)]
    catname: Option<String>,
    /// Human-readable rule/event message.
    #[serde(default, alias = "msg")]
    message: Option<String>,
    /// Suricata SID when available.
    #[serde(default)]
    signature_id: Option<String>,
    #[serde(default)]
    signature: Option<String>,
    #[serde(default)]
    category: Option<String>,
    #[serde(default, alias = "srcip")]
    src_ip: Option<String>,
    #[serde(default, alias = "dstip")]
    dst_ip: Option<String>,
    #[serde(default)]
    action: Option<String>,
    /// Anything else the controller volunteers — handed through as
    /// `extra` for the UI to render if it wants to.
    #[serde(flatten)]
    extra: Value,
}

/// Map UniFi's 1-5 severity (1 = highest) into ECS's 1-7 (7 =
/// critical). Falls back to a sensible "low" when missing, since
/// the alarms endpoint by definition only contains things the IPS
/// engine considered worth logging.
fn map_severity(unifi_sev: Option<u8>, catname: Option<&str>) -> Severity {
    if let Some(s) = unifi_sev {
        // 1 → 7, 2 → 6, 3 → 5, 4 → 3, 5 → 1
        let mapped = match s {
            1 => 7,
            2 => 6,
            3 => 5,
            4 => 3,
            _ => 1,
        };
        return Severity::clamped(mapped);
    }
    // Some firmwares only send a string category. Be lenient.
    match catname.map(str::to_ascii_lowercase).as_deref() {
        Some("critical") | Some("emergency") => Severity::CRITICAL,
        Some("high") => Severity::HIGH,
        Some("medium") | Some("warning") => Severity::MEDIUM,
        Some("low") | Some("notice") => Severity::LOW,
        Some("info") | Some("informational") => Severity::INFO,
        _ => Severity::LOW,
    }
}

/// Convert a controller alarm into our ECS-shaped detection event.
/// `controller_base_url` is used to build the deep-link back to the
/// UniFi UI's threat-management page.
fn alarm_to_event(alarm: &Alarm, controller_base_url: &str) -> DetectionEvent {
    let timestamp = alarm
        .timestamp
        .and_then(|ms| u64::try_from(ms).ok())
        .map(|ms| UNIX_EPOCH + Duration::from_millis(ms))
        .unwrap_or_else(SystemTime::now);
    let severity = map_severity(alarm.severity, alarm.catname.as_deref());
    let source_ip: Option<IpAddr> = alarm.src_ip.as_deref().and_then(|s| s.parse().ok());
    let destination_ip: Option<IpAddr> = alarm.dst_ip.as_deref().and_then(|s| s.parse().ok());
    let message = alarm
        .message
        .clone()
        .or_else(|| alarm.signature.clone())
        .unwrap_or_else(|| "UniFi IPS alert".to_string());
    let rule = Rule {
        id: alarm.signature_id.clone(),
        name: alarm.signature.clone(),
        description: None,
        category: alarm.category.clone(),
    };
    // Threat-management page in the UniFi UI is per-controller; the
    // alarm `_id` is the route param. Works on UDM-class hardware.
    let url_original = Some(format!(
        "{}/network/default/insights/triggers/alarms/{}",
        controller_base_url.trim_end_matches('/'),
        alarm.id
    ));
    DetectionEvent {
        timestamp,
        kind: EventKind::Alert,
        category: vec!["intrusion_detection".to_string(), "network".to_string()],
        severity,
        action: alarm.action.clone(),
        message,
        rule,
        agent: Agent {
            type_: "unifi-ips".to_string(),
            vendor: Some("Ubiquiti".to_string()),
            version: None,
        },
        source_ip,
        destination_ip,
        url_original,
        extra: alarm.extra.clone(),
    }
}

/// Bounded FIFO dedupe set. Tracks alarm ids we've already published
/// so a controller that keeps returning the same backlog never causes
/// duplicate events. Insert order = eviction order.
struct DedupeSet {
    seen: HashSet<String>,
    order: VecDeque<String>,
    cap: usize,
}

impl DedupeSet {
    fn new(cap: usize) -> Self {
        Self {
            seen: HashSet::new(),
            order: VecDeque::new(),
            cap,
        }
    }

    /// Returns true if `id` is new to this set (and records it).
    fn observe(&mut self, id: &str) -> bool {
        if self.seen.contains(id) {
            return false;
        }
        self.seen.insert(id.to_string());
        self.order.push_back(id.to_string());
        while self.order.len() > self.cap {
            if let Some(old) = self.order.pop_front() {
                self.seen.remove(&old);
            }
        }
        true
    }

    fn prime(&mut self, ids: impl IntoIterator<Item = String>) {
        for id in ids {
            if self.seen.insert(id.clone()) {
                self.order.push_back(id);
            }
        }
        while self.order.len() > self.cap {
            if let Some(old) = self.order.pop_front() {
                self.seen.remove(&old);
            }
        }
    }
}

/// Spawn the IPS poller. Runs until aborted; per-tick errors are
/// logged and the loop continues. Returns the `AbortHandle` so the
/// supervisor can stop it when admin settings change.
pub fn spawn(
    client: UnifiIpsClient,
    bus: DetectionBus,
    diag: Recorder,
) -> tokio::task::AbortHandle {
    let handle = tokio::spawn(async move {
        info!(
            site = %client.site,
            interval_secs = POLL_INTERVAL.as_secs(),
            "unifi-ips: poller started"
        );
        // Establish the session up front so a bad password fails
        // loud at startup rather than silently on each poll.
        if let Err(e) = client.login().await {
            warn!(error = %e, "unifi-ips: initial login failed (will retry)");
            diag.record_err(format!("initial login failed: {e:#}"));
        }

        let mut dedupe = DedupeSet::new(DEDUPE_CAPACITY);
        // On the first successful fetch:
        // 1. Sort alarms newest-first by timestamp.
        // 2. Publish the most recent INITIAL_BACKFILL as detection
        //    events so the admin sees signal immediately.
        // 3. Prime the dedupe set with *all* of them — including the
        //    ones we just published — so the next poll only fires for
        //    genuinely new alarms.
        // This is the smallest workable "we configured it, where are
        // my events?" fix; the proper 30-day persistence + risk
        // model is a follow-up PR.
        match client.fetch_alarms().await {
            Ok(mut initial) => {
                let total = initial.len();
                // Newest-first by timestamp; alarms without a
                // timestamp sort last (they have no claim on being
                // "recent").
                initial.sort_by_key(|a| std::cmp::Reverse(a.timestamp.unwrap_or(i64::MIN)));
                let backfill_count = total.min(INITIAL_BACKFILL);
                let mut last_summary = None;
                for alarm in initial.iter().take(backfill_count) {
                    let event = alarm_to_event(alarm, &client.base_url);
                    if last_summary.is_none() {
                        last_summary = Some(format_summary(alarm));
                    }
                    bus.publish(event);
                    dedupe.observe(&alarm.id); // mark as seen before priming the rest
                }
                // Prime the remaining alarms so we don't republish on
                // the next tick. (observe() is a no-op if already
                // present, so the iter is safe.)
                dedupe.prime(initial.into_iter().map(|a| a.id));
                info!(
                    backfilled = backfill_count,
                    primed = total,
                    "unifi-ips: backfilled recent alarms; dedupe primed"
                );
                diag.record_ok(total as u32, backfill_count as u32, last_summary);
            }
            Err(e) => {
                warn!(error = %e, "unifi-ips: initial alarm fetch failed (will retry)");
                diag.record_err(format!("alarm fetch failed: {e:#}"));
            }
        }

        let mut ticker = tokio::time::interval(POLL_INTERVAL);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        // First tick fires immediately; skip it since we already
        // primed above and don't want to double-fetch.
        ticker.tick().await;

        loop {
            ticker.tick().await;
            match poll_with_summary(&client, &bus, &mut dedupe).await {
                Ok(stats) => {
                    if stats.new > 0 {
                        info!(new_events = stats.new, "unifi-ips: published new alarms");
                    } else {
                        debug!("unifi-ips: no new alarms");
                    }
                    diag.record_ok(stats.observed, stats.new, stats.last_summary);
                }
                Err(e) => {
                    warn!(error = %e, "unifi-ips: poll failed");
                    diag.record_err(format!("{e:#}"));
                }
            }
        }
    });
    handle.abort_handle()
}

/// Render an alarm as a one-line "last seen" string for the
/// diagnostics panel. Truncates absurdly long messages so the UI
/// stays tidy.
fn format_summary(alarm: &Alarm) -> String {
    let msg = alarm
        .message
        .as_deref()
        .or(alarm.signature.as_deref())
        .unwrap_or("UniFi IPS alarm");
    if msg.len() > 120 {
        format!("{}…", &msg[..120])
    } else {
        msg.to_string()
    }
}

#[derive(Debug, Default)]
struct PollStats {
    observed: u32,
    new: u32,
    last_summary: Option<String>,
}

#[instrument(skip_all)]
async fn poll_with_summary(
    client: &UnifiIpsClient,
    bus: &DetectionBus,
    dedupe: &mut DedupeSet,
) -> Result<PollStats> {
    let alarms = client.fetch_alarms().await?;
    let mut stats = PollStats {
        observed: alarms.len() as u32,
        ..PollStats::default()
    };
    // Track the newest alarm we saw this tick (any alarm, whether
    // new-to-us or not) so the diagnostics panel always shows the
    // most recent thing the controller is reporting.
    let mut newest_seen: Option<(i64, &Alarm)> = None;
    for alarm in &alarms {
        if let Some(ts) = alarm.timestamp {
            if newest_seen.map(|(t, _)| ts > t).unwrap_or(true) {
                newest_seen = Some((ts, alarm));
            }
        }
        if !dedupe.observe(&alarm.id) {
            continue;
        }
        bus.publish(alarm_to_event(alarm, &client.base_url));
        stats.new += 1;
    }
    if let Some((_, alarm)) = newest_seen {
        stats.last_summary = Some(format_summary(alarm));
    }
    Ok(stats)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn severity_maps_numeric_unifi_to_ecs() {
        // 1 (UniFi critical) → 7 (ECS critical)
        assert_eq!(map_severity(Some(1), None), Severity::CRITICAL);
        assert_eq!(map_severity(Some(2), None), Severity::HIGH);
        assert_eq!(map_severity(Some(3), None), Severity::MEDIUM);
        assert_eq!(map_severity(Some(4), None), Severity::LOW);
        assert_eq!(map_severity(Some(5), None), Severity::INFO);
    }

    #[test]
    fn severity_maps_string_catname_case_insensitive() {
        assert_eq!(map_severity(None, Some("Critical")), Severity::CRITICAL);
        assert_eq!(map_severity(None, Some("HIGH")), Severity::HIGH);
        assert_eq!(map_severity(None, Some("warning")), Severity::MEDIUM);
        assert_eq!(map_severity(None, Some("low")), Severity::LOW);
        assert_eq!(map_severity(None, Some("Informational")), Severity::INFO);
    }

    #[test]
    fn severity_defaults_to_low_when_unknown() {
        assert_eq!(map_severity(None, None), Severity::LOW);
        assert_eq!(map_severity(None, Some("nonsense")), Severity::LOW);
    }

    #[test]
    fn alarm_parses_real_unifi_payload() {
        // Trimmed from a real UDM Pro alarm response (firmware 9.x).
        let body = r#"{
            "meta": {"rc": "ok"},
            "data": [
                {
                    "_id": "abc123def4567890",
                    "timestamp": 1715900000000,
                    "severity": 2,
                    "msg": "ET MALWARE Possible Cobalt Strike beacon",
                    "signature_id": "2031234",
                    "signature": "ET MALWARE Cobalt Strike",
                    "category": "intrusion_detection",
                    "srcip": "10.0.0.5",
                    "dstip": "45.33.32.156",
                    "action": "blocked",
                    "key": "EVT_IPS_IpsAlert"
                }
            ]
        }"#;
        let env: AlarmEnvelope = serde_json::from_str(body).unwrap();
        assert_eq!(env.data.len(), 1);
        let alarm = &env.data[0];
        assert_eq!(alarm.id, "abc123def4567890");
        assert_eq!(alarm.severity, Some(2));
        assert_eq!(alarm.signature_id.as_deref(), Some("2031234"));
        assert_eq!(alarm.src_ip.as_deref(), Some("10.0.0.5"));
    }

    #[test]
    fn alarm_to_event_populates_ecs_fields() {
        let alarm = Alarm {
            id: "alarm-1".to_string(),
            timestamp: Some(1_715_900_000_000),
            severity: Some(2),
            catname: None,
            message: Some("ET MALWARE Cobalt Strike beacon".to_string()),
            signature_id: Some("2031234".to_string()),
            signature: Some("ET MALWARE Cobalt Strike".to_string()),
            category: Some("intrusion_detection".to_string()),
            src_ip: Some("10.0.0.5".to_string()),
            dst_ip: Some("45.33.32.156".to_string()),
            action: Some("blocked".to_string()),
            extra: Value::Null,
        };
        let ev = alarm_to_event(&alarm, "https://192.168.0.1");
        assert_eq!(ev.severity, Severity::HIGH);
        assert_eq!(ev.kind, EventKind::Alert);
        assert_eq!(ev.message, "ET MALWARE Cobalt Strike beacon");
        assert_eq!(ev.action.as_deref(), Some("blocked"));
        assert_eq!(ev.agent.type_, "unifi-ips");
        assert_eq!(ev.rule.id.as_deref(), Some("2031234"));
        assert_eq!(ev.rule.name.as_deref(), Some("ET MALWARE Cobalt Strike"));
        assert_eq!(ev.source_ip.unwrap().to_string(), "10.0.0.5");
        assert_eq!(ev.destination_ip.unwrap().to_string(), "45.33.32.156");
        let url = ev.url_original.unwrap();
        assert!(
            url.contains("/network/default/insights/triggers/alarms/alarm-1"),
            "got {url}"
        );
        // Timestamp survives the round-trip through ms-since-epoch.
        let secs = ev.timestamp.duration_since(UNIX_EPOCH).unwrap().as_secs();
        assert_eq!(secs, 1_715_900_000);
    }

    #[test]
    fn alarm_to_event_falls_back_when_msg_missing() {
        let alarm = Alarm {
            id: "alarm-2".to_string(),
            timestamp: None,
            severity: Some(3),
            catname: None,
            message: None,
            signature_id: None,
            signature: Some("ET POLICY Whatever".to_string()),
            category: None,
            src_ip: None,
            dst_ip: None,
            action: None,
            extra: Value::Null,
        };
        let ev = alarm_to_event(&alarm, "https://x");
        // Falls back to signature, then to the static placeholder.
        assert_eq!(ev.message, "ET POLICY Whatever");
        assert_eq!(ev.severity, Severity::MEDIUM);
    }

    #[test]
    fn dedupe_observe_returns_true_only_first_time() {
        let mut d = DedupeSet::new(8);
        assert!(d.observe("a"));
        assert!(!d.observe("a"));
        assert!(d.observe("b"));
        assert!(!d.observe("b"));
    }

    #[test]
    fn dedupe_evicts_in_fifo_order() {
        let mut d = DedupeSet::new(3);
        d.observe("a");
        d.observe("b");
        d.observe("c");
        d.observe("d"); // evicts "a"
        assert!(d.observe("a")); // "a" is forgotten, so it's new again
        assert!(!d.observe("d"));
    }

    #[test]
    fn dedupe_prime_marks_ids_as_seen() {
        let mut d = DedupeSet::new(16);
        d.prime(["a".to_string(), "b".to_string()]);
        assert!(!d.observe("a"));
        assert!(!d.observe("b"));
        assert!(d.observe("c"));
    }

    #[test]
    fn detection_event_serialises_with_ecs_field_names() {
        // Belt-and-braces: ensure the event we produce serialises
        // exactly the way downstream SIEMs expect (verified in
        // lumen_core::detection but worth a smoke test here too).
        let alarm = Alarm {
            id: "x".to_string(),
            timestamp: Some(1_700_000_000_000),
            severity: Some(1),
            catname: None,
            message: Some("test".to_string()),
            signature_id: None,
            signature: None,
            category: None,
            src_ip: Some("1.2.3.4".to_string()),
            dst_ip: None,
            action: None,
            extra: Value::Null,
        };
        let ev = alarm_to_event(&alarm, "https://x");
        let json = serde_json::to_string(&ev).unwrap();
        assert!(json.contains("\"event.severity\":7"));
        assert!(json.contains("\"source.ip\":\"1.2.3.4\""));
        assert!(json.contains("\"agent\""));
    }
}
