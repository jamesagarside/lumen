//! In-process fan-out of parsed `Flow` records.
//!
//! Listeners (one per protocol) push into the bus; consumers (WebSocket,
//! Live State Engine, rollups) subscribe. Slow consumers lag and miss
//! messages — that is intentional, the data plane should never block on a
//! slow client. Lagging is exposed via the metrics endpoint when #11
//! lands.

use std::sync::Arc;

use lumen_core::Flow;
use tokio::sync::broadcast;

const DEFAULT_CAPACITY: usize = 1024;

/// Cheap-to-clone handle to the in-process flow bus.
#[derive(Clone)]
pub struct FlowBus {
    inner: Arc<Inner>,
}

struct Inner {
    sender: broadcast::Sender<Flow>,
}

impl FlowBus {
    pub fn new() -> Self {
        Self::with_capacity(DEFAULT_CAPACITY)
    }

    pub fn with_capacity(capacity: usize) -> Self {
        let (sender, _) = broadcast::channel(capacity);
        Self {
            inner: Arc::new(Inner { sender }),
        }
    }

    /// Publish a flow. Returns the number of subscribers it was delivered
    /// to, which may be 0 if no one is listening.
    pub fn publish(&self, flow: Flow) -> usize {
        // `send` only errors when there are no subscribers — that's fine,
        // the data is allowed to be dropped on the floor.
        self.inner.sender.send(flow).unwrap_or(0)
    }

    pub fn subscribe(&self) -> broadcast::Receiver<Flow> {
        self.inner.sender.subscribe()
    }
}

impl Default for FlowBus {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lumen_core::{FlowEndpoint, FlowSource, Protocol};
    use std::net::{IpAddr, Ipv4Addr};
    use std::time::UNIX_EPOCH;

    fn flow() -> Flow {
        Flow {
            source: FlowSource::NetflowV5,
            src: FlowEndpoint {
                ip: IpAddr::V4(Ipv4Addr::LOCALHOST),
                port: 1,
            },
            dst: FlowEndpoint {
                ip: IpAddr::V4(Ipv4Addr::LOCALHOST),
                port: 2,
            },
            protocol: Protocol::TCP,
            bytes: 0,
            packets: 0,
            start: UNIX_EPOCH,
            end: UNIX_EPOCH,
        }
    }

    #[tokio::test]
    async fn subscribers_receive_published_flows() {
        let bus = FlowBus::new();
        let mut rx = bus.subscribe();
        bus.publish(flow());
        let received = rx.recv().await.unwrap();
        assert_eq!(received.src.port, 1);
    }

    #[tokio::test]
    async fn publish_with_no_subscribers_is_a_noop() {
        let bus = FlowBus::new();
        assert_eq!(bus.publish(flow()), 0);
    }
}
