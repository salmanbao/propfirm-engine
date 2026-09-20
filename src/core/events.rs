//! Domain events emitted by the engine.
//!
//! The event log is the source-of-truth for account state. Every state
//! mutation (open position, close position, tick re-evaluation, rule
//! violation) emits a domain event.

use crate::core::account::{AccountSnapshot, AccountStatus};
use crate::core::ids::{AccountId, EventId, OrderId, PositionId};
use crate::core::order::OrderStatus;
use crate::core::position::PositionSide;
use crate::core::trade::Trade;
use crate::core::types::{Money, Symbol, Timestamp};
use crate::core::violation::Violation;

/// Type of domain event.
#[derive(Debug, Clone)]
pub enum DomainEventKind {
    /// Account opened / started.
    AccountStarted,
    /// Account status changed.
    AccountStatusChanged {
        from: AccountStatus,
        to: AccountStatus,
    },
    /// Account snapshot taken (e.g. on tick evaluation).
    AccountSnapshotted { snapshot: AccountSnapshot },
    /// Order lifecycle event.
    OrderEvent {
        order_id: OrderId,
        new_status: OrderStatus,
    },
    /// Trade fill recorded.
    TradeFilled { trade: Trade },
    /// Position opened.
    PositionOpened {
        position_id: PositionId,
        symbol: Symbol,
        side: PositionSide,
        qty: crate::core::types::Quantity,
    },
    /// Position closed.
    PositionClosed {
        position_id: PositionId,
        realized_pnl: Money,
    },
    /// Tick received and equity updated.
    TickEvaluated { equity: Money },
    /// New trading day rolled over.
    DayRollover {
        new_day_index: u32,
        day_start: Money,
    },
    /// Rule violation detected.
    RuleViolated { violation: Violation },
    /// Plan upgrade (Phase1 -> Phase2 -> Funded).
    PlanUpgraded {
        from_phase: crate::config::plan::ChallengePhase,
        to_phase: crate::config::plan::ChallengePhase,
    },
    /// **P1.10 fix**: liquidation requested. The bridge must close all
    /// listed positions immediately. Carries the full instruction
    /// (positions, reason, audit metadata) so the bridge has everything
    /// it needs to act — no extra lookup required.
    LiquidationRequested {
        instruction: crate::liquidation::LiquidationInstruction,
    },
}

/// A fully-timestamped domain event.
#[derive(Debug, Clone)]
pub struct DomainEvent {
    pub id: EventId,
    pub account_id: AccountId,
    pub kind: DomainEventKind,
    pub occurred_at: Timestamp,
    /// Causation id (the event that triggered this one, if any).
    pub causation_id: Option<EventId>,
}

impl DomainEvent {
    #[must_use]
    pub fn new(account_id: AccountId, kind: DomainEventKind, occurred_at: Timestamp) -> Self {
        DomainEvent {
            id: EventId::new(),
            account_id,
            kind,
            occurred_at,
            causation_id: None,
        }
    }

    #[must_use]
    pub fn with_causation(mut self, parent: EventId) -> Self {
        self.causation_id = Some(parent);
        self
    }
}
