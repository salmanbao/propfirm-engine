//! Event store: an append-only log of domain events.

use crate::core::events::DomainEvent;
use crate::core::ids::AccountId;
use crate::core::Error;
use async_trait::async_trait;
use parking_lot::RwLock;
use std::collections::HashMap;
use std::sync::Arc;

/// Append-only event log backend.
#[async_trait]
pub trait EventStore: Send + Sync {
    /// Appends a new event to the log.
    async fn append(&self, ev: DomainEvent) -> Result<(), Error>;
    /// Returns all events for an account, in order.
    async fn all(&self, id: AccountId) -> Result<Vec<DomainEvent>, Error>;
    /// Returns the most recent `n` events for an account.
    async fn recent(&self, id: AccountId, n: usize) -> Result<Vec<DomainEvent>, Error>;
    /// Replays the event log to reconstruct account state.
    async fn replay(
        &self,
        id: AccountId,
        initial: crate::core::account::Account,
    ) -> Result<crate::core::account::Account, Error>;
}

/// In-memory event store implementation.
#[derive(Clone, Default)]
pub struct InMemoryEventStore {
    events: Arc<RwLock<HashMap<AccountId, Vec<DomainEvent>>>>,
}

impl InMemoryEventStore {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn in_memory() -> Self {
        Self::new()
    }
}

#[async_trait]
impl EventStore for InMemoryEventStore {
    async fn append(&self, ev: DomainEvent) -> Result<(), Error> {
        let mut w = self.events.write();
        w.entry(ev.account_id).or_default().push(ev);
        Ok(())
    }

    async fn all(&self, id: AccountId) -> Result<Vec<DomainEvent>, Error> {
        Ok(self.events.read().get(&id).cloned().unwrap_or_default())
    }

    async fn recent(&self, id: AccountId, n: usize) -> Result<Vec<DomainEvent>, Error> {
        let all = self.all(id).await?;
        let len = all.len();
        if len > n {
            Ok(all[len - n..].to_vec())
        } else {
            Ok(all)
        }
    }

    async fn replay(
        &self,
        id: AccountId,
        initial: crate::core::account::Account,
    ) -> Result<crate::core::account::Account, Error> {
        use crate::core::events::DomainEventKind as K;
        let events = self.all(id).await?;
        let mut acc = initial;
        for ev in events {
            match ev.kind {
                K::AccountStarted => {
                    acc = acc.start(ev.occurred_at)?;
                }
                K::AccountStatusChanged { to, .. } => {
                    acc.status = to;
                }
                K::TradeFilled { trade } => {
                    let net = trade.net_pnl();
                    acc.balance = crate::core::types::Money(acc.balance.0 + net.0);
                    acc.total_realized_pnl =
                        crate::core::types::Money(acc.total_realized_pnl.0 + net.0);
                    if acc.balance.0 > acc.peak_balance.0 {
                        acc.peak_balance = acc.balance;
                    }
                }
                K::DayRollover {
                    new_day_index,
                    day_start,
                } => {
                    if acc.today_realized_pnl.0 != crate::core::types::dec!(0) {
                        acc.active_trading_days += 1;
                    }
                    acc.trading_day_index = new_day_index;
                    acc.day_start_balance = day_start;
                    acc.today_realized_pnl = crate::core::types::Money::ZERO;
                }
                K::TickEvaluated { equity } => {
                    acc.equity = equity;
                    if equity.0 > acc.peak_equity.0 {
                        acc.peak_equity = equity;
                    }
                }
                _ => {}
            }
        }
        Ok(acc)
    }
}
