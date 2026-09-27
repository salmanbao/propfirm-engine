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
        id: crate::core::ids::AccountId,
    ) -> Result<crate::core::account::Account, Error> {
        let events = self.all(id).await?;
        let mut acc = match events.first() {
            Some(crate::core::events::DomainEvent {
                kind: crate::core::events::DomainEventKind::AccountStarted { plan },
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
                crate::core::events::DomainEventKind::AccountStarted { .. } => {}
                crate::core::events::DomainEventKind::AccountStatusChanged { to, .. } => {
                    acc.status = to;
                }
                crate::core::events::DomainEventKind::TradeFilled { trade } => {
                    let net = trade.net_pnl();
                    acc.balance = crate::core::types::Money(acc.balance.0 + net.0);
                    acc.total_realized_pnl =
                        crate::core::types::Money(acc.total_realized_pnl.0 + net.0);
                    if acc.balance.0 > acc.peak_balance.0 {
                        acc.peak_balance = acc.balance;
                    }
                }
                crate::core::events::DomainEventKind::DayRollover {
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
                crate::core::events::DomainEventKind::TickEvaluated { equity } => {
                    acc.equity = equity;
                    if equity.0 > acc.peak_equity.0 {
                        acc.peak_equity = equity;
                    }
                }
                crate::core::events::DomainEventKind::AccountSnapshotted { snapshot } => {
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
                crate::core::events::DomainEventKind::OrderEvent { new_status: _, .. } => {}
                crate::core::events::DomainEventKind::PositionOpened {
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
                crate::core::events::DomainEventKind::PositionClosed {
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
                crate::core::events::DomainEventKind::RuleViolated { violation } => {
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
                crate::core::events::DomainEventKind::PlanUpgraded { to_phase, .. } => {
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
                crate::core::events::DomainEventKind::PayoutRequested { .. } => {
                    acc.status = crate::core::account::AccountStatus::PayoutPending;
                }
                crate::core::events::DomainEventKind::PayoutApproved { amount, fee_refund } => {
                    acc.balance = crate::core::types::Money(acc.balance.0 - amount.0);
                    acc.balance_at_last_payout = acc.balance;
                    acc.last_payout_at = Some(ev.occurred_at);
                    acc.payout_count += 1;
                    if fee_refund.0 > rust_decimal::Decimal::ZERO {
                        acc.refund_used = true;
                    }
                    acc.status = crate::core::account::AccountStatus::Funded;
                }
                crate::core::events::DomainEventKind::LiquidationRequested { .. } => {
                    acc.status = crate::core::account::AccountStatus::Closed;
                }
                crate::core::events::DomainEventKind::OverrideCleared { .. } => {
                    acc.status = crate::core::account::AccountStatus::Active;
                }
            }
        }
        Ok(acc)
    }
}
