//! Common prelude for the prop firm engine.
//!
//! Importing `propfirm::prelude::*` brings the most-used types into scope so
//! downstream code can avoid long paths.

pub use crate::core::{
    account::{Account, AccountSnapshot, AccountStatus, AccountType},
    ids::{
        AccountId, ChallengeId, EventId, OrderId, PositionId, RuleId, SessionId, TradeId,
        ViolationId,
    },
    order::{Order, OrderKind, OrderSide, OrderStatus, OrderType, TimeInForce},
    position::{unrealized_pnl, Position, PositionSide, PositionStatus},
    tick::{Quote, Tick},
    trade::{Trade, TradeExit, TradeSide},
    types::{
        dec, Date, Decimal, Duration, Leverage, Lots, Money, Pct, Price, Quantity, ServerTime,
        Symbol, Time, Timestamp,
    },
    violation::{Violation, ViolationKind, ViolationSeverity},
    Error,
};

pub use crate::config::{
    plan::{ChallengePhase, ChallengePlan, PlanMeta},
    rule_config::RuleConfig,
};

pub use crate::rules::{
    context::{EvaluationScope, RuleContext, RuleContextKind},
    registry::RuleRegistry,
    traits::{Rule, RuleReport, RuleVerdict},
};

pub use crate::engine::{
    decision::{Decision, DecisionKind, DecisionReason},
    evaluator::Evaluator,
    pipeline::{Pipeline, PipelineEvent, PipelineResult},
    snapshot::Snapshot,
    state::{AccountState, StateDelta},
};

pub use crate::events::types::{DomainEvent, DomainEventKind, EventId as EventIdT};
pub use crate::notifications::traits::Notifier;
pub use crate::reporting::report::PerformanceReport;
pub use crate::risk::metrics::RiskMetrics;
