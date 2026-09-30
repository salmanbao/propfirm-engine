//! Optional HTTP API layer (axum-based).
//!
//! **No authentication.** The engine is an internal service reached only
//! from the platform backend over the private compose network (or via the
//! Redis event bus worker). The caller is the platform itself; trust is
//! established at the network boundary, not in-process.
//!
//! Modules:
//! - [`dto`] — wire shapes for requests/responses
//! - [`handlers`] — axum handlers (no auth extraction)
//! - [`idempotency`] — in-memory idempotency backend (durable backends
//!   live in [`crate::persistence`])
//! - [`middleware`] — request-id propagation, panic hook, error IntoResponse
//! - [`routes`] — router factory
//! - [`server`] — server state + bootstrap (TLS, observability, shutdown)
//! - [`shutdown`] — graceful shutdown signal handling

pub mod audit_log;
pub mod dto;
pub mod handlers;
pub mod idempotency;
pub mod metrics;
pub mod middleware;
#[cfg(feature = "openapi")]
pub mod openapi;
pub mod otel;
pub mod routes;
pub mod server;
pub mod shutdown;

// Convenience re-exports so callers can write `use propfirm::api::IdempotencyBackend`
// instead of the longer `use propfirm::api::idempotency::IdempotencyBackend`.
pub use idempotency::{IdempotencyBackend, IdempotencyOutcome, IdempotencyStore};
pub use middleware::ApiError;
