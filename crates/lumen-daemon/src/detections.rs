//! Detection event bus + ring buffer.
//!
//! Detection events are inherently more interesting than raw flows
//! (so we want them all) but less voluminous (a typical home network
//! sees dozens-to-hundreds per day, not millions per minute). So
//! we keep a bounded in-memory ring of the most recent ones, push
//! them onto a tokio broadcast channel for live subscribers, and
//! never persist beyond process lifetime.
//!
//! Why no persistence: per CONTEXT.md §6 the source-of-truth is the
//! producer (UniFi UI, Suricata logs, …); lumen surfaces them and
//! deep-links back. If you want long-term retention, route the
//! producer to a SIEM in parallel.

use std::collections::VecDeque;
use std::sync::{Arc, RwLock};

use lumen_core::DetectionEvent;
use tokio::sync::broadcast;

const RING_CAPACITY: usize = 500;
const BROADCAST_CAPACITY: usize = 64;

/// Cheap-to-clone handle. Shares a single ring buffer + broadcast
/// channel across the engine, HTTP handlers, integrations, and the
/// UI WebSocket path.
#[derive(Clone)]
pub struct DetectionBus {
    inner: Arc<Inner>,
}

struct Inner {
    ring: RwLock<VecDeque<DetectionEvent>>,
    tx: broadcast::Sender<DetectionEvent>,
}

impl DetectionBus {
    pub fn new() -> Self {
        let (tx, _) = broadcast::channel(BROADCAST_CAPACITY);
        Self {
            inner: Arc::new(Inner {
                ring: RwLock::new(VecDeque::with_capacity(RING_CAPACITY)),
                tx,
            }),
        }
    }

    /// Publish an event. Lossy on the broadcast side (slow subscribers
    /// just see Lagged on next recv); always succeeds for the ring.
    pub fn publish(&self, event: DetectionEvent) {
        {
            let mut ring = self.inner.ring.write().expect("ring poisoned");
            if ring.len() == RING_CAPACITY {
                ring.pop_front();
            }
            ring.push_back(event.clone());
        }
        let _ = self.inner.tx.send(event);
    }

    /// Snapshot of recent events, newest first.
    pub fn recent(&self, max: usize) -> Vec<DetectionEvent> {
        let ring = self.inner.ring.read().expect("ring poisoned");
        ring.iter().rev().take(max).cloned().collect()
    }

    /// Live stream for the WebSocket push path (once wired). v1 UI
    /// just polls `/events`; this is here for the imminent upgrade.
    #[allow(dead_code)]
    pub fn subscribe(&self) -> broadcast::Receiver<DetectionEvent> {
        self.inner.tx.subscribe()
    }

    #[allow(dead_code)] // used by tests + future /metrics gauge
    pub fn len(&self) -> usize {
        self.inner.ring.read().expect("ring poisoned").len()
    }
}

impl Default for DetectionBus {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lumen_core::{Agent, EventKind, Rule, Severity};
    use std::time::SystemTime;

    fn ev(message: &str, severity: u8) -> DetectionEvent {
        DetectionEvent {
            timestamp: SystemTime::UNIX_EPOCH,
            kind: EventKind::Alert,
            category: vec![],
            severity: Severity::clamped(severity),
            action: None,
            message: message.to_string(),
            rule: Rule::default(),
            agent: Agent {
                type_: "test".to_string(),
                vendor: None,
                version: None,
            },
            source_ip: None,
            destination_ip: None,
            url_original: None,
            extra: serde_json::Value::Null,
        }
    }

    #[test]
    fn recent_is_newest_first() {
        let bus = DetectionBus::new();
        bus.publish(ev("first", 3));
        bus.publish(ev("second", 5));
        bus.publish(ev("third", 7));
        let r = bus.recent(10);
        assert_eq!(r.len(), 3);
        assert_eq!(r[0].message, "third");
        assert_eq!(r[1].message, "second");
        assert_eq!(r[2].message, "first");
    }

    #[test]
    fn ring_evicts_oldest_when_full() {
        let bus = DetectionBus::new();
        for i in 0..(RING_CAPACITY + 10) {
            bus.publish(ev(&format!("{i}"), 3));
        }
        assert_eq!(bus.len(), RING_CAPACITY);
        let r = bus.recent(1);
        // Newest must be the very last one we pushed.
        assert_eq!(r[0].message, format!("{}", RING_CAPACITY + 10 - 1));
    }

    #[test]
    fn recent_caps_at_max() {
        let bus = DetectionBus::new();
        for i in 0..10 {
            bus.publish(ev(&format!("{i}"), 3));
        }
        assert_eq!(bus.recent(3).len(), 3);
    }

    #[tokio::test]
    async fn subscribers_receive_published_events() {
        let bus = DetectionBus::new();
        let mut rx = bus.subscribe();
        bus.publish(ev("hello", 5));
        let got = rx.recv().await.unwrap();
        assert_eq!(got.message, "hello");
    }
}
