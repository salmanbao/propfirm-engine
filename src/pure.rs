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
use crate::core::events::DomainEvent;
use crate::core::order::Order;
use crate::core::position::Position;
use crate::core::tick::Tick;
use crate::core::trade::Trade;
use crate::core::types::ServerTime;
use crate::engine::decision::Decision;
use crate::engine::evaluator::EvaluationResult;
use crate::rulepack::RulePack;
use crate::rules::context::{RuleContext, RuleContextKind};
use crate::rules::registry::RuleRegistry;
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
    inputs: EvaluateInputs<'_>,
) -> crate::Result<PureVerdict> {
    // 1. Build the context.
    let mut ctx = RuleContext::new(account.clone());
    ctx.kind = ctx_kind;
    ctx.open_positions = inputs.open_positions.to_vec();
    ctx.today_trades = inputs.today_trades.to_vec();
    ctx.recent_events = inputs.recent_events;
    ctx.cross_reference_trades = inputs.cross_reference_trades.clone();
    ctx.rule_config = crate::config::rule_config::RuleConfig::from_plan(&account.plan);
    // P0-C: explicit server_time — rules read this instead of `Utc::now()`.
    ctx.server_time = server_time;
    // **P0.5 fix**: equity provenance is now EXPLICIT. The previous code
    // unconditionally stamped `with_broker_equity(...)`, letting any
    // caller have arbitrary equity treated as broker truth — which
    // bypassed the P1-5 "estimates cannot terminate" guard entirely
    // (and /internal/v1/evaluate took account state from the request
    // body). Callers must now state the source via
    // `EquitySource::BrokerReported` or `EquitySource::Estimated`;
    // the HTTP DTO defaults to the safe option (`estimated`).
    ctx = match inputs.equity_source {
        EquitySource::BrokerReported => ctx.with_broker_equity(account.equity, account.balance),
        EquitySource::Estimated => ctx.with_estimated_equity(account.equity, account.balance),
    };
    if let Some(o) = inputs.pending_order {
        ctx.pending_order = Some(o.clone());
    }
    if let Some(t) = inputs.latest_trade {
        ctx.latest_trade = Some(t.clone());
    }
    if let Some(t) = inputs.latest_tick {
        ctx.latest_tick = Some(t.clone());
    }

    // 2. Compute the input hash from the relevant inputs *before*
    //    evaluation. This is what makes the verdict reproducible: the
    //    same (account, pack, ctx_kind, server_time, open_positions,
    //    today_trades, pending_order, latest_trade, latest_tick)
    //    tuple always hashes to the same value, and always produces
    //    the same decision.
    let input_hash = compute_input_hash(
        account,
        pack,
        ctx_kind,
        server_time,
        inputs.open_positions,
        inputs.today_trades,
        inputs.pending_order,
        inputs.latest_trade,
        inputs.latest_tick,
        &inputs.cross_reference_trades,
        inputs.equity_source,
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
    #[must_use]
    pub fn is_terminating(&self) -> bool {
        self.decision.is_terminating()
    }
}

/// **P0.5 fix**: who vouches for the equity/balance numbers on the
/// account state. Mirrors [`EquityInput`] at the API boundary so a
/// caller must *state* provenance instead of the engine assuming it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Hash)]
pub enum EquitySource {
    /// Equity came from the broker bridge. Breach-capable rules may
    /// terminate on it (P1-5). Only pass this from trusted paths.
    BrokerReported,
    /// Equity is an engine estimate. Breach-capable rules downgrade to
    /// Warn and never terminate. **The safe default.**
    #[default]
    Estimated,
}

impl EquitySource {
    /// Parses the wire form used by the evaluate request DTO.
    pub fn parse(s: &str) -> Result<Self, crate::core::Error> {
        match s.to_ascii_lowercase().as_str() {
            "broker_reported" | "broker" => Ok(EquitySource::BrokerReported),
            "estimated" | "estimate" => Ok(EquitySource::Estimated),
            other => Err(crate::core::Error::invalid_config(format!(
                "unknown equity_source '{other}' (expected broker_reported | estimated)"
            ))),
        }
    }
}

/// The per-call inputs to [`evaluate`] beyond the core
/// `(account, pack, registry, ctx_kind, server_time)` tuple. Grouped into
/// one struct so the signature stays readable and callers can pass `Default`.
///
/// **P0.5 fix**: `Default` yields `EquitySource::Estimated` — the safe
/// option. Termination requires explicitly claiming broker provenance.
#[derive(Debug, Clone, Default)]
pub struct EvaluateInputs<'a> {
    /// Open positions at evaluation time.
    pub open_positions: &'a [Position],
    /// Trades executed in the current trading day.
    pub today_trades: &'a [Trade],
    /// Recent domain events (for event-scanning rules).
    pub recent_events: Vec<DomainEvent>,
    /// **§A.2 fix**: fills from *other* accounts (cross-account reference
    /// feed) used by the copy-trading rule. Never include this account's
    /// own trades here.
    pub cross_reference_trades: Vec<Trade>,
    /// Pending (pre-trade) order, if evaluating an order.
    pub pending_order: Option<&'a Order>,
    /// Most recent fill, if evaluating a trade.
    pub latest_trade: Option<&'a Trade>,
    /// The tick being evaluated, if evaluating a tick.
    pub latest_tick: Option<&'a Tick>,
    /// **P0.5 fix**: who vouches for the account's equity/balance.
    /// Defaults to [`EquitySource::Estimated`] — never terminates.
    pub equity_source: EquitySource,
}

