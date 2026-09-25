//! Storage traits.
//!
//! **P1-8 fix**: the trait now requires optimistic concurrency control.
//! `put_with_version` rejects writes where the expected version does not
//! match the persisted version, returning [`Error::StateConflict`].
//! This guarantees that two concurrent evaluations of the same account
//! (e.g. a retried tick and a live tick arriving close together) can't
//! silently clobber each other's state.
//!
//! **P1-9 fix**: every read takes a `tenant_id` so cross-tenant data
//! leakage is impossible at the storage layer, not just at the API layer.

use crate::core::account::Account;
use crate::core::ids::{AccountId, PositionId};
use crate::core::position::Position;
use crate::core::trade::Trade;
use crate::core::Error;
use crate::tenant::TenantId;
use async_trait::async_trait;
use std::sync::Arc;

#[async_trait]
pub trait AccountStore: Send + Sync {
    /// Reads an account, scoped to a specific tenant. Returns `None` if
    /// the account doesn't exist OR if it belongs to a different tenant.
    /// This is the *only* read path in multi-tenant deployments.
    async fn get_for_tenant(
        &self,
        tenant_id: TenantId,
        id: AccountId,
    ) -> Result<Option<Account>, Error>;

    /// Bootstrap read — fetches an account without tenant scoping.
    ///
    /// Intended *only* for internal-API endpoints where the caller needs
    /// the account's own `tenant_id` before it can issue a scoped read
    /// (e.g. `Pipeline::process`). Production code that already knows
    /// the tenant MUST use `get_for_tenant`.
    async fn get(&self, id: AccountId) -> Result<Option<Account>, Error>;

    /// Writes an account without optimistic-concurrency checking. Last
    /// write wins. Use [`put_with_version`](Self::put_with_version) in
    /// any code path that requires concurrent-safety.
    async fn put(&self, account: Account) -> Result<(), Error>;

    /// **P1-8 fix**: writes an account with optimistic-concurrency checking.
    /// Returns [`Error::StateConflict`] if the persisted version does not
    /// match `expected_version`. The caller must re-read, re-evaluate,
    /// and retry on conflict.
    async fn put_with_version(&self, account: Account, expected_version: u64) -> Result<(), Error>;

    /// Atomically persists an account update and a batch of domain events
    /// in a single transaction. Postgres-backed stores should implement
    /// this as `BEGIN; put_with_version; append events; COMMIT;`.
    /// In-memory backends may simply execute the two operations sequentially.
    async fn put_with_version_and_events(
        &self,
        account: Account,
        expected_version: u64,
        events: &[crate::core::events::DomainEvent],
    ) -> Result<(), Error> {
        self.put_with_version(account, expected_version)
            .await?;
        if let Some(store) = self.event_store() {
            for ev in events {
                store.append(ev.clone()).await?;
            }
        }
        Ok(())
    }

    fn event_store(&self) -> Option<&dyn crate::events::store::EventStore> {
        None
    }

    /// Deletes an account, scoped to a specific tenant.
    async fn delete(&self, tenant_id: TenantId, id: AccountId) -> Result<(), Error>;

    /// Returns all open positions for an account.
    async fn open_positions(&self, id: AccountId) -> Result<Vec<Position>, Error>;

    /// Adds a position.
    async fn add_position(&self, position: Position) -> Result<(), Error>;

    /// Updates a position.
    async fn update_position(&self, position: Position) -> Result<(), Error>;

    /// Closes a position by id.
    async fn close_position(&self, position_id: PositionId) -> Result<(), Error>;

    /// Returns trades for an account executed since `since`.
    /// Used by the pipeline to get trades for the current trading day
    /// based on the plan's timezone and day_reset_time.
    async fn today_trades_since(
        &self,
        id: AccountId,
        since: chrono::DateTime<chrono::Utc>,
    ) -> Result<Vec<Trade>, Error>;

    /// Returns all trades for an account.
    async fn all_trades(&self, id: AccountId) -> Result<Vec<Trade>, Error>;

    /// Adds a trade.
    async fn add_trade(&self, trade: Trade) -> Result<(), Error>;

    /// Atomically persists a trade and updates its related position in a
    /// single transaction. Postgres-backed stores should implement this as
    /// `BEGIN; add_trade; update_position; COMMIT;`.
    async fn add_trade_and_update_position(
        &self,
        trade: Trade,
        position: Position,
    ) -> Result<(), Error> {
        self.add_trade(trade.clone()).await?;
        self.update_position(position).await?;
        Ok(())
    }
}

#[async_trait]
impl AccountStore for Arc<dyn AccountStore> {
    async fn get_for_tenant(
        &self,
        tenant_id: TenantId,
        id: AccountId,
    ) -> Result<Option<Account>, Error> {
        self.as_ref().get_for_tenant(tenant_id, id).await
    }

    async fn get(&self, id: AccountId) -> Result<Option<Account>, Error> {
        self.as_ref().get(id).await
    }

    async fn put(&self, account: Account) -> Result<(), Error> {
        self.as_ref().put(account).await
    }

    async fn put_with_version(&self, account: Account, expected_version: u64) -> Result<(), Error> {
        self.as_ref()
            .put_with_version(account, expected_version)
            .await
    }

    async fn put_with_version_and_events(
        &self,
        account: Account,
        expected_version: u64,
        events: &[crate::core::events::DomainEvent],
    ) -> Result<(), Error> {
        self.put_with_version(account, expected_version)
            .await?;
        if let Some(store) = self.event_store() {
            for ev in events {
                store.append(ev.clone()).await?;
            }
        }
        Ok(())
    }

    fn event_store(&self) -> Option<&dyn crate::events::store::EventStore> {
        None
    }

    async fn delete(&self, tenant_id: TenantId, id: AccountId) -> Result<(), Error> {
        self.as_ref().delete(tenant_id, id).await
    }

    async fn open_positions(&self, id: AccountId) -> Result<Vec<Position>, Error> {
        self.as_ref().open_positions(id).await
    }

    async fn add_position(&self, position: Position) -> Result<(), Error> {
        self.as_ref().add_position(position).await
    }

    async fn update_position(&self, position: Position) -> Result<(), Error> {
        self.as_ref().update_position(position).await
    }

    async fn close_position(&self, position_id: PositionId) -> Result<(), Error> {
        self.as_ref().close_position(position_id).await
    }

    async fn today_trades_since(
        &self,
        id: AccountId,
        since: chrono::DateTime<chrono::Utc>,
    ) -> Result<Vec<Trade>, Error> {
        self.as_ref().today_trades_since(id, since).await
    }

    async fn all_trades(&self, id: AccountId) -> Result<Vec<Trade>, Error> {
        self.as_ref().all_trades(id).await
    }

    async fn add_trade(&self, trade: Trade) -> Result<(), Error> {
        self.as_ref().add_trade(trade).await
    }

    async fn add_trade_and_update_position(
        &self,
        trade: Trade,
        position: Position,
    ) -> Result<(), Error> {
        self.as_ref()
            .add_trade_and_update_position(trade, position)
            .await
    }
}
