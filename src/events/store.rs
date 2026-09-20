//! Event store: an append-only log of domain events.

use crate::core::events::DomainEvent;
use crate::core::ids::AccountId;
use crate::core::Error;
use parking_lot::RwLock;
use std::collections::HashMap;
use std::sync::Arc;

/// Append-only event log.
pub struct EventStore {
    events: Arc<RwLock<HashMap<AccountId, Vec<DomainEvent>>>>,
}

impl Default for EventStore {
    fn default() -> Self { Self::in_memory() }
}

impl EventStore {
    pub fn in_memory() -> Self {
        EventStore { events: Arc::new(RwLock::new(HashMap::new())) }
    }

    pub fn append(&self, ev: DomainEvent) -> Result<(), Error> {
        let mut w = self.events.write();
        w.entry(ev.account_id).or_default().push(ev);
        Ok(())
    }

    pub fn all(&self, id: AccountId) -> Vec<DomainEvent> {
        self.events.read().get(&id).cloned().unwrap_or_default()
    }

    pub fn recent(&self, id: AccountId, n: usize) -> Vec<DomainEvent> {
        let all = self.all(id);
        let len = all.len();
        if len > n {
            all[len - n..].to_vec()
        } else {
            all
        }
    }

    /// Replays the entire event log to reconstruct account state. Returns
    /// the final account after applying all events.
    pub fn replay(&self, id: AccountId, initial: crate::core::account::Account) -> Result<crate::core::account::Account, Error> {
        use crate::core::events::DomainEventKind as K;
        let events = self.all(id);
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
                    acc.total_realized_pnl = crate::core::types::Money(acc.total_realized_pnl.0 + net.0);
                    if acc.balance.0 > acc.peak_balance.0 {
                        acc.peak_balance = acc.balance;
                    }
                }
                K::DayRollover { new_day_index, day_start } => {
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
