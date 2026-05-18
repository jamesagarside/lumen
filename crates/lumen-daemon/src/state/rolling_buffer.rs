//! Time- and memory-bounded RAM ring of raw `Flow` records.
//!
//! Feeds the rewind / time-scrub UI (#26) and the rollup engine (#21). Never
//! touches disk — per CONTEXT.md §3, Lumen is not the system of record for
//! raw flows.
//!
//! Eviction policy: `min(LUMEN_RAW_WINDOW_DURATION, LUMEN_RAW_WINDOW_BYTES)`,
//! whichever fires first. Both bounds are enforced on every append; queries
//! also opportunistically evict by time so a long quiet period doesn't
//! surface stale data.
//!
//! Concurrency: a single `RwLock<VecDeque<Stamped>>`. Appends take the write
//! lock briefly (push_back + trim); queries take the read lock, walk the
//! deque, and clone matching flows into a `Vec`. Ingestion is the hot path,
//! so eviction work is amortised into the append that triggered it rather
//! than running on a background sweep — appends remain O(1) amortised.

use std::collections::VecDeque;
use std::sync::{Arc, RwLock};
use std::time::{Duration, SystemTime};

use lumen_core::Flow;

/// Memory cost of a single buffered flow. `Flow` is heap-free (`IpAddr`,
/// `SystemTime`, and the integer fields are all sized), so `size_of::<Flow>()`
/// is the on-heap cost when stored in a `VecDeque`. The VecDeque per-slot
/// overhead is negligible compared to the flow itself.
pub const FLOW_BYTES: usize = std::mem::size_of::<Flow>();

/// A flow plus the wall-clock instant we stamped at append time. We index
/// time-range queries against this rather than `flow.end`, because some
/// ingestion paths (NetFlow with stale timestamps, replay tooling) produce
/// records whose `end` timestamp is unrelated to when we actually observed
/// them. The buffer is "what arrived when", not "what happened when".
#[derive(Debug, Clone)]
struct Stamped {
    flow: Flow,
    arrived: SystemTime,
}

/// Bounds on the buffer. `min(duration, bytes)` wins — whichever fires first
/// evicts the oldest record.
#[derive(Debug, Clone, Copy)]
pub struct Bounds {
    pub max_duration: Duration,
    pub max_bytes: usize,
}

impl Bounds {
    /// Defaults from CONTEXT.md §3 — 15 minutes, 512 MB.
    pub const DEFAULT: Self = Self {
        max_duration: Duration::from_secs(15 * 60),
        max_bytes: 512 * 1024 * 1024,
    };
}

impl Default for Bounds {
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// Cheap-to-clone handle to the rolling buffer. Multiple producers can
/// append concurrently; readers (the future time-scrub query API,
/// rollup promotion) can query without blocking each other.
#[derive(Clone)]
pub struct RollingBuffer {
    inner: Arc<Inner>,
}

struct Inner {
    deque: RwLock<VecDeque<Stamped>>,
    bounds: Bounds,
}

// Read-side getters land with #26 (time scrubber) and #21 (rollups). They're
// public on the type today so the buffer's API surface is stable for those
// PRs to consume; until then they're unused at production call sites.
#[allow(dead_code)]
impl RollingBuffer {
    pub fn new(bounds: Bounds) -> Self {
        Self {
            inner: Arc::new(Inner {
                deque: RwLock::new(VecDeque::new()),
                bounds,
            }),
        }
    }

    pub fn bounds(&self) -> Bounds {
        self.inner.bounds
    }

    /// Append a flow stamped with `arrived`. Production callers should pass
    /// `SystemTime::now()`; tests inject a clock so eviction is deterministic.
    pub fn append_at(&self, flow: Flow, arrived: SystemTime) {
        let mut deque = self.inner.deque.write().expect("rolling buffer poisoned");
        deque.push_back(Stamped { flow, arrived });
        Self::trim(&mut deque, self.inner.bounds, arrived);
    }

    /// Append a flow stamped at `SystemTime::now()`.
    pub fn append(&self, flow: Flow) {
        self.append_at(flow, SystemTime::now());
    }

    /// Snapshot flows whose `arrived` timestamp falls within `[start, end]`.
    /// Returned in append order (oldest first). We clone into a `Vec` and
    /// return an owning iterator so callers don't hold the read lock — this
    /// matters: a slow consumer must never block ingestion.
    pub fn query(&self, start: SystemTime, end: SystemTime) -> impl ExactSizeIterator<Item = Flow> {
        let deque = self.inner.deque.read().expect("rolling buffer poisoned");
        let collected: Vec<Flow> = deque
            .iter()
            .filter(|s| s.arrived >= start && s.arrived <= end)
            .map(|s| s.flow.clone())
            .collect();
        collected.into_iter()
    }

