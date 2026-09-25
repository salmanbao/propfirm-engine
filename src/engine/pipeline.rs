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
use crate::core::types::{Money, Quantity, Timestamp};
use crate::engine::decision::{Decision, DecisionKind};
use crate::engine::evaluator::Evaluator;
use crate::engine::snapshot::Snapshot;
use crate::engine::state::{equity_after_tick, AccountState};
use crate::equity_input::EquityInput;
use crate::notifications::traits::Notifier;
use crate::persistence::traits::AccountStore;
use std::sync::Arc;

/// **P1-14 fix**: a tick older than this many minutes from wall-clock "now"
/// is rejected before evaluation runs, regardless of whether it would have
/// produced a breach. The binding spec reference value is 10 minutes.
pub const STALE_TICK_THRESHOLD_MINUTES: i64 = 10;

/// Output of [`Pipeline::apply_event`]: the mutated state plus the
/// pre-fetched collections the rule context is built from.
pub struct AppliedEvent {
    /// The account state after the event's state transition was applied.
    pub state: AccountState,
    /// Which rule-context kind this event maps to.
    pub ctx_kind: crate::rules::context::RuleContextKind,
    /// Open positions fetched from the store before evaluation.
    pub open_positions: Vec<Position>,
    /// Today's trades fetched from the store before evaluation.
    pub today_trades: Vec<Trade>,
    /// Recent domain events fetched from the event store.
    pub recent_events: Vec<DomainEvent>,
    /// Trade fill data for atomic ingestion (only Some for TradeFilled events)
    pub trade_fill: Option<(Trade, Position)>,
}

/// Inputs to the pipeline.
#[derive(Debug, Clone)]
pub enum PipelineEvent {
    /// Account opened / started.
    AccountStarted { at: Timestamp },
    /// A new order was submitted (pre-trade check).
    OrderSubmitted { order: Order },
    /// A trade has been filled (post-trade state update).
    TradeFilled { trade: Trade },
    /// **P1-5 fix**: a market tick arrived, carrying the **broker-reported**
    /// equity/balance (the only values that can drive a `Fail`/`Liquidate`
    /// verdict). The engine does NOT recompute equity from positions + quote.
    /// If only a quote is available (no broker equity), use
    /// [`PipelineEvent::TickEstimated`] instead — breach-capable rules will
    /// downgrade their verdicts to `Warn` at most.
    Tick {
        tick: Tick,
        /// Broker-reported equity (balance + unrealized P&L per the broker).
        broker_equity: crate::core::types::Money,
        /// Broker-reported balance (cash, no floating P&L).
        broker_balance: crate::core::types::Money,
    },
    /// **P1-5 fix**: a market tick arrived but the broker did not report
    /// equity on this tick (e.g. an interim quote between sync windows).
    /// The engine estimates equity from positions + quote, but breach-capable
    /// rules will refuse to terminate on the estimate.
    TickEstimated { tick: Tick },
    /// New trading day rollover.
    DayRollover { had_trades_today: bool },
    /// Manual end-of-day evaluation.
    EndOfDay,
    /// Manual on-demand evaluation.
    OnDemand,
    /// **P1-12 fix**: emergency stop. Short-circuits normal rule evaluation
    /// and forces `DecisionKind::Emergency` with full audit metadata
    /// (reason + `actor_id`). Used for disaster-response scenarios — e.g.
    /// a broker feed is clearly corrupted and every account needs to
    /// freeze immediately.
    EmergencyStop {
        reason: String,
        actor_id: String,
        at: Timestamp,
    },
    /// **P1-11 fix**: manual override clearing a false-positive breach.
    /// Reverts the account from `Failed`/`EmergencyStopped` back to
    /// `Active`. The original violation stays in the audit log; the
    /// override is recorded alongside it as the rebuttal.
    OverrideBreach {
        override_record: crate::override_engine::Override,
    },
    /// **§D.2 fix**: a payout request. Evaluates the payout via the
    /// plan's [`PayoutConfig`](crate::payout::PayoutConfig); a permitted
    /// request transitions the account to `PayoutPending` and emits
    /// `PayoutRequested`; a rejected request returns an error carrying
    /// the reason.
    PayoutRequest,
    /// **§D.2 fix**: approval of a pending payout. Executes the payout
    /// (balance deduction, watermark/tier bookkeeping), resolves
    /// `PayoutPending` → `Funded`, and emits `PayoutApproved`.
    PayoutApprove,
}

