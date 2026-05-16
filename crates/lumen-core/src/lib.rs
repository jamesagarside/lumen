//! Core domain types for Lumen.
//!
//! This crate holds the vocabulary used across the daemon and plugin SDK.
//! Types here are normalised representations — every ingestion path produces
//! these, regardless of source protocol.

#![forbid(unsafe_code)]

pub mod flow;
pub mod graph;

pub use flow::{Flow, FlowEndpoint, FlowSource, Protocol};
pub use graph::{is_internal_ip, Delta, Edge, EdgeId, Node, NodeId, Position, Snapshot};

pub const ABI_VERSION: &str = "0.1.0-pre";
