//! `propfirm-server` binary entry point.
//!
//! Initializes:
//! 1. `.env` loading (handled by `Settings::load`)
//! 2. `tracing_subscriber` with `EnvFilter` (JSON or pretty format) +
//!    optional OTLP exporter when `otel` feature is enabled AND
//!    `observability.otlp.endpoint` is set
//! 3. Prometheus metrics recorder (idempotent; the actual install
//!    happens in `propfirm::api::server::default_metrics_handle()`)
//! 4. Panic hook (routes panics through `tracing::error`)
//! 5. Settings load + Postgres migrations (if enabled)
//! 6. Optional TLS (rustls, in-process)
//! 7. Graceful shutdown (SIGINT/SIGTERM, drain in-flight requests,
//!    flush OTLP provider)

use propfirm::api::middleware::install_panic_hook;
use propfirm::api::otel;
use propfirm::api::server::{default_metrics_handle, run_server};
use propfirm::settings::Settings;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // 1. Load settings (also loads .env if present).
    let settings = Settings::load().map_err(|e| {
        eprintln!("FATAL: failed to load settings: {e}");
        e
    })?;

    // 2. Initialize tracing subscriber — fmt layer + optional OTLP layer.
    otel::init_tracing(&settings.observability)?;

    tracing::info!(
        bind_addr = %settings.server.bind_addr,
        tls_enabled = %settings.server.tls.enabled,
        metrics_enabled = %settings.observability.metrics_enabled,
        otlp_enabled = !settings.observability.otlp.endpoint.is_empty(),
        "propfirm-server starting (D81: stateless compute service, no DB)"
    );

    // 3. Install panic hook.
    if settings.observability.panic_hook {
        install_panic_hook();
        tracing::info!("panic hook installed");
    }

    // 4. Touch the metrics handle so the recorder is installed up-front
    //    (cosmetic — `run_server` also calls this idempotently).
    if settings.observability.metrics_enabled {
        let _ = default_metrics_handle();
        tracing::info!(
            metrics_path = %settings.observability.metrics_path,
            "Prometheus metrics recorder installed"
        );
    }

    // 5. Run the server (handles TLS, graceful shutdown, state building).
    let result = run_server(settings).await;

    // 6. Flush the OTLP provider so spans in flight are exported before
    //    process exit (best-effort).
    otel::shutdown_otlp();

    result
}