impl PipelineEvent {
    /// Returns the wall-clock timestamp associated with this event.
    /// Used by the auto-rollover check to determine whether the event
    /// crosses a trading day boundary.
    pub fn event_timestamp(&self) -> Timestamp {
        match self {
            PipelineEvent::AccountStarted { at } => *at,
            PipelineEvent::OrderSubmitted { order } => order.submitted_at,
            PipelineEvent::TradeFilled { trade } => trade.executed_at,
            PipelineEvent::Tick { tick, .. } => tick.quote.ts,
            PipelineEvent::TickEstimated { tick } => tick.quote.ts,
            PipelineEvent::DayRollover { .. } => chrono::Utc::now(),
            PipelineEvent::EndOfDay => chrono::Utc::now(),
            PipelineEvent::OnDemand => chrono::Utc::now(),
            PipelineEvent::EmergencyStop { at, .. } => *at,
            PipelineEvent::OverrideBreach { override_record } => override_record.at,
            // Payout events use wall-clock now (they are ops-initiated).
            PipelineEvent::PayoutRequest | PipelineEvent::PayoutApprove => chrono::Utc::now(),
        }
    }
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
    #[must_use]
    pub fn passed(&self) -> bool {
        self.result.passed()
    }
    /// Returns true if any rule produced a terminating decision.
    #[must_use]
    pub fn failed(&self) -> bool {
        self.result.failed()
    }
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
    pub event_store: Arc<dyn crate::events::store::EventStore>,
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
            event_store: Arc::new(crate::events::store::InMemoryEventStore::new()),
        }
    }

    /// Processes a single pipeline event.
    ///
    /// **P0-E fix**: reads the account (`account_id` is globally unique —
    /// `UUIDv4`), then writes via `put_with_version` (optimistic
    /// concurrency — a concurrent writer between our read and write
    /// produces `Error::StateConflict`, which the caller must retry).
    /// The pipeline is no longer the version owner; the store is — so
    /// there's no double-increment.
    ///
    /// For strict tenant-scoped reads (e.g. when the caller doesn't
    /// trust the `account_id` to be globally unique within their store),
    /// use [`process_for_tenant`](Self::process_for_tenant) instead.
    pub async fn process(
        &mut self,
        account_id: crate::core::ids::AccountId,
        ev: PipelineEvent,
    ) -> crate::Result<PipelineResult> {
        let account = self
            .store
            .get(account_id)
            .await?
            .ok_or_else(|| crate::Error::NotFound(format!("account {account_id}")))?;
        let tenant_id = account.tenant_id;
        self.process_with_loaded_account(account, tenant_id, ev)
            .await
    }

    /// **P0-E fix**: tenant-scoped process. Reads via `get_for_tenant` so
    /// cross-tenant data leakage is impossible at the storage layer.
    /// Writes via `put_with_version(expected_version)` so a concurrent
    /// writer between our read and write produces `Error::StateConflict`,
    /// which the caller must retry.
    pub async fn process_for_tenant(
        &mut self,
        tenant_id: crate::tenant::TenantId,
        account_id: crate::core::ids::AccountId,
        ev: PipelineEvent,
    ) -> crate::Result<PipelineResult> {
        let account = self
            .store
            .get_for_tenant(tenant_id, account_id)
            .await?
            .ok_or_else(|| {
                crate::Error::NotFound(format!(
                    "account {account_id} not found for tenant {tenant_id}"
                ))
            })?;
        self.process_with_loaded_account(account, tenant_id, ev)
            .await
    }

    /// Common path for `process` and `process_for_tenant` once the
    /// account is loaded. Owns the OCC write.
    /// Returns the next phase for a successful phase completion.
    fn next_phase_for(
        phase: &crate::config::plan::ChallengePhase,
    ) -> crate::config::plan::ChallengePhase {
        use crate::config::plan::ChallengePhase::{Funded, Phase1, Phase2};
        match phase {
            Phase1 => Phase2,
            Phase2 => Funded,
            Funded => Phase1, // unreachable in practice; keeps the match exhaustive
        }
    }

    async fn process_with_loaded_account(
        &mut self,
        account: Account,
        _tenant_id: crate::tenant::TenantId,
        ev: PipelineEvent,
    ) -> crate::Result<PipelineResult> {
        let account_id = account.id;
        let expected_version = account.version;
        let mut state = AccountState::new(account);
        let mut events: Vec<DomainEvent> = Vec::new();

        if !matches!(ev, PipelineEvent::DayRollover { .. }) {
            let event_ts = ev.event_timestamp();
            let event_day_start = state.account.plan.trading_day_start(event_ts);
            let current_day_start = state
                .account
                .current_trading_day_start
                .unwrap_or_else(|| state.account.plan.trading_day_start(event_ts));
            if event_day_start > current_day_start {
                let rollover_ts = event_ts;
                let mut first_rollover = true;
                while state
                    .account
                    .current_trading_day_start
                    .map(|start| start < event_day_start)
                    .unwrap_or(true)
                {
                    let had_trades = if first_rollover {
                        !state.account.today_realized_pnl.0.is_zero()
                    } else {
                        false
                    };
                    first_rollover = false;
                    let next_start = state
                        .account
                        .plan
                        .next_trading_day_start(state.account.current_trading_day_start.unwrap());
                    if next_start >= event_day_start {
                        state = state.rollover_day(had_trades, Some(event_ts));
                    } else {
                        state = state.rollover_day(had_trades, Some(next_start));
                    }
                }
                events.push(DomainEvent::new(
                    account_id,
                    DomainEventKind::DayRollover {
                        new_day_index: state.account.trading_day_index,
                        day_start: state.account.day_start_balance,
                    },
                    rollover_ts,
                ));
            }
        }

        let applied = self.apply_event(state, &ev, &mut events).await?;
        let (new_state, ctx_kind, open_positions, today_trades, recent_events, trade_fill) = (
            applied.state,
            applied.ctx_kind,
            applied.open_positions,
            applied.today_trades,
            applied.recent_events,
            applied.trade_fill,
        );
        let ctx = self.build_context(
            new_state.account.clone(),
            ctx_kind,
            &ev,
            &open_positions,
            &today_trades,
            recent_events,
        );
        let result = self.evaluator.evaluate(&ctx)?;
        let mut final_state = new_state;
        if result.decision.is_target_hit() {
            final_state = final_state.mark_target_reached(ctx.server_time.ts());
            events.push(DomainEvent::new(
                account_id,
                DomainEventKind::AccountStatusChanged {
                    from: crate::core::account::AccountStatus::Active,
                    to: crate::core::account::AccountStatus::TargetHitPending,
                },
                ctx.server_time.ts(),
            ));
        }
        if final_state.account.target_reached_at.is_some()
            && final_state.account.status == crate::core::account::AccountStatus::TargetHitPending
            && final_state.account.active_trading_days >= final_state.account.plan.min_trading_days
        {
            final_state.account.status = crate::core::account::AccountStatus::Passed;
            events.push(DomainEvent::new(
                account_id,
                DomainEventKind::AccountStatusChanged {
                    from: crate::core::account::AccountStatus::TargetHitPending,
                    to: crate::core::account::AccountStatus::Passed,
                },
                ctx.server_time.ts(),
            ));
            let from_phase = final_state.account.plan.phase;
            let to_phase = Self::next_phase_for(&from_phase);
            if let Ok(upped) = final_state.clone().upgrade_phase(to_phase) {
                final_state = upped;
                events.push(DomainEvent::new(
                    account_id,
                    DomainEventKind::PlanUpgraded {
                        from_phase,
                        to_phase,
                    },
                    ctx.server_time.ts(),
                ));
            }
        }
        if let Some(target_status) = result.decision.account_status_target() {
            let from = final_state.account.status;
            final_state.account.status = target_status;
            events.push(DomainEvent::new(
                account_id,
                DomainEventKind::AccountStatusChanged {
                    from,
                    to: target_status,
                },
                ctx.server_time.ts(),
            ));
            if matches!(
                result.decision.kind,
                crate::engine::decision::DecisionKind::Liquidate
                    | crate::engine::decision::DecisionKind::Emergency
            ) {
                let reason = match result.decision.kind {
                    crate::engine::decision::DecisionKind::Liquidate => {
                        let violation = result
                            .decision
                            .all_violations
                            .iter()
                            .find(|v| {
                                v.severity >= crate::core::violation::ViolationSeverity::Liquidate
                            })
                            .or_else(|| result.decision.all_violations.first());
                        let kind = violation
                            .map_or(crate::core::violation::ViolationKind::Custom, |v| v.kind);
                        crate::liquidation::LiquidationReason::RuleBreach(kind)
                    }
                    crate::engine::decision::DecisionKind::Emergency => {
                        crate::liquidation::LiquidationReason::EmergencyStop
                    }
                    _ => crate::liquidation::LiquidationReason::Manual,
                };
                let triggered_by = result.decision.all_violations.first().map(|v| v.id);
                let actor_id = match &ev {
                    PipelineEvent::EmergencyStop { actor_id, .. } => actor_id.clone(),
                    _ => "rule_engine".to_string(),
                };
                let instruction = crate::liquidation::LiquidationInstruction::new(
                    account_id,
                    final_state.account.tenant_id,
                    &open_positions,
                    reason,
                    triggered_by,
                    actor_id,
                    ctx.server_time.ts(),
                );
                events.push(DomainEvent::new(
                    account_id,
                    DomainEventKind::LiquidationRequested { instruction },
                    ctx.server_time.ts(),
                ));
            }
        }
        match &ev {
            PipelineEvent::Tick { tick, .. } | PipelineEvent::TickEstimated { tick } => {
                final_state.account.last_tick_ts = Some(tick.quote.ts);
            }
            _ => {}
        }
        let snap = Snapshot::new(&final_state.account, result.decision.clone());
        let _decision_event = self.emit_decision_event(account_id, &result.decision, &mut events);
        for v in result.violations() {
            events.push(DomainEvent::new(
                account_id,
                DomainEventKind::RuleViolated {
                    violation: v.clone(),
                },
                ctx.server_time.ts(),
            ));
        }
        if let Some((trade, position)) = trade_fill {
            self.store
                .ingest_trade_fill(
                    trade,
                    position,
                    final_state.account.clone(),
                    expected_version,
                    &events,
                )
                .await?;
        } else {
            self.store
                .put_with_version_and_events(final_state.account.clone(), expected_version, &events)
                .await?;
        }
        for v in result.violations() {
            self.notifier.notify_violation(v)?;
        }
        Ok(PipelineResult {
            snapshot: snap,
            events,
            result,
        })
    }

    async fn apply_event(
        &self,
        mut state: AccountState,
        ev: &PipelineEvent,
        events: &mut Vec<DomainEvent>,
    ) -> crate::Result<AppliedEvent> {
        use crate::rules::context::RuleContextKind::{
            OnDayRollover, OnDemand, OnEndOfDay, OnOrderSubmit, OnTick, OnTradeFill,
        };
        let open_positions = self.store.open_positions(state.account.id).await?;
        let event_ts = ev.event_timestamp();
        let day_start = state.account.plan.trading_day_start(event_ts);
        let today_trades = self
            .store
            .today_trades_since(state.account.id, day_start)
            .await?;
        let recent_events = self.event_store.recent(state.account.id, 50).await?;
        match ev {
            PipelineEvent::AccountStarted { at } => {
                let new_acc = state.account.clone().start(*at)?;
                events.push(DomainEvent::new(
                    new_acc.id,
                    DomainEventKind::AccountStarted,
                    *at,
                ));
                state.account = new_acc;
                Ok(AppliedEvent {
                    state,
                    ctx_kind: OnDemand,
                    open_positions,
                    today_trades,
                    recent_events,
                    trade_fill: None,
                })
            }
            PipelineEvent::OrderSubmitted { order: _ } => Ok(AppliedEvent {
                state,
                ctx_kind: OnOrderSubmit,
                open_positions,
                today_trades,
                recent_events,
                trade_fill: None,
            }),
            PipelineEvent::TradeFilled { trade } => {
                // Apply realized P&L on exits
                let (pnl, commission, swap) = match trade.exit_info.as_ref() {
                    Some(info) => (info.realized_pnl, trade.commission, trade.swap),
                    None => (Money::ZERO, trade.commission, trade.swap),
                };
                let new_state = state
                    .apply_realized_pnl(pnl, commission, swap, trade.executed_at)
                    .mark_active_trading_day();

                // Broker fill ingestion data - will be persisted atomically
                // via ingest_trade_fill in process_with_loaded_account after rule evaluation.
                // This avoids persisting the fill before we know the evaluation result.
                let position = match trade.trade_side {
                    crate::core::trade::TradeSide::Entry => Position::open(
                        trade.account_id,
                        trade.symbol.clone(),
                        crate::core::position::PositionSide::from_order(trade.side),
                        trade.price,
                        trade.quantity,
                        trade.executed_at,
                        trade.commission,
                        None,
                        None,
                        None,
                        trade.comment.clone(),
                    ),
                    crate::core::trade::TradeSide::Exit => {
                        if let Some(exit_info) = &trade.exit_info {
                            if let Some(mut pos) = open_positions
                                .iter()
                                .find(|p| p.id == exit_info.position_id)
                                .cloned()
                            {
                                pos.status = crate::core::position::PositionStatus::Closed;
                                pos.closed_at = Some(trade.executed_at);
                                pos.realized_pnl = exit_info.realized_pnl;
                                pos.open_quantity = Quantity::ZERO;
                                pos
                            } else {
                                // Synthetic closed position for tests/external fills where the
                                // original position is not tracked in this store.
                                Position {
                                    id: exit_info.position_id,
                                    account_id: trade.account_id,
                                    symbol: trade.symbol.clone(),
                                    side: crate::core::position::PositionSide::from_order(
                                        trade.side,
                                    ),
                                    opened_at: trade.executed_at,
                                    closed_at: Some(trade.executed_at),
                                    status: crate::core::position::PositionStatus::Closed,
                                    avg_entry_price: exit_info.entry_price,
                                    opened_quantity: exit_info.closed_quantity,
                                    open_quantity: Quantity::ZERO,
                                    realized_pnl: exit_info.realized_pnl,
                                    commission: trade.commission,
                                    swap: trade.swap,
                                    stop_loss: None,
                                    take_profit: None,
                                    magic: None,
                                    comment: trade.comment.clone(),
                                }
                            }
                        } else {
                            return Err(crate::Error::Persistence(
                                "exit trade missing exit_info".to_string(),
                            ));
                        }
                    }
                    _ => Position::open(
                        trade.account_id,
                        trade.symbol.clone(),
                        crate::core::position::PositionSide::from_order(trade.side),
                        trade.price,
                        trade.quantity,
                        trade.executed_at,
                        trade.commission,
                        None,
                        None,
                        None,
                        trade.comment.clone(),
                    ),
                };

                events.push(DomainEvent::new(
                    new_state.account.id,
                    DomainEventKind::TradeFilled {
                        trade: trade.clone(),
                    },
                    trade.executed_at,
                ));
                Ok(AppliedEvent {
                    state: new_state,
                    ctx_kind: OnTradeFill,
                    open_positions,
                    today_trades,
                    recent_events,
                    trade_fill: Some((trade.clone(), position)),
                })
            }
            PipelineEvent::Tick {
                tick,
                broker_equity,
                broker_balance,
            } => {
                // P1-14: stale-tick guard — skip evaluation if tick is older
                // than 10 minutes from wall-clock "now". A stale equity value
                // must never drive a breach decision.
                let now = chrono::Utc::now();
                let staleness = now - tick.quote.ts;
                if staleness.num_minutes() > STALE_TICK_THRESHOLD_MINUTES {
                    return Err(crate::Error::TickRejected(format!(
                        "tick ts {} is {} minutes old (threshold {}m) — refusing to evaluate stale equity",
                        tick.quote.ts, staleness.num_minutes(), STALE_TICK_THRESHOLD_MINUTES
                    )));
                }
                // P1-14: out-of-order-tick guard — skip if older than the
                // last-evaluated tick for this account (replay protection).
                if let Some(last_ts) = state.account.last_tick_ts {
                    if tick.quote.ts <= last_ts {
                        return Err(crate::Error::TickRejected(format!(
                            "tick ts {} is not newer than last-evaluated ts {} — refusing to evaluate out-of-order tick",
                            tick.quote.ts, last_ts
                        )));
                    }
                }
                // P1-5: use broker-reported equity/balance directly.
                // The engine does NOT recompute equity.
                let new_state = state
                    .update_equity(*broker_equity)
                    .update_balance(*broker_balance);
                events.push(DomainEvent::new(
                    new_state.account.id,
                    DomainEventKind::TickEvaluated {
                        equity: *broker_equity,
                    },
                    tick.quote.ts,
                ));
                Ok(AppliedEvent {
                    state: new_state,
                    ctx_kind: OnTick,
                    open_positions,
                    today_trades,
                    recent_events,
                    trade_fill: None,
                })
            }
            PipelineEvent::TickEstimated { tick } => {
                // P1-5 / estimated-equity separation: compute equity from
                // positions + quote, but write it ONLY to the estimated
                // track. Authoritative `equity`/`balance` and peak tracking
                // must remain unchanged so an estimate can never drive a
                // breach verdict or distort drawdown baselines.
                let equity = equity_after_tick(state.account.balance, &open_positions, &tick.quote);
                let balance = state.account.balance;
                let new_state = state.set_estimated_equity(equity, balance);
                events.push(DomainEvent::new(
                    new_state.account.id,
                    DomainEventKind::TickEvaluated { equity },
                    tick.quote.ts,
                ));
                Ok(AppliedEvent {
                    state: new_state,
                    ctx_kind: OnTick,
                    open_positions,
                    today_trades,
                    recent_events,
                    trade_fill: None,
                })
            }
            PipelineEvent::DayRollover { had_trades_today } => {
                let new_state = state.rollover_day(*had_trades_today, None);
                events.push(DomainEvent::new(
                    new_state.account.id,
                    DomainEventKind::DayRollover {
                        new_day_index: new_state.account.trading_day_index,
                        day_start: new_state.account.day_start_balance,
                    },
                    chrono::Utc::now(),
                ));
                Ok(AppliedEvent {
                    state: new_state,
                    ctx_kind: OnDayRollover,
                    open_positions,
                    today_trades,
                    recent_events,
                    trade_fill: None,
                })
            }
            PipelineEvent::EndOfDay => Ok(AppliedEvent {
                state,
                ctx_kind: OnEndOfDay,
                open_positions,
                today_trades,
                recent_events,
                trade_fill: None,
            }),
            PipelineEvent::OnDemand => Ok(AppliedEvent {
                state,
                ctx_kind: OnDemand,
                open_positions,
                today_trades,
                recent_events,
                trade_fill: None,
            }),
            // P1-12: emergency stop short-circuits state transition;
            // the actual `Emergency` verdict is produced in `process()`
            // after this function returns. Here we just stamp the
            // emergency-stop status on the account.
            PipelineEvent::EmergencyStop {
                reason,
                actor_id,
                at,
            } => {
                let new_state = state.emergency_stop(reason, actor_id, *at);
                events.push(DomainEvent::new(
                    new_state.account.id,
                    DomainEventKind::AccountStatusChanged {
                        from: crate::core::account::AccountStatus::Active,
                        to: crate::core::account::AccountStatus::EmergencyStopped,
                    },
                    *at,
                ));
                Ok(AppliedEvent {
                    state: new_state,
                    ctx_kind: OnDemand,
                    open_positions,
                    today_trades,
                    recent_events,
                    trade_fill: None,
                })
            }
            // P1-11: override-breach reverts the account from
            // Failed/EmergencyStopped back to Active. The original
            // violation stays in the audit log; this just transitions
            // the state and records the override.
            PipelineEvent::OverrideBreach { override_record } => {
                override_record.validate()?;
                if override_record.account_id != state.account.id {
                    return Err(crate::Error::invalid_state(format!(
                        "override account_id {} does not match evaluated account {}",
                        override_record.account_id, state.account.id
                    )));
                }
                let existing_events = self.event_store.all(state.account.id).await?;
                let clears_violation = existing_events
                    .iter()
                    .find_map(|e| match &e.kind {
                        crate::core::events::DomainEventKind::RuleViolated { violation } => {
                            if violation.id == override_record.clears_violation_id {
                                Some(violation)
                            } else {
                                None
                            }
                        }
                        _ => None,
                    })
                    .ok_or_else(|| {
                        crate::Error::invalid_state(format!(
                            "override references violation {} which does not exist for account {}",
                            override_record.clears_violation_id, state.account.id
                        ))
                    })?;
                if !clears_violation.is_terminating() {
                    return Err(crate::Error::invalid_state(format!(
                        "override references violation {} which is not a breach-terminal violation",
                        override_record.clears_violation_id
                    )));
                }
                let new_state = state.clear_breach(override_record)?;
                events.push(DomainEvent::new(
                    new_state.account.id,
                    DomainEventKind::AccountStatusChanged {
                        from: crate::core::account::AccountStatus::Failed,
                        to: crate::core::account::AccountStatus::Active,
                    },
                    override_record.at,
                ));
                events.push(DomainEvent::new(
                    new_state.account.id,
                    DomainEventKind::OverrideCleared {
                        clears_violation_id: override_record.clears_violation_id,
                        reason: override_record.reason.clone(),
                        actor_id: override_record.actor_id.clone(),
                        at: override_record.at,
                    },
                    override_record.at,
                ));
                Ok(AppliedEvent {
                    state: new_state,
                    ctx_kind: OnDemand,
                    open_positions,
                    today_trades,
                    recent_events,
                    trade_fill: None,
                })
            }
            // §D.2: payout request — evaluate via the plan's payout
            // config; a permitted request moves the funded account to
            // PayoutPending. A rejected request is a typed error.
            PipelineEvent::PayoutRequest => {
                let account_id = state.account.id;
                let config = state.account.plan.payout_config.clone().ok_or_else(|| {
                    crate::Error::invalid_state("plan has no payout configuration")
                })?;
                let now = chrono::Utc::now();
                let quote = crate::payout::quote_payout(
                    &state.account,
                    &config,
                    state.account.last_payout_at,
                    Some(state.account.balance_at_last_payout),
                    state.account.refund_used,
                    now,
                );
                if let Some(rejected) = &quote.rejected {
                    return Err(crate::Error::invalid_state(format!(
                        "payout request rejected: {rejected:?}"
                    )));
                }
                let from = state.account.status;
                let new_state = state.mark_payout_pending()?;
                events.push(DomainEvent::new(
                    account_id,
                    DomainEventKind::PayoutRequested {
                        profit_basis: quote.profit_basis,
                        amount: quote.amount,
                    },
                    now,
                ));
                if from != new_state.account.status {
                    events.push(DomainEvent::new(
                        account_id,
                        DomainEventKind::AccountStatusChanged {
                            from,
                            to: new_state.account.status,
                        },
                        now,
                    ));
                }
                Ok(AppliedEvent {
                    state: new_state,
                    ctx_kind: OnDemand,
                    open_positions,
                    today_trades,
                    recent_events,
                    trade_fill: None,
                })
            }
            // §D.2: payout approval — execute the payout (bookkeeping +
            // balance deduction), resolve PayoutPending → Funded.
            PipelineEvent::PayoutApprove => {
                let account_id = state.account.id;
                let config = state.account.plan.payout_config.clone().ok_or_else(|| {
                    crate::Error::invalid_state("plan has no payout configuration")
                })?;
                let now = chrono::Utc::now();
                let quote = crate::payout::quote_payout(
                    &state.account,
                    &config,
                    state.account.last_payout_at,
                    Some(state.account.balance_at_last_payout),
                    state.account.refund_used,
                    now,
                );
                if let Some(rejected) = &quote.rejected {
                    return Err(crate::Error::invalid_state(format!(
                        "payout approval rejected: {rejected:?}"
                    )));
                }
                let mut new_state = state;
                let amount = quote.amount;
                let fee_refund = quote.fee_refund;
                crate::payout::record_payout(&mut new_state.account, amount, fee_refund, now)?;
                new_state.account.refund_used =
                    new_state.account.refund_used || fee_refund.0 > rust_decimal::Decimal::ZERO;
                events.push(DomainEvent::new(
                    account_id,
                    DomainEventKind::PayoutApproved { amount, fee_refund },
                    now,
                ));
                Ok(AppliedEvent {
                    state: new_state,
                    ctx_kind: OnDemand,
                    open_positions,
                    today_trades,
                    recent_events,
                    trade_fill: None,
                })
            }
        }
    }

    fn build_context(
        &self,
        account: Account,
        kind: crate::rules::context::RuleContextKind,
        ev: &PipelineEvent,
        open_positions: &[Position],
        today_trades: &[Trade],
        recent_events: Vec<DomainEvent>,
    ) -> crate::rules::context::RuleContext {
        use crate::rules::context::RuleContext;
        let mut ctx = RuleContext::new(account);
        ctx.kind = kind;
        ctx.open_positions = open_positions.to_vec();
        ctx.today_trades = today_trades.to_vec();
        ctx.recent_events = recent_events;
        ctx.rule_config = crate::config::rule_config::RuleConfig::from_plan(&ctx.account.plan);
        match ev {
            PipelineEvent::OrderSubmitted { order } => {
                ctx.pending_order = Some(order.clone());
            }
            PipelineEvent::TradeFilled { trade } => {
                ctx.latest_trade = Some(trade.clone());
            }
            // P1-5: broker-is-truth equity input — tag the context so
            // breach-capable rules know they CAN terminate.
            PipelineEvent::Tick {
                tick,
                broker_equity,
                broker_balance,
            } => {
                ctx.latest_tick = Some(tick.clone());
                ctx.equity_input = EquityInput::BrokerReported {
                    equity: *broker_equity,
                    balance: *broker_balance,
                };
            }
            // P1-5 / estimated-equity separation: expose the engine-derived
            // estimate on the dedicated estimated track so display/backtest
            // consumers can use it while authoritative equity/balance stay
            // untouched for breach-capable rules.
            PipelineEvent::TickEstimated { tick } => {
                ctx.latest_tick = Some(tick.clone());
                ctx.equity_input = EquityInput::Estimated {
                    equity: ctx.account.estimated_equity,
                    balance: ctx.account.estimated_balance,
                };
            }
            _ => {}
        }
        ctx
    }

    fn emit_decision_event(
        &self,
        account_id: crate::core::ids::AccountId,
        decision: &Decision,
        events: &mut Vec<DomainEvent>,
    ) -> Option<DomainEvent> {
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
