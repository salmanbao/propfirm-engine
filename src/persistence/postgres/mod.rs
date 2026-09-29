//! PostgreSQL durable persistence backends.
//!
//! Three implementations:
//! - [`PostgresEventStore`] — append-only domain event log (`events` table)
//! - [`PostgresIdempotencyBackend`] — durable idempotency-key dedup
//!   (`idempotency` table)
//! - [`run_migrations`] — runs pending migrations from
//!   `src/persistence/migrations/`
//!
//! All backends use `sqlx` with `runtime-tokio-rustls` for connection
//! pooling. The pool is sized by `Settings::postgres.max_connections`.
//!
//! # Schema
//!
//! See [`migrations/0001_init.sql`](migrations/0001_init.sql).

pub mod event_store;
pub mod idempotency;

use crate::settings::PostgresSettings;
use sqlx::postgres::PgPoolOptions;
use sqlx::PgPool;

pub use event_store::PostgresEventStore;
pub use idempotency::PostgresIdempotencyBackend;

/// Build a `PgPool` from settings.
///
/// # Errors
/// Returns an error if the pool cannot be created or the connection
/// acquisition timeout fires.
pub async fn connect(settings: &PostgresSettings) -> Result<PgPool, sqlx::Error> {
    PgPoolOptions::new()
        .max_connections(settings.max_connections)
        .acquire_timeout(std::time::Duration::from_secs(
            settings.acquire_timeout_secs,
        ))
        .connect(&settings.dsn)
        .await
}

/// Run pending migrations from `src/persistence/migrations/`.
///
/// # Errors
/// Returns an error if any migration fails.
pub async fn run_migrations(pool: &PgPool) -> Result<(), sqlx::migrate::MigrateError> {
    sqlx::migrate!("./src/persistence/migrations")
        .run(pool)
        .await
}
