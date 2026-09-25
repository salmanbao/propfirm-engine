//! In-memory store implementation (no persistence).

use crate::core::account::Account;
use crate::core::ids::{AccountId, PositionId};
use crate::core::position::Position;
use crate::core::trade::Trade;
use crate::core::Error;
use crate::persistence::traits::AccountStore;
use async_trait::async_trait;
use chrono::Utc;
use parking_lot::RwLock;
use std::collections::HashMap;
use std::sync::Arc;

#[derive(Default, Clone)]
pub struct InMemoryStore {
    accounts: Arc<RwLock<HashMap<AccountId, Account>>>,
    positions: Arc<RwLock<HashMap<AccountId, Vec<Position>>>>,
    trades: Arc<RwLock<HashMap<AccountId, Vec<Trade>>>>,
}

impl InMemoryStore {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl AccountStore for InMemoryStore {
    async fn get(&self, id: AccountId) -> Result<Option<Account>, Error> {
        Ok(self.accounts.read().get(&id).cloned())
    }
    async fn get_for_tenant(
        &self,
        tenant_id: crate::tenant::TenantId,
        id: AccountId,
    ) -> Result<Option<Account>, Error> {
        Ok(self
            .accounts
            .read()
            .get(&id)
            .filter(|a| a.tenant_id == tenant_id)
            .cloned())
    }
    async fn put(&self, account: Account) -> Result<(), Error> {
        // Last-write-wins path; bumps version unconditionally.
        let mut accounts = self.accounts.write();
        let mut updated = account;
        match accounts.get_mut(&updated.id) {
            Some(existing) => {
                updated.version = existing.version.wrapping_add(1);
                *existing = updated;
            }
            None => {
                // New account: version stays at 0 (set by caller).
                accounts.insert(updated.id, updated);
            }
        }
        Ok(())
    }

    /// **P1-8 fix**: in-memory optimistic concurrency check. Reads the
    /// persisted version and rejects the write if it doesn't match
    /// `expected_version`.
    async fn put_with_version(&self, account: Account, expected_version: u64) -> Result<(), Error> {
        let mut accounts = self.accounts.write();
        let existing = accounts
            .get(&account.id)
            .ok_or_else(|| Error::NotFound(format!("account {}", account.id)))?;
        if existing.version != expected_version {
            return Err(Error::StateConflict(
                format!("account {}", account.id),
                expected_version,
                existing.version,
            ));
        }
        // Apply: bump version, write fields.
        let mut updated = account.clone();
        updated.version = expected_version.wrapping_add(1);
        *accounts.get_mut(&account.id).unwrap() = updated;
        Ok(())
    }

    fn event_store(&self) -> Option<&dyn crate::events::store::EventStore> {
        None
    }

    async fn put_with_version_and_events(
        &self,
        account: Account,
        expected_version: u64,
        _events: &[crate::core::events::DomainEvent],
    ) -> Result<(), Error> {
        self.put_with_version(account, expected_version).await
    }
    async fn delete(&self, tenant_id: crate::tenant::TenantId, id: AccountId) -> Result<(), Error> {
        let account = self.accounts.read().get(&id).cloned();
        if let Some(account) = account {
            if account.tenant_id != tenant_id {
                return Ok(());
            }
        }
        self.accounts.write().remove(&id);
        self.positions.write().remove(&id);
        self.trades.write().remove(&id);
        Ok(())
    }
    async fn open_positions(&self, id: AccountId) -> Result<Vec<Position>, Error> {
        Ok(self
            .positions
            .read()
            .get(&id)
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .filter(super::super::core::position::Position::is_open)
            .collect())
    }
    async fn add_position(&self, position: Position) -> Result<(), Error> {
        let mut w = self.positions.write();
        let v = w.entry(position.account_id).or_default();
        v.push(position);
        Ok(())
    }
    async fn update_position(&self, position: Position) -> Result<(), Error> {
        let mut w = self.positions.write();
        let v = w.entry(position.account_id).or_default();
        if let Some(idx) = v.iter().position(|p| p.id == position.id) {
            v[idx] = position;
        } else {
            v.push(position);
        }
        Ok(())
    }
    async fn close_position(&self, position_id: PositionId) -> Result<(), Error> {
        let mut w = self.positions.write();
        for v in w.values_mut() {
            if let Some(idx) = v.iter().position(|p| p.id == position_id) {
                v[idx].status = crate::core::position::PositionStatus::Closed;
                v[idx].closed_at = Some(Utc::now());
                return Ok(());
            }
        }
        Err(Error::NotFound(format!("position {position_id} not found")))
    }
    async fn today_trades_since(
        &self,
        id: AccountId,
        since: chrono::DateTime<chrono::Utc>,
    ) -> Result<Vec<Trade>, Error> {
        Ok(self
            .trades
            .read()
            .get(&id)
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .filter(|t| t.executed_at >= since)
            .collect())
    }
    async fn all_trades(&self, id: AccountId) -> Result<Vec<Trade>, Error> {
        Ok(self.trades.read().get(&id).cloned().unwrap_or_default())
    }
    async fn add_trade(&self, trade: Trade) -> Result<(), Error> {
        let mut w = self.trades.write();
        w.entry(trade.account_id).or_default().push(trade);
        Ok(())
    }

    async fn add_trade_and_update_position(
        &self,
        trade: Trade,
        position: Position,
    ) -> Result<(), Error> {
        let mut trades = self.trades.write();
        trades.entry(trade.account_id).or_default().push(trade);
        let mut positions = self.positions.write();
        let v = positions.entry(position.account_id).or_default();
        if let Some(idx) = v.iter().position(|p| p.id == position.id) {
            v[idx] = position;
        } else {
            v.push(position);
        }
        Ok(())
    }
}
