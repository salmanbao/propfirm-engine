//! HTTP server bootstrap: state, TLS, observability, graceful shutdown.
//!
//! ## No authentication
//!
//! The engine is deployed as an internal component of the Prop Firm as
//! a Service Platform. The platform backend is the sole caller over the
//! private compose network. Trust is established at the network
//! boundary, not in-process. Authentication has been intentionally
//! removed.
//!
//! ## Components
//!
//! - [`ServerState`] — shared state with the configured idempotency
//!   backend (memory/postgres/redis).
//! - [`run_server`] — load settings, optionally run migrations, build
//!   the router, and serve with optional TLS + graceful shutdown.

use crate::api::handlers::SharedState;
use crate::api::idempotency::{IdempotencyBackend, IdempotencyStore};
use crate::events::store::{EventStore, InMemoryEventStore};
use crate::notifications::log::LogNotifier;
use crate::persistence::postgres::{PostgresEventStore, PostgresIdempotencyBackend};
use crate::persistence::redis_store::{RedisConn, RedisIdempotencyBackend};
use crate::settings::{Settings, TlsSettings};

use metrics_exporter_prometheus::{PrometheusBuilder, PrometheusHandle};
use std::sync::Arc;
use std::sync::OnceLock;
use std::time::Duration;
use tokio::sync::RwLock;
use tracing::{info, warn};

/// Shared server state.
///
/// All backends are stored as `Arc<dyn ...>` trait objects so the
/// handler clones share the underlying connection pool / connection
/// manager. The `metrics_handle` is a clone of the global Prometheus
/// recorder handle (cheaply cloneable).
#[derive(Clone)]
pub struct ServerState {
    pub notifier: LogNotifier,
    pub idempotency: Arc<dyn IdempotencyBackend>,
    pub event_store: Arc<dyn EventStore>,
    /// Prometheus metrics render handle. The `/metrics` endpoint calls
    /// `.render()` on this. Cheap to clone (Arc internally).
    pub metrics_handle: PrometheusHandle,
    /// Optional Postgres pool for audit-log writes (used by the
    /// `Override` and `EmergencyStop` handlers when `audit_log` table
    /// is configured).
    pub pg_pool: Option<Arc<sqlx::PgPool>>,
}

impl ServerState {
    /// Build with the in-memory backends (default for tests / dev).
    #[must_use]
    pub fn with_memory() -> Self {
        ServerState::with_memory_and_handle(default_metrics_handle(), None)
    }

    /// Build with the in-memory backends + an explicit metrics handle +
    /// optional pg pool (for tests).
    #[must_use]
    pub fn with_memory_and_handle(
        metrics_handle: PrometheusHandle,
        pg_pool: Option<Arc<sqlx::PgPool>>,
    ) -> Self {
        ServerState {
            notifier: LogNotifier::new(),
            idempotency: Arc::new(IdempotencyStore::with_defaults()),
            event_store: Arc::new(InMemoryEventStore::new()),
            metrics_handle,
            pg_pool,
        }
    }

    /// Build with Postgres backends (production durable).
    #[must_use]
    pub fn with_postgres(
        pool: Arc<sqlx::PgPool>,
        idempotency_ttl: Duration,
        metrics_handle: PrometheusHandle,
    ) -> Self {
        ServerState {
            notifier: LogNotifier::new(),
            idempotency: Arc::new(PostgresIdempotencyBackend::new(
                pool.clone(),
                idempotency_ttl,
            )),
            event_store: Arc::new(PostgresEventStore::new(pool.clone())),
            metrics_handle,
            pg_pool: Some(pool),
        }
    }

    /// Build with Redis idempotency (Postgres still used for the event store,
    /// or fall back to in-memory event store if Postgres isn't configured).
    #[must_use]
    pub fn with_redis_idempotency(
        redis_conn: RedisConn,
        idempotency_ttl: Duration,
        pool: Option<Arc<sqlx::PgPool>>,
        metrics_handle: PrometheusHandle,
    ) -> Self {
        let event_store: Arc<dyn EventStore> = match &pool {
            Some(p) => Arc::new(PostgresEventStore::new(p.clone())),
            None => Arc::new(InMemoryEventStore::new()),
        };
        ServerState {
            notifier: LogNotifier::new(),
            idempotency: Arc::new(RedisIdempotencyBackend::new(redis_conn, idempotency_ttl)),
            event_store,
            metrics_handle,
            pg_pool: pool,
        }
    }

