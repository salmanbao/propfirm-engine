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
    async fn replay(&self, id: AccountId) -> Result<crate::core::account::Account, Error>;
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

    async fn replay(&self, id: AccountId) -> Result<crate::core::account::Account, Error> {
        use crate::core::events::DomainEventKind as K;
        let events = self.all(id).await?;
        let mut acc = match events.first() {
            Some(DomainEvent {
                kind: K::AccountStarted { plan },
                ..
            }) => crate::core::account::Account::new(id, plan.clone()),
            _ => {
                return Err(crate::core::Error::NotFound(format!(
                    "no AccountStarted event for account {id}"
                )));
            }
        };
        for ev in events {
            match ev.kind {
                K::AccountStarted { .. } => {
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
                K::AccountSnapshotted { snapshot } => {
                    acc.status = snapshot.status;
                    acc.balance = snapshot.balance;
                    acc.equity = snapshot.equity;
                    acc.peak_equity = snapshot.peak_equity;
                    acc.peak_balance = snapshot.peak_balance;
                    acc.day_start_balance = snapshot.day_start_balance;
                    acc.active_trading_days = snapshot.active_trading_days;
                    acc.trading_day_index = snapshot.trading_day_index;
                    acc.target_reached_at = snapshot.target_reached_at;
                    acc.version = snapshot.version;
                }
                K::OrderEvent { new_status: _, .. } => {}
                K::PositionOpened {
                    position_id,
                    symbol,
                    side,
                    qty,
                } => {
                    if acc.open_positions.iter().any(|p| p.id == position_id) {
                        continue;
                    }
                    let now = ev.occurred_at;
                    acc.open_positions.push(crate::core::position::Position {
                        id: position_id,
                        account_id: id,
                        symbol: symbol.clone(),
                        side,
                        opened_at: now,
                        closed_at: None,
                        status: crate::core::position::PositionStatus::Open,
                        avg_entry_price: crate::core::types::Price::ZERO,
                        opened_quantity: qty,
                        open_quantity: qty,
                        realized_pnl: crate::core::types::Money::ZERO,
                        commission: crate::core::types::Money::ZERO,
                        swap: crate::core::types::Money::ZERO,
                        stop_loss: None,
                        take_profit: None,
                        magic: None,
                        comment: None,
                    });
                }
                K::PositionClosed {
                    position_id,
                    realized_pnl,
                } => {
                    if let Some(pos) = acc.open_positions.iter_mut().find(|p| p.id == position_id) {
                        pos.status = crate::core::position::PositionStatus::Closed;
                        pos.closed_at = Some(ev.occurred_at);
                        pos.realized_pnl = realized_pnl;
                        pos.open_quantity = crate::core::types::Quantity::ZERO;
                    }
                }
                K::RuleViolated { violation } => {
                    let terminal = match violation.kind {
                        crate::core::violation::ViolationKind::DailyDrawdown => {
                            Some(crate::core::account::AccountStatus::Failed)
                        }
                        crate::core::violation::ViolationKind::MaxDrawdown => {
                            Some(crate::core::account::AccountStatus::Failed)
                        }
                        crate::core::violation::ViolationKind::TrailingDrawdown => {
                            Some(crate::core::account::AccountStatus::Failed)
                        }
                        crate::core::violation::ViolationKind::ProfitTargetMissed => {
                            Some(crate::core::account::AccountStatus::Passed)
                        }
                        _ => None,
                    };
                    if let Some(status) = terminal {
                        acc.status = status;
                    }
                }
                K::PlanUpgraded { to_phase, .. } => {
                    acc.account_type = match to_phase {
                        crate::config::plan::ChallengePhase::Phase1 => {
                            crate::core::account::AccountType::Phase1
                        }
                        crate::config::plan::ChallengePhase::Phase2 => {
                            crate::core::account::AccountType::Phase2
                        }
                        crate::config::plan::ChallengePhase::Funded => {
                            crate::core::account::AccountType::Funded
                        }
                    };
                }
                K::PayoutRequested { .. } => {
                    acc.status = crate::core::account::AccountStatus::PayoutPending;
                }
                K::PayoutApproved { amount, fee_refund } => {
                    acc.balance = crate::core::types::Money(acc.balance.0 - amount.0);
                    acc.balance_at_last_payout = acc.balance;
                    acc.last_payout_at = Some(ev.occurred_at);
                    acc.payout_count += 1;
                    if fee_refund.0 > rust_decimal::Decimal::ZERO {
                        acc.refund_used = true;
                    }
                    acc.status = crate::core::account::AccountStatus::Funded;
                }
                K::LiquidationRequested { .. } => {
                    acc.status = crate::core::account::AccountStatus::Closed;
                }
                K::OverrideCleared { .. } => {
                    acc.status = crate::core::account::AccountStatus::Active;
                }
            }
        }
        Ok(acc)
    }
}
