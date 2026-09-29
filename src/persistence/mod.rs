//! Persistence layer.
//!
//! ## Backends
//!
//! | Backend | Use case |
//! |---|---|
//! | [`traits`] | Sync placeholder (legacy). |
//! | [`events::store::InMemoryEventStore`] | Dev/test only. |
//! | [`postgres::PostgresEventStore`] | Durable event log (Postgres). |
//! | [`postgres::PostgresIdempotencyBackend`] | Durable idempotency (Postgres). |
//! | [`redis_store::RedisIdempotencyBackend`] | Durable idempotency (Redis). |
//! | [`redis_store::RedisEventBus`] | Redis Streams event bus. |
//!
//! ## ADR-11 reminder
//!
//! Account state is **caller-owned** — the engine does not persist accounts.
//! The durable backends above are for:
//! - the event-sourcing audit log (events)
//! - cross-replica idempotency dedup
//! - async communication with the platform backend (Redis Streams)

pub mod traits;

#[cfg(feature = "server")]
pub mod postgres;
#[cfg(feature = "server")]
pub mod redis_store;

#[cfg(feature = "server")]
pub mod migrations {
    //! Compiled-in migrations. Use [`postgres::run_migrations`] to apply.
    pub use sqlx::migrate::*;
}