    /// Convenience: build a per-request pipeline (currently only used
    /// by tests / manual runs that want to drive the pipeline directly).
    #[must_use]
    pub fn pipeline(
        &self,
    ) -> crate::engine::pipeline::Pipeline<crate::notifications::log::LogNotifier> {
        crate::engine::pipeline::Pipeline::new(
            crate::engine::evaluator::Evaluator::with_registry(
                crate::rules::registry::RuleRegistry::with_default_rules(),
            ),
            self.notifier.clone(),
        )
    }
}

/// Get-or-install the global Prometheus recorder handle.
///
/// Idempotent — the first call installs the recorder and caches the
/// handle. Subsequent calls (in tests, or when the binary already
/// called `init_metrics`) return the cached handle.
///
/// The global recorder is process-wide, so all clones of `ServerState`
/// share the same metrics registry.
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

/// Build the `ServerState` from `Settings` — picks the configured
/// idempotency backend, connects to Postgres/Redis, runs migrations
/// if requested.
///
/// The `metrics_handle` is created by the binary's `init_metrics()` and
/// passed in — this ensures the global recorder is installed exactly
/// once.
pub async fn build_state(
    settings: &Settings,
    metrics_handle: PrometheusHandle,
) -> Result<ServerState, anyhow::Error> {
    let idem_ttl = settings
        .idempotency_ttl()
        .unwrap_or_else(|| Duration::from_secs(86_400));

    // Postgres pool — needed when idempotency backend is postgres OR
    // migrations are requested OR the event store should be durable.
    let pg_pool = match settings.idempotency.backend.as_str() {
        "postgres" | "redis" => Some(Arc::new(
            crate::persistence::postgres::connect(&settings.postgres).await?,
        )),
        _ => {
            if settings.postgres.run_migrations {
                Some(Arc::new(
                    crate::persistence::postgres::connect(&settings.postgres).await?,
                ))
            } else {
                None
            }
        }
    };

    if let Some(pool) = &pg_pool {
        if settings.postgres.run_migrations {
            info!("running pending migrations");
            crate::persistence::postgres::run_migrations(pool).await?;
            info!("migrations complete");
        }
    }

    let state = match settings.idempotency.backend.as_str() {
        "memory" => ServerState::with_memory_and_handle(metrics_handle, pg_pool),
        "postgres" => {
            let pool = pg_pool.expect("postgres pool required for postgres backend");
            ServerState::with_postgres(pool, idem_ttl, metrics_handle)
        }
        "redis" => {
            let redis_conn = crate::persistence::redis_store::connect(&settings.redis).await?;
            ServerState::with_redis_idempotency(
                redis_conn,
                idem_ttl,
                pg_pool.clone(),
                metrics_handle,
            )
        }
        other => {
            warn!(backend = %other, "unknown idempotency backend, falling back to memory");
            ServerState::with_memory_and_handle(metrics_handle, pg_pool)
        }
    };

    Ok(state)
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
                "mTLS client CA read failed (path={}): {}",
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
/// TLS → graceful shutdown.
pub async fn run_server(settings: Settings) -> Result<(), anyhow::Error> {
    let addr = settings.server.bind_addr.clone();
    let shutdown_timeout = settings.shutdown_timeout();

    // Install panic hook.
    crate::api::middleware::install_panic_hook();

    // Build shared state. `default_metrics_handle()` installs the
    // global Prometheus recorder if not already installed (idempotent
    // via OnceLock).
    let metrics_handle = default_metrics_handle();
    let state = build_state(&settings, metrics_handle).await?;
    let shared: SharedState = Arc::new(RwLock::new(state));
    let app = crate::api::routes::router(shared).await;

    // Pick TLS or plain.
    let tls_config = load_tls_config(&settings.server.tls).await?;

    info!(bind_addr = %addr, tls_enabled = tls_config.is_some(), "server starting");

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
