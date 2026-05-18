//! Outbound webhook consumer for detection events.
//!
//! Subscribes to the in-process `DetectionBus` and POSTs every event
//! to a single configured URL as JSON. Designed for the common
//! homelab pattern of "ping me on Slack/Discord/n8n/Home Assistant
//! when something fires" without needing to wire up a separate
//! collector.
//!
//! Failure model: per-event, fire-and-forget with a short timeout.
//! Network blip → debug log, skip; HTTP non-2xx → warn log, skip.
//! Never blocks the bus.

use std::time::Duration;

use lumen_core::DetectionEvent;
use tracing::{debug, info, warn};

use crate::detections::DetectionBus;

const REQUEST_TIMEOUT: Duration = Duration::from_secs(5);

pub fn spawn(url: String, bus: DetectionBus) -> Option<tokio::task::AbortHandle> {
    let http = match reqwest::Client::builder()
        .timeout(REQUEST_TIMEOUT)
        .danger_accept_invalid_certs(true)
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            warn!(error = %e, "webhook consumer: HTTP client init failed; disabled");
            return None;
        }
    };

    let handle = tokio::spawn(async move {
        info!(url = %url, "webhook consumer: forwarding detection events");
        let mut rx = bus.subscribe();
        loop {
            match rx.recv().await {
                Ok(ev) => {
                    let url = url.clone();
                    let http = http.clone();
                    // One task per event so a slow webhook doesn't
                    // back-pressure the bus subscription. At ~2/sec
                    // peak this is cheap.
                    tokio::spawn(async move { forward(&http, &url, ev).await });
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                    warn!(missed = n, "webhook consumer: lagged on detection bus");
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                    debug!("webhook consumer: bus closed, exiting");
                    return;
                }
            }
        }
    });
    Some(handle.abort_handle())
}

async fn forward(http: &reqwest::Client, url: &str, event: DetectionEvent) {
    match http.post(url).json(&event).send().await {
        Ok(resp) if resp.status().is_success() => {
            debug!(status = %resp.status(), "webhook delivered");
        }
        Ok(resp) => {
            warn!(status = %resp.status(), url = %url, "webhook non-2xx response");
        }
        Err(e) => {
            debug!(error = %e, url = %url, "webhook delivery failed");
        }
    }
}
