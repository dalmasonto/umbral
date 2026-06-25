//! Observability init: structured logging + OpenTelemetry OTLP trace export.
//!
//! umbral apps previously hand-rolled the tracing subscriber in `main.rs`
//! (`tracing_subscriber::fmt()...init()`). This module gives the framework a
//! single ergonomic entry point that does the same fmt logging out of the box
//! and, when the `otel` feature is on and an OTLP endpoint is configured, also
//! exports a span-per-request to any OpenTelemetry collector (Jaeger, Tempo,
//! Honeycomb, …).
//!
//! ```ignore
//! use umbral_logs::observability::{init, ObservabilityConfig};
//!
//! #[tokio::main]
//! async fn main() {
//!     // Keep the guard alive for the whole program: on drop it flushes and
//!     // shuts down the OTLP exporter so in-flight spans aren't lost at exit.
//!     let _obs = init(ObservabilityConfig::from_env());
//!     // ... build and serve the app ...
//! }
//! ```
//!
//! ## Env knobs
//!
//! - `RUST_LOG` — the `EnvFilter` directive. Default `info`.
//! - `UMBRAL_LOG_FORMAT=json` — emit structured JSON log lines (with `level`,
//!   `target`, fields, and — under the `otel` feature — `trace_id`/`span_id`).
//! - `OTEL_EXPORTER_OTLP_ENDPOINT` — OTLP gRPC endpoint. Default
//!   `http://localhost:4317`. (`otel` feature only.)
//! - `OTEL_SERVICE_NAME` — the `service.name` resource attribute. (`otel`
//!   feature only.)
//!
//! ## Without the `otel` feature
//!
//! The init helper still configures fmt/JSON logging from the same config;
//! it simply never builds the OpenTelemetry layer. A base build pulls none of
//! the otel/tonic dependencies.

use std::env;

/// Configuration for [`init`]. Construct with [`ObservabilityConfig::from_env`]
/// (reads `RUST_LOG`, `UMBRAL_LOG_FORMAT`, `OTEL_EXPORTER_OTLP_ENDPOINT`,
/// `OTEL_SERVICE_NAME`) or build one explicitly via the setters.
#[derive(Debug, Clone, Default)]
pub struct ObservabilityConfig {
    /// `service.name` reported to the OTLP collector. `None` falls back to
    /// `OTEL_SERVICE_NAME`, then to `"umbral"`.
    pub service_name: Option<String>,
    /// Emit JSON-structured log lines instead of the human-readable format.
    pub json: bool,
    /// OTLP gRPC endpoint. `None` falls back to `OTEL_EXPORTER_OTLP_ENDPOINT`,
    /// then to `http://localhost:4317`. Only consulted under the `otel`
    /// feature; without it, span export is a no-op regardless.
    pub otlp_endpoint: Option<String>,
}

impl ObservabilityConfig {
    /// Build a config from the environment:
    /// - `json` is `true` when `UMBRAL_LOG_FORMAT=json` (case-insensitive).
    /// - `service_name` from `OTEL_SERVICE_NAME` if set.
    /// - `otlp_endpoint` from `OTEL_EXPORTER_OTLP_ENDPOINT` if set.
    ///
    /// `RUST_LOG` is read inside [`init`] by the `EnvFilter` directly, so it is
    /// not stored on the config.
    pub fn from_env() -> Self {
        let json = env::var("UMBRAL_LOG_FORMAT")
            .map(|v| v.eq_ignore_ascii_case("json"))
            .unwrap_or(false);
        Self {
            service_name: env::var("OTEL_SERVICE_NAME").ok().filter(|s| !s.is_empty()),
            json,
            otlp_endpoint: env::var("OTEL_EXPORTER_OTLP_ENDPOINT").ok().filter(|s| !s.is_empty()),
        }
    }

    /// Set the `service.name` resource attribute.
    pub fn service_name(mut self, name: impl Into<String>) -> Self {
        self.service_name = Some(name.into());
        self
    }

    /// Emit JSON-structured logs.
    pub fn json(mut self, json: bool) -> Self {
        self.json = json;
        self
    }

    /// Set the OTLP gRPC endpoint (e.g. `http://localhost:4317`).
    pub fn otlp_endpoint(mut self, endpoint: impl Into<String>) -> Self {
        self.otlp_endpoint = Some(endpoint.into());
        self
    }

    /// The effective service name: explicit value, else `OTEL_SERVICE_NAME` (via
    /// `from_env`), else `"umbral"`. Only used by the OTLP exporter, so gated to
    /// the `otel` feature (dead code otherwise).
    #[cfg(feature = "otel")]
    fn effective_service_name(&self) -> String {
        self.service_name.clone().unwrap_or_else(|| "umbral".to_string())
    }

    /// The effective OTLP endpoint: explicit value, else the
    /// `OTEL_EXPORTER_OTLP_ENDPOINT` default `http://localhost:4317`.
    #[cfg(feature = "otel")]
    fn effective_endpoint(&self) -> String {
        self.otlp_endpoint.clone().unwrap_or_else(|| "http://localhost:4317".to_string())
    }
}