impl<'a> EvaluateInputs<'a> {
    /// Convenience constructor: tick evaluation with only positions and
    /// today's trades (the common `/internal/v1/evaluate` shape).
    ///
    /// **P0.5 fix**: equity provenance defaults to
    /// [`EquitySource::Estimated`]. Use [`Self::with_equity_source`] to
    /// claim broker truth explicitly.
    pub fn for_tick(
        open_positions: &'a [Position],
        today_trades: &'a [Trade],
        latest_tick: &'a Tick,
    ) -> Self {
        EvaluateInputs {
            open_positions,
            today_trades,
            latest_tick: Some(latest_tick),
            ..Default::default()
        }
    }

    /// Overrides the equity provenance (P0.5). Chain after `for_tick`
    /// when the caller is a trusted broker-bridge path.
    #[must_use]
    pub fn with_equity_source(mut self, src: EquitySource) -> Self {
        self.equity_source = src;
        self
    }

    /// **§A.2 fix**: supplies the cross-account reference trades used by
    /// the copy-trading rule. The caller (platform bridge) must only
    /// pass fills from *other* accounts.
    #[must_use]
    pub fn with_cross_reference_trades(mut self, trades: Vec<Trade>) -> Self {
        self.cross_reference_trades = trades;
        self
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
/// Previously this was mislabeled `SipHash` truncated to 16 hex chars.
/// Computes the `input_hash` (P0-B + P0-C fix). Real sha256 of every
/// input that affects the verdict — including `server_time`, so the
/// same recorded inputs at different wall-clock times produce different
/// hashes (and potentially different verdicts from time-aware rules).
#[allow(clippy::too_many_arguments)] // hash covers every evaluation input
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
    cross_reference_trades: &[Trade],
    equity_source: EquitySource,
) -> String {
    use std::hash::Hash;
    let mut h = Sha256Hasher::new();

    // Hash account state — the bits that affect rule evaluation.
    account.id.hash(&mut h);
    account.balance.0.hash(&mut h);
    account.equity.0.hash(&mut h);
    account.estimated_equity.0.hash(&mut h);
    account.estimated_balance.0.hash(&mut h);
    account.peak_balance.0.hash(&mut h);
    account.peak_equity.0.hash(&mut h);
    account.day_start_balance.0.hash(&mut h);
    account.total_realized_pnl.0.hash(&mut h);
    account.active_trading_days.hash(&mut h);
    account.target_reached_at.hash(&mut h);
    account.status.hash(&mut h);

    // Hash the account's bound plan so the input changes when the plan
    // changes (P1-6 fix: the evaluator's source of truth is the plan,
    // not an unbound caller-supplied pack).
    let plan_bytes = serde_json::to_vec(&account.plan).unwrap_or_default();
    plan_bytes.hash(&mut h);

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

    for t in cross_reference_trades {
        t.id.hash(&mut h);
        t.account_id.hash(&mut h);
        t.symbol.hash(&mut h);
        t.price.0.hash(&mut h);
        t.quantity.0.hash(&mut h);
    }

    equity_source.hash(&mut h);

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
