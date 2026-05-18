//! Direct integrations with third-party systems.
//!
//! Lives here as built-in modules until the WASM plugin runtime
//! (#16 in CONTEXT.md sequencing) lands. Once that's in place,
//! each integration can move to a `.wasm` shipped separately
//! without changing the engine's public API.

pub mod supervisor;
pub mod unifi;
pub mod unifi_ips;
pub mod webhook;