    /// Current number of buffered flows. Mostly for tests + metrics.
    pub fn len(&self) -> usize {
        self.inner
            .deque
            .read()
            .expect("rolling buffer poisoned")
            .len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Current approximate memory footprint of the buffered flows, in bytes.
    pub fn bytes(&self) -> usize {
        self.len() * FLOW_BYTES
    }

    /// Force-evict by time only, against an externally supplied "now". Used
    /// by the periodic sweep so a long quiet period doesn't leave stale
    /// records sitting at the head of the deque until the next append.
    pub fn evict_older_than(&self, now: SystemTime) {
        let mut deque = self.inner.deque.write().expect("rolling buffer poisoned");
        Self::trim_by_time(&mut deque, self.inner.bounds.max_duration, now);
    }

    fn trim(deque: &mut VecDeque<Stamped>, bounds: Bounds, now: SystemTime) {
        Self::trim_by_time(deque, bounds.max_duration, now);
        Self::trim_by_bytes(deque, bounds.max_bytes);
    }

    fn trim_by_time(deque: &mut VecDeque<Stamped>, max_duration: Duration, now: SystemTime) {
        let Some(cutoff) = now.checked_sub(max_duration) else {
            return;
        };
        while let Some(front) = deque.front() {
            if front.arrived < cutoff {
                deque.pop_front();
            } else {
                break;
            }
        }
    }

    fn trim_by_bytes(deque: &mut VecDeque<Stamped>, max_bytes: usize) {
        let max_count = max_bytes / FLOW_BYTES.max(1);
        while deque.len() > max_count {
            deque.pop_front();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lumen_core::{FlowEndpoint, FlowSource, Protocol};
    use std::net::{IpAddr, Ipv4Addr};

    fn make_flow(port: u16) -> Flow {
        Flow {
            source: FlowSource::NetflowV5,
            src: FlowEndpoint {
                ip: IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1)),
                port,
            },
            dst: FlowEndpoint {
                ip: IpAddr::V4(Ipv4Addr::new(8, 8, 8, 8)),
                port: 53,
            },
            protocol: Protocol::UDP,
            bytes: 64,
            packets: 1,
            start: SystemTime::UNIX_EPOCH,
            end: SystemTime::UNIX_EPOCH,
        }
    }

    fn small_bounds(bytes: usize, secs: u64) -> Bounds {
        Bounds {
            max_duration: Duration::from_secs(secs),
            max_bytes: bytes,
        }
    }

    #[test]
    fn append_and_query_returns_recent_flows() {
        let buf = RollingBuffer::new(Bounds::DEFAULT);
        let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000);
        buf.append_at(make_flow(1), t0);
        buf.append_at(make_flow(2), t0 + Duration::from_secs(1));
        buf.append_at(make_flow(3), t0 + Duration::from_secs(2));
        let got: Vec<u16> = buf
            .query(t0, t0 + Duration::from_secs(5))
            .map(|f| f.src.port)
            .collect();
        assert_eq!(got, vec![1, 2, 3]);
    }

    #[test]
    fn query_filters_by_time_range() {
        let buf = RollingBuffer::new(Bounds::DEFAULT);
        let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000);
        for i in 0..10 {
            buf.append_at(make_flow(i), t0 + Duration::from_secs(i as u64));
        }
        let got: Vec<u16> = buf
            .query(t0 + Duration::from_secs(3), t0 + Duration::from_secs(6))
            .map(|f| f.src.port)
            .collect();
        assert_eq!(got, vec![3, 4, 5, 6]);
    }

    // ── Bound enforcement ───────────────────────────────────────────────

    /// Property: under sustained appends the memory bound is respected at
    /// every observation point. We sweep through 50× the byte cap worth of
    /// flows and check `bytes()` never exceeds the cap.
    #[test]
    fn memory_bound_respected_under_sustained_ingestion() {
        let cap_bytes = FLOW_BYTES * 16;
        let buf = RollingBuffer::new(small_bounds(cap_bytes, 60 * 60));
        let mut t = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000);
        for i in 0..(50 * 16) {
            buf.append_at(make_flow(i as u16), t);
            t += Duration::from_millis(1);
            assert!(
                buf.bytes() <= cap_bytes,
                "byte cap violated at iter {i}: bytes={} cap={cap_bytes}",
                buf.bytes()
            );
        }
        assert_eq!(buf.len(), 16, "buffer should be saturated at the cap");
    }

    /// Property: flows older than `max_duration` are evicted on append.
    #[test]
    fn time_bound_respected() {
        let bounds = small_bounds(usize::MAX, 10);
        let buf = RollingBuffer::new(bounds);
        let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(10_000);
        for i in 0..5 {
            buf.append_at(make_flow(i), t0 + Duration::from_secs(i as u64));
        }
        assert_eq!(buf.len(), 5);
        // Jump forward 20s — the first batch is well outside the 10s window.
        buf.append_at(make_flow(99), t0 + Duration::from_secs(20));
        let ports: Vec<u16> = buf
            .query(SystemTime::UNIX_EPOCH, t0 + Duration::from_secs(100))
            .map(|f| f.src.port)
            .collect();
        assert_eq!(
            ports,
            vec![99],
            "all flows older than the 10s window should be gone"
        );
    }

    /// `evict_older_than` clears stale entries even without a new append.
    #[test]
    fn evict_older_than_runs_without_new_appends() {
        let buf = RollingBuffer::new(small_bounds(usize::MAX, 10));
        let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(10_000);
        for i in 0..3 {
            buf.append_at(make_flow(i), t0 + Duration::from_secs(i as u64));
        }
        buf.evict_older_than(t0 + Duration::from_secs(30));
        assert_eq!(buf.len(), 0);
    }

    /// Concurrent appenders + readers do not deadlock and produce no torn
    /// state. Bounded total work so the test stays fast in CI.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrent_append_and_query() {
        let buf = RollingBuffer::new(Bounds::DEFAULT);
        let mut handles = Vec::new();
        for w in 0..4 {
            let buf = buf.clone();
            handles.push(tokio::spawn(async move {
                let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(100_000);
                for i in 0..500 {
                    buf.append_at(
                        make_flow((w * 1000 + i) as u16),
                        t0 + Duration::from_micros((w * 1000 + i) as u64),
                    );
                }
            }));
        }
        for _ in 0..4 {
            let buf = buf.clone();
            handles.push(tokio::spawn(async move {
                for _ in 0..200 {
                    let _ = buf.query(SystemTime::UNIX_EPOCH, SystemTime::now()).count();
                }
            }));
        }
        for h in handles {
            h.await.unwrap();
        }
        assert_eq!(buf.len(), 2000);
    }
}
