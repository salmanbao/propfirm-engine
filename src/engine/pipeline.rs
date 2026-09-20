//! Pipeline: orchestrates state transitions, event emission, rule
//! evaluation, and decision execution.
//!
//! The pipeline is the high-level entry point used by the API server and
//! downstream integrations. It accepts a stream of [`PipelineEvent`]s and
//! produces [`PipelineResult`]s.

use crate::core::account::{Account, AccountStatus};
use crate::core::events::{DomainEvent, DomainEventKind};
use crate::core::order::Order;
use crate::core::position::Position;
use crate::core::tick::Tick;
use crate::core::trade::Trade;
use crate::core::types::{Money, Timestamp};
use crate::engine::decision::{Decision, DecisionKind};
use crate::engine::evaluator::Evaluator;
use crate::engine::snapshot::Snapshot;
use crate::engine::state::{equity_after_tick, AccountState};
use crate::events::store::EventStore;
use crate::notifications::traits::Notifier;
use crate::persistence::traits::AccountStore;

/// Inputs to the pipeline.
#[derive(Debug, Clone)]
pub enum PipelineEvent {
    /// Account opened / started.
    AccountStarted { at: Timestamp },
    /// A new order was submitted (pre-trade check).
    OrderSubmitted { order: Order },
    /// A trade has been filled (post-trade state update).
    TradeFilled { trade: Trade },
    /// A market tick arrived.
    Tick { tick: Tick },
    /// New trading day rollover.
    DayRollover { had_trades_today: bool },
    /// Manual end-of-day evaluation.
    EndOfDay,
    /// Manual on-demand evaluation.
    OnDemand,
}

/// The result of processing a pipeline event: snapshot + emitted events + rule-evaluation result.
#[derive(Debug, Clone)]
pub struct PipelineResult {
    /// Account snapshot at evaluation time.
    pub snapshot: Snapshot,
    /// Events emitted during processing.
    pub events: Vec<DomainEvent>,
    /// The evaluation result from the engine.
    pub result: crate::engine::evaluator::EvaluationResult,
}

impl PipelineResult {
    /// Returns true if all rules passed.
    pub fn passed(&self) -> bool { self.result.passed() }
    /// Returns true if any rule produced a terminating decision.
    pub fn failed(&self) -> bool { self.result.failed() }
}

/// The pipeline. Holds an evaluator and references to backing stores.
pub struct Pipeline<S, N>
where
    S: AccountStore,
    N: Notifier,
{
    pub evaluator: Evaluator,
    pub store: S,
    pub notifier: N,
    pub event_store: EventStore,
}

