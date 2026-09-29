//! PostgreSQL-backed event store.
//!
//! Stores domain events in the `events` table (see
//! [`migrations/0001_init.sql`](../migrations/0001_init.sql)) for audit
//! and replay. The `replay` method reconstructs `Account` state from
//! the event log — the dispute-resolution seam for regulators.

use async_trait::async_trait;
use sqlx::{PgPool, Row};
use std::sync::Arc;
use uuid::Uuid;

use crate::core::events::{DomainEvent, DomainEventKind};
use crate::core::ids::AccountId;
use crate::core::Error;

use crate::events::store::EventStore;

/// PostgreSQL implementation of [`EventStore`].
#[derive(Clone)]
pub struct PostgresEventStore {
    pool: Arc<PgPool>,
}

impl PostgresEventStore {
    #[must_use]
    pub fn new(pool: Arc<PgPool>) -> Self {
        PostgresEventStore { pool }
    }
}

#[async_trait]
impl EventStore for PostgresEventStore {
    async fn append(&self, ev: DomainEvent) -> Result<(), Error> {
        let id = ev.id.raw();
        let account_id = ev.account_id.raw();
        // TODO: thread `tenant_id` through `DomainEvent` so the event itself
        // carries its tenant. For now we store a nil UUID — the column is
        // indexed but not load-bearing. The `events_tenant_idx` is still
        // useful when the caller passes a real tenant_id via the wrapper.
        let tenant_id = Uuid::nil();
        let kind = event_kind_str(&ev.kind);
        let payload = serde_json::to_value(&ev.kind)
            .map_err(|e| Error::Persistence(format!("serialize event payload: {e}")))?;
        sqlx::query(
            r#"INSERT INTO events (id, account_id, tenant_id, kind, payload, occurred_at, causation_id)
               VALUES ($1, $2, $3, $4, $5, $6, $7)"#,
        )
        .bind(id)
        .bind(account_id)
        .bind(tenant_id)
        .bind(kind)
        .bind(payload)
        .bind(ev.occurred_at)
        .bind(ev.causation_id.map(|c| c.raw()))
        .execute(&*self.pool)
        .await
        .map_err(|e| Error::Persistence(format!("insert event: {e}")))?;
        Ok(())
    }

    async fn all(&self, id: AccountId) -> Result<Vec<DomainEvent>, Error> {
        let account_uuid = id.raw();
        let rows = sqlx::query(
            r#"SELECT id, account_id, tenant_id, kind, payload, occurred_at, causation_id
               FROM events WHERE account_id = $1 ORDER BY occurred_at ASC, inserted_at ASC"#,
        )
        .bind(account_uuid)
        .fetch_all(&*self.pool)
        .await
        .map_err(|e| Error::Persistence(format!("query events: {e}")))?;

        rows.iter().map(row_to_event).collect()
    }

    async fn recent(&self, id: AccountId, n: usize) -> Result<Vec<DomainEvent>, Error> {
        let account_uuid = id.raw();
        let limit: i64 = n.try_into().unwrap_or(i64::MAX);
        let rows = sqlx::query(
            r#"SELECT id, account_id, tenant_id, kind, payload, occurred_at, causation_id
               FROM events WHERE account_id = $1
               ORDER BY occurred_at DESC, inserted_at DESC LIMIT $2"#,
        )
        .bind(account_uuid)
        .bind(limit)
        .fetch_all(&*self.pool)
        .await
        .map_err(|e| Error::Persistence(format!("query recent events: {e}")))?;

        let mut events: Vec<DomainEvent> = rows
            .iter()
            .rev()
            .map(row_to_event)
            .collect::<Result<_, _>>()?;
        // Already in chronological order (we reversed the DESC query above).
        let _ = &mut events;
        Ok(events)
    }

    async fn replay(&self, id: AccountId) -> Result<crate::core::account::Account, Error> {
        // Defer to the in-memory replay logic — load events then walk them.
        // This keeps replay semantics identical between backends.
        let events = self.all(id).await?;
        if events.is_empty() {
            return Err(Error::NotFound(format!("no events for account {id}")));
        }
        replay_events(id, &events)
    }
}

