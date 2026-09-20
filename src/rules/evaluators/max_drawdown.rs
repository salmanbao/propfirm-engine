//! Maximum (total) drawdown rule.
//!
//! The maximum cumulative drawdown from the account's peak (balance or
//! equity, configurable per plan).

use crate::core::ids::RuleId;
use crate::core::types::{dec, Money};
use crate::core::violation::{ViolationKind, ViolationSeverity};
use crate::rules::context::{EvaluationScope, RuleContext};
use crate::rules::registry::build_violation;
use crate::rules::traits::{Rule, RuleVerdict};

#[derive(Debug, Clone, Default)]
pub struct MaxDrawdownRule;

impl Rule for MaxDrawdownRule {
    fn id(&self) -> RuleId { RuleId::named("max_drawdown") }
    fn name(&self) -> &str { "Maximum Drawdown" }
    fn kind(&self) -> ViolationKind { ViolationKind::MaxDrawdown }
    fn scope(&self) -> EvaluationScope { EvaluationScope::OnTick }
    fn severity(&self) -> ViolationSeverity { ViolationSeverity::Hard }

    fn description(&self) -> &str {
        "Maximum cumulative drawdown permitted over the life of the account, measured from peak balance/equity."
    }

    fn is_enabled(&self, ctx: &RuleContext) -> bool {
        ctx.account.plan.max_total_drawdown_pct.0 > dec!(0)
    }

    fn evaluate(&self, ctx: &RuleContext) -> crate::Result<RuleVerdict> {
        let plan_pct = ctx.account.plan.max_total_drawdown_pct;
        if plan_pct.0 <= dec!(0) {
            return Ok(RuleVerdict::Pass);
        }
        let limit = ctx.account.max_dd_limit();
        let dd = if ctx.account.plan.drawdown_on_balance {
            ctx.account.balance_drawdown()
        } else {
            ctx.account.equity_drawdown()
        };
        if dd.0 > limit.0 {
            let mut v = build_violation(
                self,
                ctx,
                ViolationSeverity::Liquidate,
                format!(
                    "Maximum drawdown breached: {dd} > {limit} ({}%)",
                    plan_pct.0 * dec!(100)
                ),
            );
            v = v.with_breach(dd, limit);
            return Ok(RuleVerdict::Liquidate(v));
        }
        // Warn at 80% utilization
        let warn = limit.0 * dec!(0.8);
        if dd.0 >= warn {
            let mut v = build_violation(
                self,
                ctx,
                ViolationSeverity::Warning,
                format!(
                    "Maximum drawdown at {dd}/{limit} ({}%)",
                    (dd.0 / limit.0 * dec!(100)).round_dp(2)
                ),
            );
            v = v.with_breach(dd, limit);
            return Ok(RuleVerdict::Warn(v));
        }
        Ok(RuleVerdict::Pass)
    }
}

/// Helper to compute current drawdown from peak balance, as `Money`.
pub fn peak_drawdown(current: Money, peak: Money) -> Money {
    Money((peak.0 - current.0).max(dec!(0)))
}