impl<S, N> Pipeline<S, N>
where
    S: AccountStore,
    N: Notifier,
{
    pub fn new(evaluator: Evaluator, store: S, notifier: N) -> Self {
        Pipeline {
            evaluator,
            store,
            notifier,
            event_store: EventStore::in_memory(),
        }
    }

    /// Processes a single pipeline event.
    pub fn process(&mut self, account_id: crate::core::ids::AccountId, ev: PipelineEvent) -> crate::Result<PipelineResult> {
        let account = self.store.get(account_id)?.ok_or_else(|| crate::Error::NotFound(format!("account {account_id}")))?;
        let state = AccountState::new(account);
        let mut events: Vec<DomainEvent> = Vec::new();

        // Apply state transitions.
        let (new_state, ctx_kind, open_positions, today_trades, recent_events) = self.apply_event(state, &ev, &mut events)?;
        // Build context
        let ctx = self.build_context(new_state.account.clone(), ctx_kind, &ev, &open_positions, &today_trades, recent_events);
        // Evaluate rules
        let result = self.evaluator.evaluate(&ctx)?;
        // Snapshot
        let snap = Snapshot::new(&new_state.account, result.decision.clone());
        // Persist updated account
        self.store.put(new_state.account.clone())?;
        // Emit decision event
        let _decision_event = self.emit_decision_event(account_id, &result.decision, &mut events);
        // Append events
        for e in &events {
            self.event_store.append(e.clone())?;
        }
        // Notify on violations
        for v in result.violations() {
            self.notifier.notify_violation(v)?;
        }
        Ok(PipelineResult {
            snapshot: snap,
            events,
            result,
        })
    }

    fn apply_event(&self, mut state: AccountState, ev: &PipelineEvent, events: &mut Vec<DomainEvent>) -> crate::Result<(AccountState, crate::rules::context::RuleContextKind, Vec<Position>, Vec<Trade>, Vec<DomainEvent>)> {
        use crate::rules::context::RuleContextKind::*;
        let mut open_positions = self.store.open_positions(state.account.id).unwrap_or_default();
        let today_trades = self.store.today_trades(state.account.id).unwrap_or_default();
        let recent_events = self.event_store.recent(state.account.id, 50);
        match ev {
            PipelineEvent::AccountStarted { at } => {
                let new_acc = state.account.clone().start(*at)?;
                events.push(DomainEvent::new(new_acc.id, DomainEventKind::AccountStarted, *at));
                state.account = new_acc;
                Ok((state, OnDemand, open_positions, today_trades, recent_events))
            }
            PipelineEvent::OrderSubmitted { order } => {
                Ok((state, OnOrderSubmit, open_positions, today_trades, recent_events))
            }
            PipelineEvent::TradeFilled { trade } => {
                // Apply realized P&L on exits
                let (pnl, commission, swap) = match trade.exit_info.as_ref() {
                    Some(info) => (info.realized_pnl, trade.commission, trade.swap),
                    None => (Money::ZERO, trade.commission, trade.swap),
                };
                let new_state = state.apply_realized_pnl(pnl, commission, swap, trade.executed_at);
                events.push(DomainEvent::new(
                    new_state.account.id,
                    DomainEventKind::TradeFilled { trade: trade.clone() },
                    trade.executed_at,
                ));
                Ok((new_state, OnTradeFill, open_positions, today_trades, recent_events))
            }
            PipelineEvent::Tick { tick } => {
                let equity = equity_after_tick(state.account.balance, &open_positions, &tick.quote);
                let new_state = state.update_equity(equity);
                events.push(DomainEvent::new(
                    new_state.account.id,
                    DomainEventKind::TickEvaluated { equity },
                    tick.quote.ts,
                ));
                Ok((new_state, OnTick, open_positions, today_trades, recent_events))
            }
            PipelineEvent::DayRollover { had_trades_today } => {
                let new_state = state.rollover_day(*had_trades_today);
                events.push(DomainEvent::new(
                    new_state.account.id,
                    DomainEventKind::DayRollover { new_day_index: new_state.account.trading_day_index, day_start: new_state.account.day_start_balance },
                    chrono::Utc::now(),
                ));
                Ok((new_state, OnDayRollover, open_positions, today_trades, recent_events))
            }
            PipelineEvent::EndOfDay => Ok((state, OnEndOfDay, open_positions, today_trades, recent_events)),
            PipelineEvent::OnDemand => Ok((state, OnDemand, open_positions, today_trades, recent_events)),
        }
    }

    fn build_context(&self, account: Account, kind: crate::rules::context::RuleContextKind, ev: &PipelineEvent, open_positions: &[Position], today_trades: &[Trade], recent_events: Vec<DomainEvent>) -> crate::rules::context::RuleContext {
        use crate::rules::context::RuleContext;
        let mut ctx = RuleContext::new(account);
        ctx.kind = kind;
        ctx.open_positions = open_positions.to_vec();
        ctx.today_trades = today_trades.to_vec();
        ctx.recent_events = recent_events;
        ctx.rule_config = crate::config::rule_config::RuleConfig::from_plan(&ctx.account.plan);
        match ev {
            PipelineEvent::OrderSubmitted { order } => { ctx.pending_order = Some(order.clone()); }
            PipelineEvent::TradeFilled { trade } => { ctx.latest_trade = Some(trade.clone()); }
            PipelineEvent::Tick { tick } => { ctx.latest_tick = Some(tick.clone()); }
            _ => {}
        }
        ctx
    }

    fn emit_decision_event(&self, account_id: crate::core::ids::AccountId, decision: &Decision, events: &mut Vec<DomainEvent>) -> Option<DomainEvent> {
        if let DecisionKind::Fail | DecisionKind::Liquidate = decision.kind {
            let ev = DomainEvent::new(
                account_id,
                DomainEventKind::AccountStatusChanged {
                    from: AccountStatus::Active,
                    to: AccountStatus::Failed,
                },
                chrono::Utc::now(),
            );
            events.push(ev.clone());
            Some(ev)
        } else {
            None
        }
    }
}
