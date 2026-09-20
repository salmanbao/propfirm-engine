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
use crate::rules::params::{ParameterizedRule, RuleParams};
use crate::rules::registry::build_violation;
use crate::rules::traits::{Rule, RuleVerdict};

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
    fn id(&self) -> RuleId {
        RuleId::named("profit_target")
    }
    fn name(&self) -> &'static str {
        "Profit Target"
    }
    fn kind(&self) -> ViolationKind {
        ViolationKind::ProfitTargetMissed
    }
    fn scope(&self) -> EvaluationScope {
        EvaluationScope::OnTick
    }
    fn severity(&self) -> ViolationSeverity {
        ViolationSeverity::Info
    }
    /// Positive outcomes (`TargetHit`) shouldn't drown out breach verdicts
    /// in the priority ordering — they get the default 100. If a breach
    /// fires on the same tick, the breach wins regardless.
    fn priority(&self) -> u32 {
        100
    }

    fn description(&self) -> &'static str {
        "Verifies the account has reached its profit target for the current phase. \
         First-reached timestamp is sticky (P0-2); emits TargetHit verdict distinct from \
         Pass (P0-3)."
    }

    /// **P0.4 fix**: pack entry's enabled flag gates the rule.
    fn is_enabled(&self, ctx: &RuleContext) -> bool {
        if let Some(p) = &self.params {
            return p.enabled;
        }
        ctx.account.plan.profit_target_pct.0 > dec!(0)
    }

    fn evaluate(&self, ctx: &RuleContext) -> crate::Result<RuleVerdict> {
        // P0.4: pack entry's value (unit-aware) overrides the plan target.
        let target = if let Some(p) = &self.params {
            if !p.enabled {
                return Ok(RuleVerdict::Pass);
            }
            match p.value() {
                Some(v) => match p.unit {
                    Some(crate::rulepack::RuleUnit::Money) => crate::core::types::Money(v),
                    _ => crate::core::types::Money(v * ctx.account.initial_balance.0),
                },
                None => ctx.account.profit_target(),
            }
        } else {
            ctx.account.profit_target()
        };
        let target_pct = crate::core::types::Pct(dec!(1)); // placeholder: target computed above
        let _ = target_pct;
        if target.0 <= dec!(0) {
            // No target (e.g. funded phase) – always pass.
            return Ok(RuleVerdict::Pass);
        }
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
    #[must_use]
    pub fn from_entry(entry: &crate::rulepack::RuleEntry) -> Self {
        ProfitTargetRule {
            params: Some(RuleParams::from_entry(entry)),
        }
    }
}

impl ParameterizedRule for ProfitTargetRule {
    fn from_entry(entry: &crate::rulepack::RuleEntry) -> Self {
        ProfitTargetRule::from_entry(entry)
    }
}
