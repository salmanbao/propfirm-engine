//! Cooldown rule.
//!
//! Enforces a minimum interval between trades. Useful for firms that want to
//! discourage high-frequency scalping behavior.

use crate::core::ids::RuleId;
use crate::core::violation::{ViolationKind, ViolationSeverity};
use crate::rules::context::{EvaluationScope, RuleContext};
use crate::rules::params::{ParameterizedRule, RuleParams};
use crate::rules::registry::build_violation;
use crate::rules::traits::{Rule, RuleVerdict};

#[derive(Debug, Clone, Default)]
pub struct CooldownRule {
    /// **P0-D fix**: pack-derived parameters. When `Some`, rule reads
    /// `value`/`basis`/`tolerance_cents`/`priority` from here instead of
    /// from `ctx.account.plan` — so a tenant editing the pack actually
    /// changes the verdict. When `None` (constructed via `Default`), the
    /// rule falls back to plan-derived config.
    pub params: Option<RuleParams>,
}

impl Rule for CooldownRule {
    fn id(&self) -> RuleId {
        RuleId::named("cooldown")
    }
    fn name(&self) -> &'static str {
        "Cooldown"
    }
    fn kind(&self) -> ViolationKind {
        ViolationKind::Cooldown
    }
    fn scope(&self) -> EvaluationScope {
        EvaluationScope::PreTrade
    }
    fn severity(&self) -> ViolationSeverity {
        ViolationSeverity::Warning
    }

    fn description(&self) -> &'static str {
        "Enforces a minimum interval between consecutive trades."
    }

    fn is_enabled(&self, ctx: &RuleContext) -> bool {
        ctx.account.plan.cooldown_seconds > 0
    }

    fn evaluate(&self, ctx: &RuleContext) -> crate::Result<RuleVerdict> {
        let cooldown = ctx.account.plan.cooldown_seconds;
        if cooldown == 0 {
            return Ok(RuleVerdict::Pass);
        }
        let Some(order) = &ctx.pending_order else {
            return Ok(RuleVerdict::Pass);
        };
        let Some(last_trade) = ctx.today_trades.last() else {
            return Ok(RuleVerdict::Pass);
        };
        let elapsed = (order.submitted_at - last_trade.executed_at).num_seconds();
        if elapsed < cooldown as i64 {
            let remaining = cooldown as i64 - elapsed;
            let v = build_violation(
                self,
                ctx,
                ViolationSeverity::Warning,
                format!("Cooldown violated: only {elapsed}s since last trade (need {cooldown}s); {remaining}s remaining"),
            );
            return Ok(RuleVerdict::Warn(v));
        }
        Ok(RuleVerdict::Pass)
    }
}

impl CooldownRule {
    /// Constructs a parameterized rule from a pack entry (P0-D fix).
    #[must_use]
    pub fn from_entry(entry: &crate::rulepack::RuleEntry) -> Self {
        CooldownRule {
            params: Some(RuleParams::from_entry(entry)),
        }
    }
}

impl ParameterizedRule for CooldownRule {
    fn from_entry(entry: &crate::rulepack::RuleEntry) -> Self {
        CooldownRule::from_entry(entry)
    }
}
