//! HTTP server bootstrap: state, TLS, observability, graceful shutdown.
//!
//! ## D81: Stateless compute service (docs/64)
//!
//! The engine holds no state and opens no database connection. `workers`
//! owns `evaluation_state`, ordering, idempotency, retry and DLQ
//! (docs/64 §4.1). This file constructs a `ServerState` with in-memory
//! backends only — no Postgres pool, no Redis connection, no migrations.
//! The `pg_pool` field has been removed; audit-log writes are no-ops
//! (the platform's AUD module owns the audit trail).
//!
//! ## Components
//!
//! - [`ServerState`] — shared state with in-memory idempotency + event
//!   store backends (kept for API compat; the platform's `workers`
//!   consumer is the real idempotency/ordering mechanism).
//! - [`run_server`] — load settings, build the router, and serve with
//!   optional TLS + graceful shutdown. **No Postgres/Redis connection.**

use crate::api::handlers::SharedState;
use crate::api::idempotency::IdempotencyStore;
use crate::events::store::{EventStore, InMemoryEventStore};
use crate::notifications::log::LogNotifier;
use crate::settings::{Settings, TlsSettings};

use metrics_exporter_prometheus::{PrometheusBuilder, PrometheusHandle};
use std::sync::Arc;
use std::sync::OnceLock;
use tracing::info;

/// Shared server state.
///
/// All backends are in-memory — the engine is a stateless compute
/// service (D81). No Postgres pool, no Redis connection.
#[derive(Clone)]
pub struct ServerState {
    pub notifier: LogNotifier,
    pub idempotency: Arc<dyn crate::api::idempotency::IdempotencyBackend>,
    pub event_store: Arc<dyn EventStore>,
    /// Prometheus metrics render handle. The `/metrics` endpoint calls
    /// `.render()` on this. Cheap to clone (Arc internally).
    pub metrics_handle: PrometheusHandle,
}

impl ServerState {
    /// Build with the in-memory backends (default for tests / dev / prod).
    #[must_use]
    pub fn with_memory() -> Self {
        ServerState::with_memory_and_handle(default_metrics_handle())
    }

    /// Build with the in-memory backends + an explicit metrics handle.
    #[must_use]
    pub fn with_memory_and_handle(metrics_handle: PrometheusHandle) -> Self {
        ServerState {
            notifier: LogNotifier::new(),
            idempotency: Arc::new(IdempotencyStore::with_defaults()),
            event_store: Arc::new(InMemoryEventStore::new()),
            metrics_handle,
        }
    }

    /// Convenience: build a per-request pipeline (currently only used
    /// by tests / manual runs that want to drive the pipeline directly).
    #[must_use]
    pub fn pipeline(
        &self,
    ) -> crate::engine::pipeline::Pipeline<crate::notifications::log::LogNotifier> {
        crate::engine::pipeline::Pipeline::new(
            crate::engine::evaluator::Evaluator::with_registry(std::sync::Arc::new(
                crate::rules::registry::RuleRegistry::with_default_rules(),
            )),
            self.notifier.clone(),
        )
    }
}

/// Get-or-install the global Prometheus recorder handle.
///
/// Idempotent — the first call installs the recorder and caches the
/// handle. Subsequent calls (in tests, or when the binary already
/// called `init_metrics`) return the cached handle.
#[must_use]
pub fn default_metrics_handle() -> PrometheusHandle {
    static HANDLE: OnceLock<PrometheusHandle> = OnceLock::new();
    HANDLE
        .get_or_init(|| {
            PrometheusBuilder::new()
                .install_recorder()
                .expect("failed to install Prometheus recorder")
        })
        .clone()
}

/// Build the `ServerState` from `Settings` — **always in-memory** (D81).
///
/// No Postgres connection. No Redis connection. No migrations. The
/// engine is a stateless compute service; `workers` owns all state.
pub async fn build_state(
    _settings: &Settings,
    metrics_handle: PrometheusHandle,
) -> Result<ServerState, anyhow::Error> {
    Ok(ServerState::with_memory_and_handle(metrics_handle))
}

