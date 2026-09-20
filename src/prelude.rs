//! Common prelude for the prop firm engine.
//!
//! Importing `propfirm::prelude::*` brings the most-used types into scope so
//! downstream code can avoid long paths.

pub use crate::core::{
    account::{Account, AccountStatus, AccountType, AccountSnapshot},
    ids::{RuleId, TradeId, OrderId, PositionId, AccountId, ChallengeId, EventId, ViolationId, SessionId},
    order::{Order, OrderKind, OrderSide, OrderStatus, OrderType, TimeInForce},
    position::{Position, PositionSide, PositionStatus, unrealized_pnl},
    tick::{Tick, Quote},
    trade::{Trade, TradeSide, TradeExit},
    types::{
        Decimal, Money, Price, Quantity, Lots, Pct, Timestamp, Duration,
        Symbol, Leverage, ServerTime, Date, Time, dec,
    },
    violation::{Violation, ViolationKind, ViolationSeverity},
    Error,
};

pub use crate::config::{
    plan::{ChallengePlan, ChallengePhase, PlanMeta},
    rule_config::RuleConfig,
};

pub use crate::rules::{
    context::{RuleContext, RuleContextKind, EvaluationScope},
    registry::RuleRegistry,
    traits::{Rule, RuleOutcome, RuleVerdict, RuleReport},
    outcome::Outcome,
};

pub use crate::engine::{
    evaluator::Evaluator,
    pipeline::{Pipeline, PipelineEvent, PipelineResult},
    snapshot::Snapshot,
    decision::{Decision, DecisionKind, DecisionReason},
    state::{AccountState, StateDelta},
};

pub use crate::risk::metrics::RiskMetrics;
pub use crate::events::types::{DomainEvent, DomainEventKind, EventId as EventIdT};
pub use crate::notifications::traits::Notifier;
pub use crate::reporting::report::PerformanceReport;

#[doc(hidden)]
pub use rust_decimal_macros::*;
