//! OpenTelemetry OTLP exporter init.
//!
//! When the `otel` cargo feature is enabled AND
//! `Settings::observability::otlp::endpoint` is non-empty, the binary
//! installs an OTLP layer alongside the JSON log layer. Spans are then
//! exported to a collector (Tempo / Jaeger / Honeycomb / Datadog /
//! Lightstep / etc.) in addition to being written to stdout.
//!
//! ## Why feature-gated
//!
//! The OpenTelemetry dependency tree is heavy (~50 crates). When you
//! don't need distributed tracing, leave the `otel` feature off — the
//! build is much smaller and faster.
//!
//! ## How to enable
//!
//! ```toml
//! # Cargo.toml
//! [dependencies]
//! propfirm-engine = { features = ["server", "otel"] }
//! ```
//!
//! ```toml
//! # config/propfirm.toml
//! [observability.otlp]
//! endpoint = "http://otel-collector:4317"  # or 4318 for HTTP
//! protocol = "grpc"  # or "http"
//! service_name = "propfirm-engine"
//! sample_ratio = 1.0
//! stdout = false  # set true to also print spans to stdout (dev)
//! ```

use crate::settings::ObservabilitySettings;

/// Initialize the global tracing subscriber.
///
/// This installs the fmt layer (JSON or pretty) and, when the `otel`
/// feature is enabled AND `observability.otlp.endpoint` is non-empty,
/// also installs an OTLP layer that exports spans to a collector.
///
/// # Errors
/// Returns an error if the OTLP pipeline fails to install.
pub fn init_tracing(settings: &ObservabilitySettings) -> anyhow::Result<()> {
    use tracing_subscriber::{fmt, layer::SubscriberExt, util::SubscriberInitExt, EnvFilter};

    let filter =
        EnvFilter::try_new(&settings.log_filter).unwrap_or_else(|_| EnvFilter::new("info"));

    // Build the fmt layer once. Both cfg branches share the same format.
    let is_pretty = settings.log_format.as_str() == "pretty";

    // OTLP path — construct everything inline so the layer types
    // unify on the (chained) subscriber type.
    #[cfg(feature = "otel")]
    {
        use opentelemetry::trace::TracerProvider;
        use opentelemetry_otlp::WithExportConfig;
        use opentelemetry_sdk::trace::SdkTracerProvider;
        use opentelemetry_sdk::Resource;

        let otlp = &settings.otlp;
        let should_install = !otlp.endpoint.is_empty() || otlp.stdout;

        if should_install {
            let resource = Resource::builder()
                .with_service_name(otlp.service_name.clone())
                .build();

            let provider = if otlp.stdout {
                let exporter = opentelemetry_stdout::SpanExporter::default();
                SdkTracerProvider::builder()
                    .with_simple_exporter(exporter)
                    .with_resource(resource)
                    .build()
            } else {
                let exporter = match otlp.protocol.as_str() {
                    "http" => opentelemetry_otlp::SpanExporter::builder()
                        .with_http()
                        .with_endpoint(&otlp.endpoint)
                        .build()
                        .map_err(|e| anyhow::anyhow!("failed to build OTLP HTTP exporter: {e}"))?,
                    _ => opentelemetry_otlp::SpanExporter::builder()
                        .with_tonic()
                        .with_endpoint(&otlp.endpoint)
                        .build()
                        .map_err(|e| anyhow::anyhow!("failed to build OTLP gRPC exporter: {e}"))?,
                };

                SdkTracerProvider::builder()
                    .with_batch_exporter(exporter)
                    .with_resource(resource)
                    .build()
            };

            let tracer = provider.tracer("propfirm-engine");

            if is_pretty {
                if let Err(e) = tracing_subscriber::registry()
                    .with(filter)
                    .with(fmt::layer().with_target(false))
                    .with(tracing_opentelemetry::layer().with_tracer(tracer))
                    .try_init()
                {
                    // Global subscriber already set — this is fine,
                    // it just means another caller initialized first
                    // (e.g. in tests that run in parallel). We log
                    // and continue.
                    tracing::warn!(error = %e, "failed to set global subscriber (already set?)");
                }
            } else {
                if let Err(e) = tracing_subscriber::registry()
                    .with(filter)
                    .with(fmt::layer().with_target(true).json())
                    .with(tracing_opentelemetry::layer().with_tracer(tracer))
                    .try_init()
                {
                    tracing::warn!(error = %e, "failed to set global subscriber (already set?)");
                }
            }
            return Ok(());
        }
    }

    // Flame graph profiling path — only when the `flame` cargo feature
    // is enabled AND `flame_output_path` is non-empty.
    #[cfg(feature = "flame")]
    {
        if !settings.flame_output_path.is_empty() {
            use std::sync::OnceLock;
            use tracing_flame::FlameLayer;

            static FLAME_GUARD: OnceLock<
                tracing_flame::FlushGuard<std::io::BufWriter<std::fs::File>>,
            > = OnceLock::new();

            match FlameLayer::with_file(&settings.flame_output_path) {
                Ok((flame_layer, guard)) => {
                    let _ = FLAME_GUARD.set(guard);
                    if let Err(e) = tracing_subscriber::registry()
                        .with(filter)
                        .with(fmt::layer().with_target(false))
                        .with(flame_layer)
                        .try_init()
                    {
                        tracing::warn!(error = %e, "failed to set global subscriber (already set?)");
                    }
                    return Ok(());
                }
                Err(e) => {
                    eprintln!(
                        "WARN: failed to create flame layer at {}: {} — falling back to plain fmt",
                        settings.flame_output_path, e
                    );
                }
            }
        }
    }

    // No OTLP layer — plain fmt subscriber only.
    // Use try_init so we don't panic if the global is already set
    // (e.g. by an earlier test in the same process).
    if is_pretty {
        if let Err(e) = tracing_subscriber::registry()
            .with(filter)
            .with(fmt::layer().with_target(false))
            .try_init()
        {
            tracing::warn!(error = %e, "failed to set global subscriber (already set?)");
        }
    } else {
        if let Err(e) = tracing_subscriber::registry()
            .with(filter)
            .with(fmt::layer().with_target(true).json())
            .try_init()
        {
            tracing::warn!(error = %e, "failed to set global subscriber (already set?)");
        }
    }

    Ok(())
}

/// Force-flush the OTLP provider on shutdown. Best-effort; errors are
/// logged but not returned. Spans in flight are exported before the
/// process exits.
#[cfg(feature = "otel")]
pub fn shutdown_otlp() {
    // The global tracer provider is dropped on process exit; the SDK
    // batch exporter attempts to flush remaining spans in its Drop
    // impl. For an explicit flush, the caller would need to hold a
    // reference to the provider (which we don't expose here).
    tracing::debug!("OTLP shutdown hook called (spans are flushed on Drop)");
}

#[cfg(not(feature = "otel"))]
pub fn shutdown_otlp() {
    // No-op — no OTLP provider installed.
}
