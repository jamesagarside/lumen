//! Flow ingestion: protocol parsers + listeners that feed the rest of the
//! pipeline.
//!
//! Each protocol module produces normalised [`lumen_core::Flow`] records.
//! The daemon's main composition layer wires listeners up to the broadcast
//! channel that downstream consumers (WebSocket, Live State Engine,
//! rollups) subscribe to.

pub mod broadcast;
pub mod netflow_v5;
pub mod udp_listener;

pub use broadcast::FlowBus;
