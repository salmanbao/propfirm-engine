//! Storage traits.
//!
//! **ADR-11**: account persistence is removed from the engine. The
//! `/internal/v1/evaluate` contract is fully stateless: callers supply
//! account state in the request and persist the returned state themselves.
//! This module remains as a placeholder for future non-account storage
//! needs.

use crate::core::events::DomainEvent;
use crate::core::Error;

/// No-op event store trait for compatibility.
pub trait EventStore: Send + Sync {
    /// Appends an event.
    fn append(&self, event: DomainEvent) -> Result<(), Error>;
}
