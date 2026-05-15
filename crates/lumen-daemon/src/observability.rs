use std::io::IsTerminal;

use tracing_subscriber::fmt;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::EnvFilter;

pub fn init() {
    let env_filter = EnvFilter::try_from_default_env()
        .or_else(|_| EnvFilter::try_new("info,lumen_daemon=info"))
        .expect("default tracing filter must parse");

    let registry = tracing_subscriber::registry().with(env_filter);

    if json_logs_enabled() {
        registry
            .with(fmt::layer().json().with_current_span(false))
            .init();
    } else {
        registry.with(fmt::layer().compact()).init();
    }
}

fn json_logs_enabled() -> bool {
    match std::env::var("LUMEN_LOG_FORMAT").ok().as_deref() {
        Some("json") => true,
        Some("text" | "compact") => false,
        _ => !std::io::stdout().is_terminal(),
    }
}
