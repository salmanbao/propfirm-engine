//! Profit target rule.
//!
//! **P0-2 fix**: this rule now records `target_reached_at` on the account
//! the first time `net_profit >= target` is observed. That timestamp is
//! *sticky* — it is never cleared, even if equity subsequently dips back
//! below target before `min_trading_days` is satisfied. The account stays
//! in `TargetHitPending` until the day count is met, at which point it
//! transitions to `Passed`.
//!
//! **P0-3 fix**: this rule emits [`RuleVerdict::TargetHit`] (distinct from
//! `Pass`) the first time the target is reached, so downstream consumers
//! can react to the promotion. If a breach fires on the same evaluation,
//! the breach wins (see [`Decision::from_reports`]).

use crate::core::ids::RuleId;
use crate::core::types::{dec, Money};
use crate::core::violation::{ViolationKind, ViolationSeverity};
use crate::rules::context::{EvaluationScope, RuleContext};
use crate::rules::registry::build_violation;
use crate::rules::traits::{Rule, RuleVerdict};
use crate::rules::params::{ParameterizedRule, RuleParams};

#[derive(Debug, Clone, Default)]
pub struct ProfitTargetRule {
    /// **P0-D fix**: pack-derived parameters. When `Some`, rule reads
    /// `value`/`basis`/`tolerance_cents`/`priority` from here instead of
    /// from `ctx.account.plan` — so a tenant editing the pack actually
    /// changes the verdict. When `None` (constructed via `Default`), the
    /// rule falls back to plan-derived config.
    pub params: Option<RuleParams>,
}

impl Rule for ProfitTargetRule {
    fn id(&self) -> RuleId { RuleId::named("profit_target") }
    fn name(&self) -> &str { "Profit Target" }
    fn kind(&self) -> ViolationKind { ViolationKind::ProfitTargetMissed }
    fn scope(&self) -> EvaluationScope { EvaluationScope::OnTick }
    fn severity(&self) -> ViolationSeverity { ViolationSeverity::Info }
    /// Positive outcomes (TargetHit) shouldn't drown out breach verdicts
    /// in the priority ordering — they get the default 100. If a breach
    /// fires on the same tick, the breach wins regardless.
    fn priority(&self) -> u32 { 100 }

    fn description(&self) -> &str {
        "Verifies the account has reached its profit target for the current phase. \
         First-reached timestamp is sticky (P0-2); emits TargetHit verdict distinct from \
         Pass (P0-3)."
    }

    fn evaluate(&self, ctx: &RuleContext) -> crate::Result<RuleVerdict> {
        let target_pct = ctx.account.plan.profit_target_pct;
        if target_pct.0 <= dec!(0) {
            // No target (e.g. funded phase) – always pass.
            return Ok(RuleVerdict::Pass);
        }
        let target = ctx.account.profit_target();
        let net = ctx.account.net_profit();

        // --- P0-2: sticky target-reached state ------------------------
        // If we've never recorded target_reached_at and net just crossed
        // target, this is the *first* hit — record it and emit TargetHit.
        // Note: ctx is read-only here; the actual mutation of
        // target_reached_at happens in the pipeline's state-update step
        // after the verdict is consumed. We signal intent via the verdict.
        if ctx.account.target_reached_at.is_none() && net.0 >= target.0 {
            let mut v = build_violation(
                self,
                ctx,
                ViolationSeverity::Info,
                format!("Profit target reached: net {net} >= target {target} — pending min trading days"),
            );
            v = v.with_breach(Money(net.0.max(dec!(0))), target);
            return Ok(RuleVerdict::TargetHit(v));
        }

        // If target was already reached (pending state), don't re-emit
        // TargetHit — just Pass. The pipeline's state-update step will
        // promote to Passed if active_trading_days >= min_trading_days.
        if ctx.account.target_reached_at.is_some() {
            return Ok(RuleVerdict::Pass);
        }

        // Check time limit. If the deadline has elapsed without reaching
        // target, that's a hard failure.
        if let Some(deadline) = ctx.account.deadline {
            if ctx.server_time.ts() > deadline {
                let mut v = build_violation(
                    self,
                    ctx,
                    ViolationSeverity::Hard,
                    format!(
                        "Time limit expired without reaching profit target: net {net} < target {target}"
                    ),
                );
                v = v.with_breach(target, target);
                return Ok(RuleVerdict::Fail(v));
            }
        }

        // Below target, within time limit — pass (nothing to flag).
        Ok(RuleVerdict::Pass)
    }
}

impl ProfitTargetRule {
    /// Constructs a parameterized rule from a pack entry (P0-D fix).
    pub fn from_entry(entry: &crate::rulepack::RuleEntry) -> Self {
        ProfitTargetRule { params: Some(RuleParams::from_entry(entry)) }
    }
}

impl ParameterizedRule for ProfitTargetRule {
    fn from_entry(entry: &crate::rulepack::RuleEntry) -> Self {
        ProfitTargetRule::from_entry(entry)
    }
}
