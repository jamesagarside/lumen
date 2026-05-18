//! Shared test helper for env-var manipulation.
//!
//! Cargo runs tests multi-threaded, and several modules (settings, secret
//! store, the end-to-end main.rs tests) read process-global env. Without a
//! single shared lock, those tests can race each other: one sets `UDM_URL`
//! mid-test, a sibling reads it, assertions fail. Every test that touches
//! one of these env vars must take the same `ENV_LOCK` for its full body.

#![cfg(test)]

use std::sync::{Mutex, MutexGuard};

pub static ENV_LOCK: Mutex<()> = Mutex::new(());

/// Wipe env vars the integration tests would otherwise pick up AND take
/// the shared env lock. The returned guard must be bound (e.g.
/// `let _g = clear_env();`) so it lives to end of scope. Call this once
/// at the top of every test that touches one of these vars — do not call
/// it again inside the same test, that would deadlock the current thread.
pub fn clear_env() -> MutexGuard<'static, ()> {
    let guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    for k in [
        "UDM_URL",
        "UDM_API_KEY",
        "UDM_CONTROLLER_URL",
        "UDM_USERNAME",
        "UDM_PASSWORD",
        "UDM_SITE",
        "LUMEN_DETECTION_WEBHOOK_URL",
        "LUMEN_MASTER_KEY",
    ] {
        unsafe { std::env::remove_var(k) };
    }
    guard
}
