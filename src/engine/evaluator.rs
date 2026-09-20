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
    ///
    /// **P0.2 fix**: the plan is no longer ignored. The registry is
    /// built so that rules the plan disables are not registered at all
    /// (defense in depth on top of each rule's own `is_enabled`):
    ///
    /// - `per_trade_max_loss` registered only if the plan sets
    ///   `per_trade_max_loss_pct` / `per_trade_max_loss_money`.
    /// - `hft_scalping` registered only if `plan.hft_ban_enabled`.
    /// - `inactivity` registered only if `plan.inactivity_days` is set.
    ///
    /// Always-on rules (drawdown, profit target, time limit, etc.) are
    /// registered unconditionally and gate themselves via `is_enabled`.
    /// Callers that supply an explicit registry (the rule-pack path)
    /// keep using [`Self::with_registry`].
    #[must_use]
    pub fn new(plan: &crate::config::plan::ChallengePlan) -> Self {
        Evaluator {
            registry: RuleRegistry::with_default_rules_for_plan(plan),
            account_id: AccountId::new(),
        }
    }

    /// Constructs an evaluator with a custom registry.
    #[must_use]
    pub fn with_registry(registry: RuleRegistry) -> Self {
        Evaluator {
            registry,
            account_id: AccountId::new(),
        }
    }

    /// Associates a specific account id with this evaluator.
    #[must_use]
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
    pub fn evaluate_order(
        &self,
        account: &Account,
        order: &Order,
        open_positions: &[crate::core::position::Position],
        today_trades: &[Trade],
        recent_events: Vec<DomainEvent>,
    ) -> crate::Result<EvaluationResult> {
        let mut ctx = RuleContext::for_open_order(account.clone(), order);
        ctx.open_positions = open_positions.to_vec();
        ctx.today_trades = today_trades.to_vec();
        ctx.recent_events = recent_events;
        self.evaluate(&ctx)
    }

    /// Convenience: evaluate a trade fill.
    pub fn evaluate_trade(
        &self,
        account: &Account,
        trade: &Trade,
        open_positions: &[crate::core::position::Position],
        today_trades: &[Trade],
        recent_events: Vec<DomainEvent>,
    ) -> crate::Result<EvaluationResult> {
        let mut ctx = RuleContext::for_trade_fill(account.clone(), trade);
        ctx.open_positions = open_positions.to_vec();
        ctx.today_trades = today_trades.to_vec();
        ctx.recent_events = recent_events;
        self.evaluate(&ctx)
    }

    /// Convenience: evaluate a market tick. The equity/balance on the
    /// `account` are treated as broker-reported (P1-5) — only call this
    /// helper when you're providing the broker's actual equity. For
    /// estimate-only paths, use `evaluate_tick_estimated` instead.
    pub fn evaluate_tick(
        &self,
        account: &Account,
        tick: &Tick,
        open_positions: &[crate::core::position::Position],
        today_trades: &[Trade],
        recent_events: Vec<DomainEvent>,
    ) -> crate::Result<EvaluationResult> {
        let mut ctx = RuleContext::for_tick(account.clone(), tick);
        ctx.open_positions = open_positions.to_vec();
        ctx.today_trades = today_trades.to_vec();
        ctx.recent_events = recent_events;
        // P1-5: caller-provided account.equity is treated as broker-reported.
        ctx = ctx.with_broker_equity(account.equity, account.balance);
        self.evaluate(&ctx)
    }

    /// Convenience: evaluate a market tick where the equity is an
    /// *estimate* (not broker-reported). Breach-capable rules will refuse
    /// to terminate on this context (P1-5).
    pub fn evaluate_tick_estimated(
        &self,
        account: &Account,
        tick: &Tick,
        open_positions: &[crate::core::position::Position],
        today_trades: &[Trade],
        recent_events: Vec<DomainEvent>,
    ) -> crate::Result<EvaluationResult> {
        let mut ctx = RuleContext::for_tick(account.clone(), tick);
        ctx.open_positions = open_positions.to_vec();
        ctx.today_trades = today_trades.to_vec();
        ctx.recent_events = recent_events;
        ctx = ctx.with_estimated_equity(account.equity, account.balance);
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
    #[must_use]
    pub fn passed(&self) -> bool {
        self.decision.is_pass()
    }
    #[must_use]
    pub fn failed(&self) -> bool {
        self.decision.is_terminating()
    }
    #[must_use]
    pub fn violations(&self) -> Vec<&crate::core::violation::Violation> {
        self.reports
            .iter()
            .filter_map(|r| r.verdict.violation())
            .collect()
    }
}

/// Builds a domain event for a rule violation, suitable for the audit log.
#[must_use]
pub fn make_violation_event(
    account_id: AccountId,
    violation: &crate::core::violation::Violation,
    causation: Option<EventId>,
) -> DomainEvent {
    let mut ev = DomainEvent::new(
        account_id,
        DomainEventKind::RuleViolated {
            violation: violation.clone(),
        },
        violation.occurred_at,
    );
    if let Some(c) = causation {
        ev = ev.with_causation(c);
    }
    ev
}

/// Converts a context kind into a string label for logging.
#[must_use]
pub fn context_label(kind: RuleContextKind) -> &'static str {
    use RuleContextKind::{
        OnDayRollover, OnDemand, OnEndOfDay, OnOrderSubmit, OnTick, OnTradeFill,
    };
    match kind {
        OnOrderSubmit => "order_submit",
        OnTradeFill => "trade_fill",
        OnTick => "tick",
        OnDayRollover => "day_rollover",
        OnEndOfDay => "end_of_day",
        OnDemand => "on_demand",
    }
}
