//! `propfirm-server` binary entry point.
//!
//! Initializes:
//! 1. `.env` loading (handled by `Settings::load`)
//! 2. `tracing_subscriber` with `EnvFilter` (JSON or pretty format)
//! 3. Prometheus metrics recorder (idempotent; the actual install
//!    happens in `propfirm::api::server::default_metrics_handle()`)
//! 4. Panic hook (routes panics through `tracing::error`)
//! 5. Settings load + Postgres migrations (if enabled)
//! 6. Optional TLS (rustls, in-process)
//! 7. Graceful shutdown (SIGINT/SIGTERM, drain in-flight requests)

use propfirm::api::middleware::install_panic_hook;
use propfirm::api::server::{default_metrics_handle, run_server};
use propfirm::settings::Settings;
use tracing_subscriber::{fmt, EnvFilter};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // 1. Load settings (also loads .env if present).
    let settings = Settings::load().map_err(|e| {
        eprintln!("FATAL: failed to load settings: {e}");
        e
    })?;

    // 2. Initialize tracing subscriber.
    init_tracing(&settings);

    tracing::info!(
        bind_addr = %settings.server.bind_addr,
        tls_enabled = %settings.server.tls.enabled,
        idempotency_backend = %settings.idempotency.backend,
        metrics_enabled = %settings.observability.metrics_enabled,
        "propfirm-server starting"
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
    run_server(settings).await?;

    Ok(())
}

/// Initialize the `tracing_subscriber` global default.
fn init_tracing(settings: &Settings) {
    let filter = EnvFilter::try_new(&settings.observability.log_filter)
        .unwrap_or_else(|_| EnvFilter::new("info"));

    match settings.observability.log_format.as_str() {
        "pretty" => {
            fmt().with_env_filter(filter).with_target(false).init();
        }
        // default to json for production
        _ => {
            fmt()
                .with_env_filter(filter)
                .with_target(true)
                .json()
                .init();
        }
    }
}