/// Returned by [`init`]; keep it alive for the lifetime of the program. On
/// drop it flushes and shuts down the OTLP exporter so spans buffered by the
/// batch processor aren't dropped at exit. Without the `otel` feature (or when
/// init was a no-op because the subscriber was already set) the guard does
/// nothing on drop.
#[must_use = "the ObservabilityGuard flushes the OTLP exporter on drop; bind it to a variable kept alive for the program (e.g. `let _obs = init(...)`)"]
pub struct ObservabilityGuard {
    #[cfg(feature = "otel")]
    provider: Option<opentelemetry_sdk::trace::SdkTracerProvider>,
}

impl Drop for ObservabilityGuard {
    fn drop(&mut self) {
        #[cfg(feature = "otel")]
        if let Some(provider) = self.provider.take() {
            // Flush buffered spans, then shut the provider down. Both are
            // best-effort at process exit: log the failure but never panic in
            // a Drop impl.
            if let Err(e) = provider.force_flush() {
                tracing::warn!(error = ?e, "observability: OTLP force_flush failed on shutdown");
            }
            if let Err(e) = provider.shutdown() {
                tracing::warn!(error = ?e, "observability: OTLP provider shutdown failed");
            }
        }
    }
}

/// Process-once guard. `init` is set-once: the global tracing subscriber can
/// only be installed once per process, so a second call logs a warning and
/// returns a no-op guard rather than panicking.
static INITIALISED: std::sync::OnceLock<()> = std::sync::OnceLock::new();

/// Initialise observability: install the global tracing subscriber (fmt or
/// JSON per `config.json`) and, under the `otel` feature, an OpenTelemetry
/// layer that exports spans to the configured OTLP endpoint.
///
/// Idempotent: only the first call installs the subscriber. A later call logs
/// at `warn` and returns a no-op [`ObservabilityGuard`] (never panics, never
/// double-installs).
///
/// Returns an [`ObservabilityGuard`] that flushes + shuts down the exporter on
/// drop — bind it for the lifetime of the program.
pub fn init(config: ObservabilityConfig) -> ObservabilityGuard {
    // Set-once. If we lose the race / are called twice, do nothing.
    if INITIALISED.set(()).is_err() {
        // A subscriber is already installed. `tracing::warn!` here is a no-op
        // if no subscriber is set, but in practice the first init set one.
        tracing::warn!("observability: init() called more than once; ignoring (subscriber already installed)");
        return noop_guard();
    }

    init_inner(config)
}

fn init_inner(config: ObservabilityConfig) -> ObservabilityGuard {
    use tracing_subscriber::layer::SubscriberExt;
    use tracing_subscriber::util::SubscriberInitExt;
    use tracing_subscriber::{EnvFilter, Layer};

    let env_filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));

    // fmt layer: JSON when requested, human-readable otherwise. Boxed so both
    // arms have the same type. The JSON arm includes the current span
    // (so `trace_id`/`span_id` from the OTel layer ride along under `otel`).
    let fmt_layer = if config.json {
        tracing_subscriber::fmt::layer()
            .json()
            .with_current_span(true)
            .with_span_list(false)
            .boxed()
    } else {
        tracing_subscriber::fmt::layer().boxed()
    };

    let registry = tracing_subscriber::registry().with(env_filter).with(fmt_layer);

    // Under the `otel` feature, try to build the OTLP exporter and add its
    // layer. A failure (only a malformed endpoint fails at build time — the
    // collector being unreachable is fine, the exporter connects lazily)
    // degrades to fmt-only logging rather than bringing the app down. Without
    // the feature, there is no provider and the registry is logging-only.
    #[cfg(feature = "otel")]
    {
        match build_otel(&config) {
            Ok((layer, provider)) => {
                registry.with(layer).init();
                ObservabilityGuard { provider: Some(provider) }
            }
            Err(e) => {
                eprintln!(
                    "observability: OTLP exporter setup failed, continuing with logs only: {e}"
                );
                registry.init();
                ObservabilityGuard { provider: None }
            }
        }
    }

    #[cfg(not(feature = "otel"))]
    {
        let _ = &config;
        registry.init();
        noop_guard()
    }
}

/// Build the OTLP exporter, tracer provider, and the `tracing-opentelemetry`
/// bridge layer. Split out so [`init_inner`] stays readable.
#[cfg(feature = "otel")]
#[allow(clippy::type_complexity)]
fn build_otel<S>(
    config: &ObservabilityConfig,
) -> Result<
    (
        tracing_opentelemetry::OpenTelemetryLayer<S, opentelemetry_sdk::trace::SdkTracer>,
        opentelemetry_sdk::trace::SdkTracerProvider,
    ),
    Box<dyn std::error::Error + Send + Sync>,
