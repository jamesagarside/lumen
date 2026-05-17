//! Structured logging + (optional) OTLP trace export.
//!
//! Local-only by default: text logs to stdout in a TTY, JSON in a
//! container (or when `LUMEN_LOG_FORMAT=json` is set).
//!
//! When `OTEL_EXPORTER_OTLP_ENDPOINT` is set in the env, a
//! tracing-opentelemetry layer is added that exports spans to the
//! configured collector over OTLP/gRPC. The OpenTelemetry SDK reads
//! the standard OTEL_* env vars itself (endpoint, service name,
//! headers, etc.) so no extra config plumbing is needed here.
//!
//! Service name defaults to "lumen" but is overridable via
//! `OTEL_SERVICE_NAME`.

use std::io::IsTerminal;

use opentelemetry::trace::TracerProvider as _;
use opentelemetry::KeyValue;
use opentelemetry_otlp::WithExportConfig;
use opentelemetry_sdk::trace::{SdkTracer, SdkTracerProvider};
use opentelemetry_sdk::Resource;
use tracing_subscriber::fmt;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::EnvFilter;

const DEFAULT_SERVICE_NAME: &str = "lumen";

pub fn init() {
    let env_filter = EnvFilter::try_from_default_env()
        .or_else(|_| EnvFilter::try_new("info,lumen_daemon=info"))
        .expect("default tracing filter must parse");

    let json = json_logs_enabled();

    // Each layer is wrapped in an Option so we can compose them in
    // one .with() chain regardless of which subset is active —
    // tracing-subscriber's blanket `Option<L>: Layer<S>` impl turns
    // None into a zero-cost no-op.
    let json_fmt = json.then(|| fmt::layer().json().with_current_span(false));
    let compact_fmt = (!json).then(|| fmt::layer().compact());
    let otlp_layer = build_tracer().map(|t| tracing_opentelemetry::layer().with_tracer(t));

    tracing_subscriber::registry()
        .with(env_filter)
        .with(json_fmt)
        .with(compact_fmt)
        .with(otlp_layer)
        .init();
}

fn json_logs_enabled() -> bool {
    match std::env::var("LUMEN_LOG_FORMAT").ok().as_deref() {
        Some("json") => true,
        Some("text" | "compact") => false,
        _ => !std::io::stdout().is_terminal(),
    }
}

/// Build an OTLP-backed SdkTracer if the user has asked for one.
/// Returns None (and prints a startup line via eprintln, since
/// tracing isn't up yet) if no endpoint is configured or setup fails.
fn build_tracer() -> Option<SdkTracer> {
    let endpoint = std::env::var("OTEL_EXPORTER_OTLP_ENDPOINT").ok()?;
    let service_name =
        std::env::var("OTEL_SERVICE_NAME").unwrap_or_else(|_| DEFAULT_SERVICE_NAME.to_string());

    let exporter = opentelemetry_otlp::SpanExporter::builder()
        .with_tonic()
        .with_endpoint(&endpoint)
        .build()
        .map_err(|e| eprintln!("OTLP exporter init failed: {e}"))
        .ok()?;

    let provider = SdkTracerProvider::builder()
        .with_batch_exporter(exporter)
        .with_resource(
            Resource::builder()
                .with_attribute(KeyValue::new("service.name", service_name.clone()))
                .with_attribute(KeyValue::new("service.version", env!("CARGO_PKG_VERSION")))
                .build(),
        )
        .build();

    let tracer = provider.tracer(service_name);

    // Leak the provider so its batch exporter keeps running for the
    // lifetime of the daemon. Dropping SdkTracerProvider shuts the
    // exporter down, which we don't want until process exit.
    let _ = Box::leak(Box::new(provider));

    eprintln!(
        "otel: exporting traces to {endpoint} (set OTEL_SERVICE_NAME to override service name)"
    );

    Some(tracer)
}
