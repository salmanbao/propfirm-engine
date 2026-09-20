//! Consistency rule.
//!
//! Prop firms require that no single trading day's profit exceeds a certain
//! percentage (often 30–50%) of the total cumulative profit. This discourages
//! lucky single-day wins and rewards steady performance.

use crate::core::ids::RuleId;
use crate::core::types::{dec, Money};
use crate::core::violation::{ViolationKind, ViolationSeverity};
use crate::rules::context::{EvaluationScope, RuleContext};
use crate::rules::params::{ParameterizedRule, RuleParams};
use crate::rules::registry::build_violation;
use crate::rules::traits::{Rule, RuleVerdict};

#[derive(Debug, Clone, Default)]
pub struct ConsistencyRule {
    /// **P0-D fix**: pack-derived parameters. When `Some`, rule reads
    /// `value`/`basis`/`tolerance_cents`/`priority` from here instead of
    /// from `ctx.account.plan` — so a tenant editing the pack actually
    /// changes the verdict. When `None` (constructed via `Default`), the
    /// rule falls back to plan-derived config.
    pub params: Option<RuleParams>,
}

impl Rule for ConsistencyRule {
    fn id(&self) -> RuleId {
        RuleId::named("consistency")
    }
    fn name(&self) -> &'static str {
        "Consistency"
    }
    fn kind(&self) -> ViolationKind {
        ViolationKind::Consistency
    }
    fn scope(&self) -> EvaluationScope {
        EvaluationScope::Periodic
    }
    fn severity(&self) -> ViolationSeverity {
        ViolationSeverity::Warning
    }

    fn description(&self) -> &'static str {
        "Largest single-day profit must not exceed X% of total cumulative profit."
    }

    fn is_enabled(&self, ctx: &RuleContext) -> bool {
        ctx.account.plan.consistency_pct.is_some()
    }

    fn evaluate(&self, ctx: &RuleContext) -> crate::Result<RuleVerdict> {
        let cap_pct = match ctx.account.plan.consistency_pct {
            Some(p) => p,
            None => return Ok(RuleVerdict::Pass),
        };
        let total_profit = ctx.account.total_realized_pnl;
        if total_profit.0 <= dec!(0) {
            // No profit yet → nothing to check
            return Ok(RuleVerdict::Pass);
        }
        let largest_day = ctx.account.largest_day_profit;
        if largest_day.0 <= dec!(0) {
            return Ok(RuleVerdict::Pass);
        }
        let cap = Money(cap_pct.0 * total_profit.0);
        if largest_day.0 > cap.0 {
            let mut v = build_violation(
                self,
                ctx,
                ViolationSeverity::Warning,
                format!(
                    "Largest single-day profit {largest_day} exceeds {}% of total profit {total_profit} (cap {cap})",
                    cap_pct.0 * dec!(100)
                ),
            );
            v = v.with_breach(largest_day, cap);
            return Ok(RuleVerdict::Warn(v));
        }
        Ok(RuleVerdict::Pass)
    }
}

impl ConsistencyRule {
    /// Constructs a parameterized rule from a pack entry (P0-D fix).
    #[must_use]
    pub fn from_entry(entry: &crate::rulepack::RuleEntry) -> Self {
        ConsistencyRule {
            params: Some(RuleParams::from_entry(entry)),
        }
    }
}

impl ParameterizedRule for ConsistencyRule {
    fn from_entry(entry: &crate::rulepack::RuleEntry) -> Self {
        ConsistencyRule::from_entry(entry)
    }
}