>
where
    S: tracing::Subscriber + for<'a> tracing_subscriber::registry::LookupSpan<'a>,
{
    use opentelemetry::trace::TracerProvider as _;
    use opentelemetry_otlp::WithExportConfig as _;

    let endpoint = config.effective_endpoint();
    let service_name = config.effective_service_name();

    let exporter = opentelemetry_otlp::SpanExporter::builder()
        .with_tonic()
        .with_endpoint(endpoint)
        .build()?;

    let resource = opentelemetry_sdk::Resource::builder()
        .with_service_name(service_name)
        .build();

    let provider = opentelemetry_sdk::trace::SdkTracerProvider::builder()
        .with_batch_exporter(exporter)
        .with_resource(resource)
        .build();

    let tracer = provider.tracer("umbral");
    let layer = tracing_opentelemetry::layer().with_tracer(tracer);

    Ok((layer, provider))
}

/// A guard that does nothing on drop. Used when init is a no-op (second call,
/// or the base build without the `otel` feature).
fn noop_guard() -> ObservabilityGuard {
    ObservabilityGuard {
        #[cfg(feature = "otel")]
        provider: None,
    }
}

#[cfg(all(test, feature = "otel"))]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};
    use tracing_subscriber::layer::SubscriberExt;
    use tracing_subscriber::util::SubscriberInitExt;
    use tracing_subscriber::Layer;

    /// A `MakeWriter` that captures everything written into a shared buffer so
    /// the JSON-logging test can read back the emitted line.
    #[derive(Clone)]
    struct BufWriter(Arc<Mutex<Vec<u8>>>);

    impl std::io::Write for BufWriter {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for BufWriter {
        type Writer = BufWriter;
        fn make_writer(&'a self) -> Self::Writer {
            self.clone()
        }
    }

    // JSON logging: a captured log line is valid JSON carrying the level,
    // target, and message. Uses a scoped subscriber (`with_default`) so it does
    // not touch the process-global subscriber the idempotency test exercises.
    #[test]
    fn json_layer_emits_valid_json() {
        let buf = Arc::new(Mutex::new(Vec::new()));
        let writer = BufWriter(buf.clone());

        let json_layer = tracing_subscriber::fmt::layer()
            .json()
            .with_writer(writer)
            .with_current_span(false)
            .with_span_list(false)
            .boxed();

        let subscriber = tracing_subscriber::registry().with(json_layer);

        tracing::subscriber::with_default(subscriber, || {
            tracing::info!(target: "umbral_logs::observability::tests", answer = 42, "hello json");
        });

        let raw = String::from_utf8(buf.lock().unwrap().clone()).unwrap();
        let line = raw.lines().next().expect("a log line was emitted");
        let parsed: serde_json::Value =
            serde_json::from_str(line).expect("log line is valid JSON");

        assert_eq!(parsed["level"], "INFO");
        assert_eq!(parsed["target"], "umbral_logs::observability::tests");
        assert_eq!(parsed["fields"]["message"], "hello json");
        assert_eq!(parsed["fields"]["answer"], 42);
    }

    // Span export via the in-memory OTLP exporter — proves the tracing -> OTel
    // bridge works with zero network. Emit a span under a scoped subscriber
    // carrying the OTel layer, force-flush, and assert the span surfaced with
    // the expected name + attribute.
    #[test]
    fn spans_export_to_in_memory_exporter() {
        use opentelemetry::trace::TracerProvider as _;
        use opentelemetry_sdk::trace::in_memory_exporter::InMemorySpanExporter;

        let exporter = InMemorySpanExporter::default();
        let provider = opentelemetry_sdk::trace::SdkTracerProvider::builder()
            .with_simple_exporter(exporter.clone())
            .build();
        let tracer = provider.tracer("umbral-test");
        let otel_layer = tracing_opentelemetry::layer().with_tracer(tracer);

        let subscriber = tracing_subscriber::registry().with(otel_layer);

        tracing::subscriber::with_default(subscriber, || {
            let span = tracing::info_span!("http.request", http.method = "GET", http.route = "/widgets");
            let _e = span.enter();
        });

        provider.force_flush().expect("force_flush");

        let spans = exporter.get_finished_spans().expect("finished spans");
        assert_eq!(spans.len(), 1, "exactly one span exported");
        let span = &spans[0];
        assert_eq!(span.name, "http.request");
        let method = span
            .attributes
            .iter()
            .find(|kv| kv.key.as_str() == "http.method")
            .expect("http.method attribute present");
        assert_eq!(method.value.as_str(), "GET");
    }

    // Idempotent init: calling `init` twice does not panic; the second is a
    // no-op. This DOES install the process-global subscriber, so it is the
    // single test allowed to do so. Runs inside a tokio runtime because the
    // tonic OTLP exporter the first `init` builds requires one (same as the
    // `#[tokio::main]` context production calls `init` from).
    #[tokio::test]
    async fn init_is_idempotent() {
        let _g1 = init(ObservabilityConfig::default());
        // Second call must not panic and must return a (no-op) guard.
        let _g2 = init(ObservabilityConfig::default());
    }
}
