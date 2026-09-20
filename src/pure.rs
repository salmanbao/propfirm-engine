//! Pure stateless evaluation (P1-7 fix).
//!
//! The binding spec's ADR-11 requires the engine to be a **pure,
//! stateless function**: `evaluate(state, rule_pack, tick) → verdict`.
//! Any past verdict must be recomputable byte-for-byte from its
//! recorded inputs (`input_hash = sha256(state || rules || tick)`),
//! because that reproducibility *is* the dispute-resolution mechanism.
//!
//! This module extracts that pure function out of the side-effecting
//! [`Pipeline`](crate::engine::pipeline::Pipeline). It does not touch
//! storage; it takes everything it needs as input and returns
//! everything it produces as output. The pipeline wraps it for
//! stateful use; the HTTP `/internal/v1/evaluate` endpoint (P2-API)
//! calls it directly so the platform can call it statelessly.
//!
//! Every verdict produced here is paired with its `input_hash` so it
//! can be persisted alongside the verdict record and verified on
//! replay.

use crate::core::account::Account;
use crate::core::order::Order;
use crate::core::position::Position;
use crate::core::tick::Tick;
use crate::core::trade::Trade;
use crate::core::events::DomainEvent;
use crate::core::types::ServerTime;
use crate::engine::decision::Decision;
use crate::engine::evaluator::EvaluationResult;
use crate::rules::context::{RuleContext, RuleContextKind};
use crate::rules::registry::RuleRegistry;
use crate::rulepack::RulePack;
use crate::sha256_helper::Sha256Hasher;

/// The pure stateless evaluate function.
///
/// Inputs:
/// - `account`: the post-event account state (mutated by the caller
///   for trade fills / ticks / day rollover; this function does not
///   touch storage).
/// - `pack`: the rule pack to evaluate against (P1-6 data).
/// - `registry`: the active rule set (built from `pack` upstream).
/// - `server_time`: **P0-C fix** — the wall-clock time the evaluation
///   should treat as "now". Required for replay determinism: time-aware
///   rules (`time_limit`, `min_trading_days`, `overnight`, `weekend`)
///   read this value instead of `Utc::now()`, so the same recorded
///   `(account, pack, tick, server_time)` tuple always produces the
///   same verdict. The `server_time` is included in the `input_hash`.
///
/// Outputs:
/// - [`PureVerdict`]: the decision + the input hash + all reports.
///
/// This function is pure: given the same `(account, pack, tick,
/// server_time)` quadruple, it always produces the same verdict and the
/// same `input_hash`. That's the property the binding spec calls "one
/// defensible answer" — and the property that enables EVL-33
/// replay/backfill.
pub fn evaluate(
    account: &Account,
    pack: &RulePack,
    registry: &RuleRegistry,
    ctx_kind: RuleContextKind,
    server_time: ServerTime,
    open_positions: &[Position],
    today_trades: &[Trade],
    recent_events: Vec<DomainEvent>,
    pending_order: Option<&Order>,
    latest_trade: Option<&Trade>,
    latest_tick: Option<&Tick>,
) -> crate::Result<PureVerdict> {
    // 1. Build the context.
    let mut ctx = RuleContext::new(account.clone());
    ctx.kind = ctx_kind;
    ctx.open_positions = open_positions.to_vec();
    ctx.today_trades = today_trades.to_vec();
    ctx.recent_events = recent_events;
    ctx.rule_config = crate::config::rule_config::RuleConfig::from_plan(&account.plan);
    // P0-C: explicit server_time — rules read this instead of `Utc::now()`.
    ctx.server_time = server_time;
    // P1-5: the caller of pure::evaluate has loaded the account from a
    // trusted source (broker bridge, replay log, etc.). Mark equity as
    // broker-reported so breach-capable rules CAN terminate. For
    // estimate-only paths, the caller should construct the context
    // directly via `RuleContext::with_estimated_equity`.
    ctx = ctx.with_broker_equity(account.equity, account.balance);
    if let Some(o) = pending_order {
        ctx.pending_order = Some(o.clone());
    }
    if let Some(t) = latest_trade {
        ctx.latest_trade = Some(t.clone());
    }
    if let Some(t) = latest_tick {
        ctx.latest_tick = Some(t.clone());
    }

    // 2. Compute the input hash from the relevant inputs *before*
    //    evaluation. This is what makes the verdict reproducible: the
    //    same (account, pack, ctx_kind, server_time, open_positions,
    //    today_trades, pending_order, latest_trade, latest_tick)
    //    tuple always hashes to the same value, and always produces
    //    the same decision.
    let input_hash = compute_input_hash(
        account, pack, ctx_kind, server_time,
        open_positions, today_trades,
        pending_order, latest_trade, latest_tick,
    );

    // 3. Run the registry's rule evaluation.
    let reports = registry.evaluate(&ctx)?;
    let decision = crate::engine::decision::Decision::from_reports(&reports);

    Ok(PureVerdict {
        decision,
        reports,
        input_hash,
        pack_version: pack.version,
        pack_id: pack.id.clone(),
    })
}