fn row_to_event(row: &sqlx::postgres::PgRow) -> Result<DomainEvent, Error> {
    let id: Uuid = row
        .try_get("id")
        .map_err(|e| Error::Persistence(format!("decode id: {e}")))?;
    let account_id: Uuid = row
        .try_get("account_id")
        .map_err(|e| Error::Persistence(format!("decode account_id: {e}")))?;
    // tenant_id is read but not stored on DomainEvent — see `append`
    // for the TODO about threading tenant_id through DomainEvent.
    let _tenant_id: Uuid = row
        .try_get("tenant_id")
        .map_err(|e| Error::Persistence(format!("decode tenant_id: {e}")))?;
    let _kind: String = row
        .try_get("kind")
        .map_err(|e| Error::Persistence(format!("decode kind: {e}")))?;
    let payload: serde_json::Value = row
        .try_get("payload")
        .map_err(|e| Error::Persistence(format!("decode payload: {e}")))?;
    let occurred_at: chrono::DateTime<chrono::Utc> = row
        .try_get("occurred_at")
        .map_err(|e| Error::Persistence(format!("decode occurred_at: {e}")))?;
    let causation_id: Option<Uuid> = row
        .try_get("causation_id")
        .map_err(|e| Error::Persistence(format!("decode causation_id: {e}")))?;

    let kind: DomainEventKind = serde_json::from_value(payload)
        .map_err(|e| Error::Persistence(format!("deserialize event kind: {e}")))?;

    Ok(DomainEvent {
        id: crate::core::ids::EventId::from_uuid(id),
        account_id: AccountId::from_uuid(account_id),
        kind,
        occurred_at,
        causation_id: causation_id.map(crate::core::ids::EventId::from_uuid),
    })
}

/// String tag for the event kind, used for indexing in the `kind` column.
fn event_kind_str(k: &DomainEventKind) -> &'static str {
    use DomainEventKind as K;
    match k {
        K::AccountStarted { .. } => "AccountStarted",
        K::AccountStatusChanged { .. } => "AccountStatusChanged",
        K::AccountSnapshotted { .. } => "AccountSnapshotted",
        K::OrderEvent { .. } => "OrderEvent",
        K::TradeFilled { .. } => "TradeFilled",
        K::PositionOpened { .. } => "PositionOpened",
        K::PositionClosed { .. } => "PositionClosed",
        K::TickEvaluated { .. } => "TickEvaluated",
        K::DayRollover { .. } => "DayRollover",
        K::RuleViolated { .. } => "RuleViolated",
        K::PlanUpgraded { .. } => "PlanUpgraded",
        K::PayoutRequested { .. } => "PayoutRequested",
        K::PayoutApproved { .. } => "PayoutApproved",
        K::LiquidationRequested { .. } => "LiquidationRequested",
        K::OverrideCleared { .. } => "OverrideCleared",
    }
}

/// In-process replay (mirrors `InMemoryEventStore::replay` semantics).
fn replay_events(
    id: AccountId,
    events: &[DomainEvent],
) -> Result<crate::core::account::Account, Error> {
    use crate::core::events::DomainEventKind as K;
    let mut acc = match events.first() {
        Some(DomainEvent {
            kind: K::AccountStarted { plan },
            ..
        }) => crate::core::account::Account::new(id, plan.clone()),
        _ => {
            return Err(Error::NotFound(format!(
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
            K::TradeFilled { ref trade } => {
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
            K::AccountSnapshotted { ref snapshot } => {
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
            K::OrderEvent { .. } => {}
            K::PositionOpened {
                position_id,
                ref symbol,
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
            K::RuleViolated { ref violation } => {
                let terminal = match violation.kind {
                    crate::core::violation::ViolationKind::DailyDrawdown
                    | crate::core::violation::ViolationKind::MaxDrawdown
                    | crate::core::violation::ViolationKind::TrailingDrawdown => {
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
