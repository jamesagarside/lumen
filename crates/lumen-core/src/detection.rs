//! Detection events — alerts / IDS hits / policy violations that
//! lumen visualises on top of the flow graph.
//!
//! Per CONTEXT.md §6 the wire format follows the OpenTelemetry
//! semantic conventions (formerly Elastic Common Schema). Concrete
//! field names mirror ECS so downstream tools (Security Onion,
//! Elasticsearch, Sumo, …) can ingest these without translation.
//!
//! v1 carries only what we actually need to *render*. Adding fields
//! is non-breaking thanks to serde defaults; removing or renaming
//! requires a schema version bump.

use std::net::IpAddr;
use std::time::SystemTime;

use serde::{Deserialize, Serialize};

/// Severity follows ECS — 1 (info) → 7 (critical). Values outside
/// this range are clamped on ingest.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Severity(pub u8);

impl Severity {
    pub const INFO: Self = Self(1);
    pub const LOW: Self = Self(3);
    pub const MEDIUM: Self = Self(5);
    pub const HIGH: Self = Self(6);
    pub const CRITICAL: Self = Self(7);

    pub fn clamped(raw: u8) -> Self {
        Self(raw.clamp(1, 7))
    }

    pub fn name(self) -> &'static str {
        match self.0 {
            1..=2 => "info",
            3..=4 => "low",
            5 => "medium",
            6 => "high",
            _ => "critical",
        }
    }
}

/// ECS `event.kind`. We don't carry every option — just what makes
/// sense in our context.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventKind {
    Alert,
    Event,
    Signal,
    State,
}

/// ECS `event.category`. Multiple is the norm — most rules tag a
/// single record with `["intrusion_detection", "network"]`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct EventCategory(pub Vec<String>);

/// Source of the detection — who told us. Mirrors ECS `agent.{type,vendor}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Agent {
    /// Stable identifier of the producer ("unifi-ips", "suricata",
    /// "crowdsec", "manual"). UI groups by this.
    #[serde(rename = "type")]
    pub type_: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vendor: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
}

/// ECS `rule.*` — what fired.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rule {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub category: Option<String>,
}

/// One detection event. ECS-shaped per CONTEXT.md §6.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DetectionEvent {
    /// `@timestamp` in ECS. Daemon-assigned if missing (e.g. POSTed
    /// events from sources that don't have a clock).
    #[serde(rename = "@timestamp")]
    pub timestamp: SystemTime,
    #[serde(rename = "event.kind")]
    pub kind: EventKind,
    #[serde(
        default,
        rename = "event.category",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub category: Vec<String>,
    #[serde(rename = "event.severity")]
    pub severity: Severity,
    #[serde(
        default,
        rename = "event.action",
        skip_serializing_if = "Option::is_none"
    )]
    pub action: Option<String>,
    /// Short human label rendered in the sidebar — the *one line*
    /// summary. Required because every detection should be triageable
    /// without expanding it.
    pub message: String,
    #[serde(default, rename = "rule", skip_serializing_if = "is_default_rule")]
    pub rule: Rule,
    pub agent: Agent,
    #[serde(default, rename = "source.ip", skip_serializing_if = "Option::is_none")]
    pub source_ip: Option<IpAddr>,
    #[serde(
        default,
        rename = "destination.ip",
        skip_serializing_if = "Option::is_none"
    )]
    pub destination_ip: Option<IpAddr>,
    #[serde(
        default,
        rename = "url.original",
        skip_serializing_if = "Option::is_none"
    )]
    pub url_original: Option<String>,
    /// Free-form bag the source can attach for richer UIs. We don't
    /// schema-check this — the renderer treats it as opaque JSON.
    #[serde(
        default,
        rename = "extra",
        skip_serializing_if = "serde_json::Value::is_null"
    )]
    pub extra: serde_json::Value,
}

fn is_default_rule(r: &Rule) -> bool {
    r.id.is_none() && r.name.is_none() && r.description.is_none() && r.category.is_none()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::Ipv4Addr;

    #[test]
    fn severity_clamps_out_of_range() {
        assert_eq!(Severity::clamped(0).0, 1);
        assert_eq!(Severity::clamped(99).0, 7);
        assert_eq!(Severity::clamped(5).0, 5);
    }

    #[test]
    fn severity_names() {
        assert_eq!(Severity::INFO.name(), "info");
        assert_eq!(Severity(2).name(), "info");
        assert_eq!(Severity(3).name(), "low");
        assert_eq!(Severity(5).name(), "medium");
        assert_eq!(Severity(6).name(), "high");
        assert_eq!(Severity(7).name(), "critical");
    }

    #[test]
    fn detection_event_round_trips_through_ecs_field_names() {
        let e = DetectionEvent {
            timestamp: SystemTime::UNIX_EPOCH,
            kind: EventKind::Alert,
            category: vec!["intrusion_detection".to_string(), "network".to_string()],
            severity: Severity::HIGH,
            action: Some("blocked".to_string()),
            message: "ET SCAN possible nmap probe".to_string(),
            rule: Rule {
                id: Some("2008438".to_string()),
                name: Some("ET SCAN NMAP -sS window 1024".to_string()),
                ..Default::default()
            },
            agent: Agent {
                type_: "unifi-ips".to_string(),
                vendor: Some("Ubiquiti".to_string()),
                version: None,
            },
            source_ip: Some(IpAddr::V4(Ipv4Addr::new(45, 33, 32, 156))),
            destination_ip: Some(IpAddr::V4(Ipv4Addr::new(192, 168, 1, 10))),
            url_original: Some("https://192.168.1.1/alerts/1234".to_string()),
            extra: serde_json::Value::Null,
        };
        let json = serde_json::to_string(&e).unwrap();
        // ECS field names must appear verbatim — downstream SIEMs
        // grep for these.
        assert!(json.contains("\"@timestamp\""));
        assert!(json.contains("\"event.severity\":6"));
        assert!(json.contains("\"source.ip\":\"45.33.32.156\""));
        assert!(json.contains("\"url.original\""));
        let back: DetectionEvent = serde_json::from_str(&json).unwrap();
        assert_eq!(e, back);
    }
}
