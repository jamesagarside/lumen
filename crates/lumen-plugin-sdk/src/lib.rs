//! Rust SDK for authoring Lumen plugins.
//!
//! The plugin ABI is being designed in issue #3 (HITL gate). This crate is
//! a placeholder so the workspace builds; real SDK surface will land once
//! the ABI is pinned.

#![forbid(unsafe_code)]

pub use lumen_core::ABI_VERSION;