/// Load TLS config from settings (if enabled).
///
/// When `client_ca_path` is `Some`, configures the server to require
/// client certs signed by that CA (mTLS). When `None`, the server
/// accepts any TLS handshake (one-way TLS).
pub async fn load_tls_config(
    tls: &TlsSettings,
) -> Result<Option<axum_server::tls_rustls::RustlsConfig>, anyhow::Error> {
    if !tls.enabled {
        return Ok(None);
    }
    let cert = std::fs::read(&tls.cert_path).map_err(|e| {
        anyhow::anyhow!(
            "TLS cert read failed (path={}): {}",
            tls.cert_path.display(),
            e
        )
    })?;
    let key = std::fs::read(&tls.key_path).map_err(|e| {
        anyhow::anyhow!(
            "TLS key read failed (path={}): {}",
            tls.key_path.display(),
            e
        )
    })?;

    if let Some(client_ca_path) = &tls.client_ca_path {
        // mTLS path: build a `rustls::ServerConfig` directly so we can
        // install a client-cert verifier.
        let certs: Vec<rustls::pki_types::CertificateDer<'static>> = {
            let mut reader = std::io::BufReader::new(cert.as_slice());
            rustls_pemfile::certs(&mut reader).collect::<Result<Vec<_>, _>>()?
        };
        let mut key_reader = std::io::BufReader::new(key.as_slice());
        let private_key = rustls_pemfile::private_key(&mut key_reader)?
            .ok_or_else(|| anyhow::anyhow!("no private key found in {}", tls.key_path.display()))?;
        let cert_chain: Vec<rustls::pki_types::CertificateDer<'static>> = certs;

        // Build the rustls ServerConfig directly.
        let server_config_builder = rustls::server::ServerConfig::builder();
        // Apply the client-cert verifier when mTLS is configured.
        let ca_pem = std::fs::read(client_ca_path).map_err(|e| {
            anyhow::anyhow!(
                "mTLS CA read failed (path={}): {}",
                client_ca_path.display(),
                e
            )
        })?;
        let mut root_store = rustls::RootCertStore::empty();
        let mut ca_reader = std::io::BufReader::new(ca_pem.as_slice());
        for ca_cert in rustls_pemfile::certs(&mut ca_reader).collect::<Result<Vec<_>, _>>()? {
            root_store.add(ca_cert)?;
        }
        let verifier = rustls::server::WebPkiClientVerifier::builder(root_store.into())
            .build()
            .map_err(|e| anyhow::anyhow!("failed to build mTLS verifier: {e}"))?;
        let server_config = server_config_builder
            .with_client_cert_verifier(verifier)
            .with_single_cert(cert_chain, private_key)
            .map_err(|e| anyhow::anyhow!("failed to build rustls ServerConfig: {e}"))?;
        let config = axum_server::tls_rustls::RustlsConfig::from_config(Arc::new(server_config));
        info!(client_ca = %client_ca_path.display(), "mTLS client verification enabled");
        Ok(Some(config))
    } else {
        // One-way TLS: use the simple `from_pem` constructor.
        let config = axum_server::tls_rustls::RustlsConfig::from_pem(cert, key).await?;
        Ok(Some(config))
    }
}

/// Bind and serve the HTTP server (or HTTPS if TLS is enabled).
///
/// Orchestrates: load settings → build state → build router → optional
/// TLS → graceful shutdown. **No Postgres/Redis connection** (D81).
pub async fn run_server(settings: Settings) -> Result<(), anyhow::Error> {
    let addr = settings.server.bind_addr.clone();
    let shutdown_timeout = settings.shutdown_timeout();

    // Install panic hook.
    crate::api::middleware::install_panic_hook();

    // Build shared state — in-memory only (D81: no DB, no Redis).
    let metrics_handle = default_metrics_handle();
    let state = build_state(&settings, metrics_handle).await?;
    let shared: SharedState = Arc::new(state);
    let app = crate::api::routes::router_with_limits(
        shared,
        settings.server.max_body_bytes,
        settings.request_timeout(),
    )
    .await;

    // Pick TLS or plain.
    let tls_config = load_tls_config(&settings.server.tls).await?;

    info!(bind_addr = %addr, tls_enabled = tls_config.is_some(), "server starting (D81: stateless compute service, no DB)");

    if let Some(tls) = tls_config {
        // Build a shared `Handle` so we can call `shutdown()` from a
        // signal-listener task. This is the canonical axum_server pattern.
        let handle = axum_server::Handle::new();
        let signal_handle = handle.clone();
        tokio::spawn(async move {
            crate::api::shutdown::await_signal().await;
            info!(
                "shutdown signal received, draining in-flight requests (up to {}s)",
                shutdown_timeout.as_secs()
            );
            signal_handle.shutdown();
        });

        // Parse the bind address as a SocketAddr for axum_server's bind_rustls.
        let bind_addr: std::net::SocketAddr = addr
            .parse()
            .map_err(|e| anyhow::anyhow!("invalid bind_addr '{addr}': {e}"))?;
        let server = axum_server::bind_rustls(bind_addr, tls)
            .handle(handle)
            .serve(app.into_make_service());
        server.await?;
    } else {
        let listener = tokio::net::TcpListener::bind(&addr).await?;
        axum::serve(listener, app)
            .with_graceful_shutdown(crate::api::shutdown::axum_graceful_shutdown(
                shutdown_timeout,
            ))
            .await?;
    }

    info!("server stopped");
    Ok(())
}
