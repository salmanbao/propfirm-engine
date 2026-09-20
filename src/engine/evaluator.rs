//! The rule evaluator. Wraps a [`RuleRegistry`] and provides high-level
//! `evaluate_*` methods for each context kind.

use crate::core::account::Account;
use crate::core::events::{DomainEvent, DomainEventKind};
use crate::core::ids::{AccountId, EventId};
use crate::core::order::Order;
use crate::core::tick::Tick;
use crate::core::trade::Trade;
use crate::rules::context::{RuleContext, RuleContextKind};
use crate::rules::registry::RuleRegistry;
use crate::rules::traits::RuleReport;

/// The evaluator combines a registry with the challenge plan and produces
/// rule reports and decisions.
#[derive(Clone)]
pub struct Evaluator {
    pub registry: RuleRegistry,
    account_id: AccountId,
}

impl Evaluator {
    /// Constructs a new evaluator with the default rule library.
    pub fn new(_plan: crate::config::plan::ChallengePlan) -> Self {
        Evaluator {
            registry: RuleRegistry::with_default_rules(),
            account_id: AccountId::new(),
        }
    }

    /// Constructs an evaluator with a custom registry.
    pub fn with_registry(registry: RuleRegistry) -> Self {
        Evaluator {
            registry,
            account_id: AccountId::new(),
        }
    }

    /// Associates a specific account id with this evaluator.
    pub fn for_account(mut self, id: AccountId) -> Self {
        self.account_id = id;
        self
    }

    /// Evaluates all rules against the given context. Returns reports and
    /// a final [`Decision`](crate::engine::decision::Decision).
    pub fn evaluate(&self, ctx: &RuleContext) -> crate::Result<EvaluationResult> {
        let reports = self.registry.evaluate(ctx)?;
        let decision = crate::engine::decision::Decision::from_reports(&reports);
        Ok(EvaluationResult { reports, decision })
    }

    /// Convenience: evaluate a pending order (pre-trade).
    pub fn evaluate_order(&self, account: &Account, order: &Order, open_positions: &[crate::core::position::Position], today_trades: &[Trade], recent_events: Vec<DomainEvent>) -> crate::Result<EvaluationResult> {
        let mut ctx = RuleContext::for_open_order(account.clone(), order);
        ctx.open_positions = open_positions.to_vec();
        ctx.today_trades = today_trades.to_vec();
        ctx.recent_events = recent_events;
        self.evaluate(&ctx)
    }

    /// Convenience: evaluate a trade fill.
    pub fn evaluate_trade(&self, account: &Account, trade: &Trade, open_positions: &[crate::core::position::Position], today_trades: &[Trade], recent_events: Vec<DomainEvent>) -> crate::Result<EvaluationResult> {
        let mut ctx = RuleContext::for_trade_fill(account.clone(), trade);
        ctx.open_positions = open_positions.to_vec();
        ctx.today_trades = today_trades.to_vec();
        ctx.recent_events = recent_events;
        self.evaluate(&ctx)
    }

    /// Convenience: evaluate a market tick.
    pub fn evaluate_tick(&self, account: &Account, tick: &Tick, open_positions: &[crate::core::position::Position], today_trades: &[Trade], recent_events: Vec<DomainEvent>) -> crate::Result<EvaluationResult> {
        let mut ctx = RuleContext::for_tick(account.clone(), tick);
        ctx.open_positions = open_positions.to_vec();
        ctx.today_trades = today_trades.to_vec();
        ctx.recent_events = recent_events;
        self.evaluate(&ctx)
    }

    /// Convenience: evaluate on day rollover.
    pub fn evaluate_day_rollover(&self, account: &Account) -> crate::Result<EvaluationResult> {
        let ctx = RuleContext::for_day_rollover(account.clone());
        self.evaluate(&ctx)
    }
}

/// The result of evaluating a context: rule reports + a final decision.
#[derive(Debug, Clone)]
pub struct EvaluationResult {
    pub reports: Vec<RuleReport>,
    pub decision: crate::engine::decision::Decision,
}

impl EvaluationResult {
    pub fn passed(&self) -> bool {
        self.decision.is_pass()
    }
    pub fn failed(&self) -> bool {
        self.decision.is_terminating()
    }
    pub fn violations(&self) -> Vec<&crate::core::violation::Violation> {
        self.reports.iter().filter_map(|r| r.verdict.violation()).collect()
    }
}

/// Builds a domain event for a rule violation, suitable for the audit log.
pub fn make_violation_event(account_id: AccountId, violation: &crate::core::violation::Violation, causation: Option<EventId>) -> DomainEvent {
    let mut ev = DomainEvent::new(
        account_id,
        DomainEventKind::RuleViolated { violation: violation.clone() },
        violation.occurred_at,
    );
    if let Some(c) = causation {
        ev = ev.with_causation(c);
    }
    ev
}

/// Converts a context kind into a string label for logging.
pub fn context_label(kind: RuleContextKind) -> &'static str {
    use RuleContextKind::*;
    match kind {
        OnOrderSubmit => "order_submit",
        OnTradeFill => "trade_fill",
        OnTick => "tick",
        OnDayRollover => "day_rollover",
        OnEndOfDay => "end_of_day",
        OnDemand => "on_demand",
    }
}
