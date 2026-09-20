//! Event types and aliases. Re-exports the underlying `DomainEvent` and
//! `DomainEventKind` so downstream consumers don't need to dig into the
//! `core::events` module.

pub use crate::core::events::{DomainEvent, DomainEventKind};
pub use crate::core::ids::EventId;
