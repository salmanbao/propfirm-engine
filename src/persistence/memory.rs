//! In-memory store implementation (no persistence).

use crate::core::events::DomainEvent;
use crate::core::Error;
use async_trait::async_trait;

/// No-op in-memory event store for testing.
#[derive(Default, Clone)]
pub struct InMemoryEventStore {
    events: std::sync::Arc<parking_lot::RwLock<Vec<DomainEvent>>>,
}

impl InMemoryEventStore {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl crate::events::store::EventStore for InMemoryEventStore {
    async fn append(&self, event: DomainEvent) -> Result<(), Error> {
        self.events.write().push(event);
        Ok(())
    }

    async fn all(
        &self,
        _account_id: crate::core::ids::AccountId,
    ) -> Result<Vec<DomainEvent>, Error> {
        Ok(self.events.read().clone())
    }

    async fn recent(
        &self,
        _account_id: crate::core::ids::AccountId,
        _limit: usize,
    ) -> Result<Vec<DomainEvent>, Error> {
        Ok(self.events.read().clone())
    }

    async fn replay(
        &self,
        _id: crate::core::ids::AccountId,
        initial: crate::core::account::Account,
    ) -> Result<crate::core::account::Account, Error> {
        Ok(initial)
    }
}