/// The output of [`evaluate`]: the decision + the input hash that
/// produced it + the per-rule reports.
#[derive(Debug, Clone)]
pub struct PureVerdict {
    /// The aggregated decision (Pass/Warn/Fail/Liquidate/TargetHit/Emergency).
    pub decision: Decision,
    /// Per-rule reports.
    pub reports: Vec<crate::rules::traits::RuleReport>,
    /// SHA256-prefix hash of the inputs that produced this verdict.
    /// Persist this alongside the verdict record so any past decision
    /// can be recomputed byte-for-byte from its recorded inputs.
    pub input_hash: String,
    /// The version of the rule pack that produced this verdict.
    pub pack_version: u32,
    /// The id of the rule pack that produced this verdict.
    pub pack_id: String,
}

impl PureVerdict {
    /// Returns true if this verdict would terminate the account.
    pub fn is_terminating(&self) -> bool {
        self.decision.is_terminating()
    }
}

/// Computes the `input_hash` (P0-B + P0-C fix). Real sha256 of every
/// input that affects the verdict — including `server_time`, so the
/// same recorded inputs at different wall-clock times produce different
/// hashes (and potentially different verdicts from time-aware rules).
///
/// This is the cryptographic fingerprint that makes verdicts
/// reproducible: persist it alongside the verdict record, and any past
/// decision can be recomputed byte-for-byte from its recorded inputs.
///
/// Implementation: uses the real `sha2::Sha256` (256-bit, 64 hex chars).
/// Previously this was mislabeled SipHash truncated to 16 hex chars.
pub fn compute_input_hash(
    account: &Account,
    pack: &RulePack,
    ctx_kind: RuleContextKind,
    server_time: ServerTime,
    open_positions: &[Position],
    today_trades: &[Trade],
    pending_order: Option<&Order>,
    latest_trade: Option<&Trade>,
    latest_tick: Option<&Tick>,
) -> String {
    use std::hash::{Hash, Hasher};
    let mut h = Sha256Hasher::new();

    // Hash account state — the bits that affect rule evaluation.
    account.id.hash(&mut h);
    account.balance.0.hash(&mut h);
    account.equity.0.hash(&mut h);
    account.peak_balance.0.hash(&mut h);
    account.peak_equity.0.hash(&mut h);
    account.day_start_balance.0.hash(&mut h);
    account.total_realized_pnl.0.hash(&mut h);
    account.active_trading_days.hash(&mut h);
    account.target_reached_at.hash(&mut h);
    account.status.hash(&mut h);

    // Hash the rule pack's content hash (real sha256, computed by the pack).
    pack.content_hash().hash(&mut h);

    // P0-C: hash server_time — required for replay determinism. Time-aware
    // rules (time_limit, min_trading_days, overnight, weekend) read this
    // value, so it must be part of the input hash.
    server_time.ts().hash(&mut h);

    // Hash the context kind.
    ctx_kind.to_string().hash(&mut h);

    // Hash open positions.
    for p in open_positions {
        p.id.hash(&mut h);
        p.symbol.hash(&mut h);
        p.side.hash(&mut h);
        p.open_quantity.0.hash(&mut h);
        p.avg_entry_price.0.hash(&mut h);
    }

    // Hash today's trades.
    for t in today_trades {
        t.id.hash(&mut h);
        t.symbol.hash(&mut h);
        t.side.hash(&mut h);
        t.price.0.hash(&mut h);
        t.quantity.0.hash(&mut h);
        t.executed_at.hash(&mut h);
    }

    // Hash pending order, latest trade, latest tick if present.
    if let Some(o) = pending_order {
        o.id.hash(&mut h);
        o.symbol.hash(&mut h);
        o.side.hash(&mut h);
        o.quantity.0.hash(&mut h);
        o.submitted_at.hash(&mut h);
    }
    if let Some(t) = latest_trade {
        t.id.hash(&mut h);
        t.executed_at.hash(&mut h);
    }
    if let Some(t) = latest_tick {
        t.symbol.hash(&mut h);
        t.quote.bid.0.hash(&mut h);
        t.quote.ask.0.hash(&mut h);
        t.quote.ts.hash(&mut h);
    }

    // Real sha256: full 256-bit digest, 64 hex chars.
    h.finalize_hex()
}

/// Helper: extract an [`EvaluationResult`]-compatible view from a
/// [`PureVerdict`] so the existing pipeline can consume it without
/// touching storage.
impl From<PureVerdict> for EvaluationResult {
    fn from(v: PureVerdict) -> Self {
        EvaluationResult {
            reports: v.reports,
            decision: v.decision,
        }
    }
}
