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
use crate::equity_input::EquityInput;
use crate::events::store::EventStore;
use crate::notifications::traits::Notifier;
use crate::persistence::traits::AccountStore;

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
    pub fn process(
        &mut self,
        account_id: crate::core::ids::AccountId,
        ev: PipelineEvent,
    ) -> crate::Result<PipelineResult> {
        // Read the account (unscoped — account_id is UUIDv4 globally unique).
        let account = self
            .store
            .get(account_id)?
            .ok_or_else(|| crate::Error::NotFound(format!("account {account_id}")))?;
        // Extract the account's own tenant_id; pass to the OCC-scoped
        // process_for_tenant so the write uses put_with_version.
        let tenant_id = account.tenant_id;
        self.process_with_loaded_account(account, tenant_id, ev)
    }

    /// **P0-E fix**: tenant-scoped process. Reads via `get_for_tenant` so
    /// cross-tenant data leakage is impossible at the storage layer.
    /// Writes via `put_with_version(expected_version)` so a concurrent
    /// writer between our read and write produces `Error::StateConflict`,
    /// which the caller must retry.
    pub fn process_for_tenant(
        &mut self,
        tenant_id: crate::tenant::TenantId,
        account_id: crate::core::ids::AccountId,
        ev: PipelineEvent,
    ) -> crate::Result<PipelineResult> {
        let account = self
            .store
            .get_for_tenant(tenant_id, account_id)?
            .ok_or_else(|| {
                crate::Error::NotFound(format!(
                    "account {account_id} not found for tenant {tenant_id}"
                ))
            })?;
        self.process_with_loaded_account(account, tenant_id, ev)
    }

    /// Common path for `process` and `process_for_tenant` once the
    /// account is loaded. Owns the OCC write.
    fn process_with_loaded_account(
        &mut self,
        account: Account,
        _tenant_id: crate::tenant::TenantId,
        ev: PipelineEvent,
    ) -> crate::Result<PipelineResult> {
        let account_id = account.id;
        let expected_version = account.version;
        let mut state = AccountState::new(account);
        let mut events: Vec<DomainEvent> = Vec::new();

        // P1.1 auto-rollover: if the event's timestamp falls in a new
        // trading day compared to the account's current day, rollover
        // automatically so correctness never depends on the caller
        // sending DayRollover.
        if !matches!(ev, PipelineEvent::DayRollover { .. }) {
            let event_ts = ev.event_timestamp();
            let event_day_start = state.account.plan.trading_day_start(event_ts);
            let current_day_start =
                state.account.plan.trading_day_start(chrono::Utc::now());
            if event_day_start > current_day_start {
                let had_trades = !state.account.today_realized_pnl.0.is_zero();
                let rollover_ts = event_ts;
                state = state.rollover_day(had_trades);
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

        // Apply state transitions.
        let applied = self.apply_event(state, &ev, &mut events)?;
        let (new_state, ctx_kind, open_positions, today_trades, recent_events) = (
            applied.state,
            applied.ctx_kind,
            applied.open_positions,
            applied.today_trades,
            applied.recent_events,
        );
        // Build context
        let ctx = self.build_context(
            new_state.account.clone(),
            ctx_kind,
            &ev,
            &open_positions,
            &today_trades,
            recent_events,
        );
        // Evaluate rules
        let result = self.evaluator.evaluate(&ctx)?;
        // P0-2: post-evaluation state update.
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
            // P1.10: emit a LiquidationInstruction when the decision is
            // Liquidate or Emergency. The bridge consumes this event and
            // closes all listed positions on the broker side.
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
        // P0-E: do NOT bump version here — the store is the single owner of
        // version increments. `put_with_version` will bump on success or
        // return `StateConflict` if another writer beat us.
        // P1-14: stamp last_tick_ts if this was a tick event.
        match &ev {
            PipelineEvent::Tick { tick, .. } | PipelineEvent::TickEstimated { tick } => {
                final_state.account.last_tick_ts = Some(tick.quote.ts);
            }
            _ => {}
        }
        // Snapshot before write (so we can return it even on conflict-retry).
        let snap = Snapshot::new(&final_state.account, result.decision.clone());
        // P0-E: persist via `put_with_version` so optimistic-concurrency
        // conflicts are detected at the storage layer.
        self.store
            .put_with_version(final_state.account.clone(), expected_version)?;
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

    fn apply_event(
        &self,
        mut state: AccountState,
        ev: &PipelineEvent,
        events: &mut Vec<DomainEvent>,
    ) -> crate::Result<AppliedEvent> {
        use crate::rules::context::RuleContextKind::{
            OnDayRollover, OnDemand, OnEndOfDay, OnOrderSubmit, OnTick, OnTradeFill,
        };
        let open_positions = self
            .store
            .open_positions(state.account.id)
            .unwrap_or_default();
        let day_start = state.account.plan.trading_day_start(chrono::Utc::now());
        let today_trades = self
            .store
            .today_trades_since(state.account.id, day_start)
            .unwrap_or_default();
        let recent_events = self.event_store.recent(state.account.id, 50);
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
                })
            }
            PipelineEvent::OrderSubmitted { order: _ } => Ok(AppliedEvent {
                state,
                ctx_kind: OnOrderSubmit,
                open_positions,
                today_trades,
                recent_events,
            }),
            PipelineEvent::TradeFilled { trade } => {
                // Apply realized P&L on exits
                let (pnl, commission, swap) = match trade.exit_info.as_ref() {
                    Some(info) => (info.realized_pnl, trade.commission, trade.swap),
                    None => (Money::ZERO, trade.commission, trade.swap),
                };
                let new_state = state.apply_realized_pnl(pnl, commission, swap, trade.executed_at);
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
                })
            }
            PipelineEvent::TickEstimated { tick } => {
                // P1-5: estimate-only path. The engine computes equity
                // from positions + quote for display purposes only; breach
                // rules will see `EquityInput::Estimated` on the context
                // and refuse to terminate.
                let equity = equity_after_tick(state.account.balance, &open_positions, &tick.quote);
                let new_state = state.update_equity(equity);
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
                })
            }
            PipelineEvent::DayRollover { had_trades_today } => {
                let new_state = state.rollover_day(*had_trades_today);
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
                })
            }
            PipelineEvent::EndOfDay => Ok(AppliedEvent {
                state,
                ctx_kind: OnEndOfDay,
                open_positions,
                today_trades,
                recent_events,
            }),
            PipelineEvent::OnDemand => Ok(AppliedEvent {
                state,
                ctx_kind: OnDemand,
                open_positions,
                today_trades,
                recent_events,
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
                })
            }
            // P1-11: override-breach reverts the account from
            // Failed/EmergencyStopped back to Active. The original
            // violation stays in the audit log; this just transitions
            // the state and records the override.
            PipelineEvent::OverrideBreach { override_record } => {
                override_record.validate()?;
                let new_state = state.clear_breach(override_record)?;
                events.push(DomainEvent::new(
                    new_state.account.id,
                    DomainEventKind::AccountStatusChanged {
                        from: crate::core::account::AccountStatus::Failed,
                        to: crate::core::account::AccountStatus::Active,
                    },
                    override_record.at,
                ));
                Ok(AppliedEvent {
                    state: new_state,
                    ctx_kind: OnDemand,
                    open_positions,
                    today_trades,
                    recent_events,
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
            // P1-5: estimate-only path — breach-capable rules will refuse
            // to terminate on this context.
            PipelineEvent::TickEstimated { tick } => {
                ctx.latest_tick = Some(tick.clone());
                ctx.equity_input = EquityInput::Estimated {
                    equity: ctx.account.equity,
                    balance: ctx.account.balance,
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
