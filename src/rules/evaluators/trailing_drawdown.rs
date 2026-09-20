//! Trailing drawdown rule.
//!
//! Unlike static max drawdown (anchored at the initial balance or a fixed
//! peak), trailing drawdown trails the peak by a fixed percentage and
//! terminates the account if equity falls below the trail.

use crate::core::ids::RuleId;
use crate::core::types::{dec, Money};
use crate::core::violation::{ViolationKind, ViolationSeverity};
use crate::rules::context::{EvaluationScope, RuleContext};
use crate::rules::registry::build_violation;
use crate::rules::traits::{Rule, RuleVerdict};

#[derive(Debug, Clone, Default)]
pub struct TrailingDrawdownRule;

impl Rule for TrailingDrawdownRule {
    fn id(&self) -> RuleId { RuleId::named("trailing_drawdown") }
    fn name(&self) -> &str { "Trailing Drawdown" }
    fn kind(&self) -> ViolationKind { ViolationKind::TrailingDrawdown }
    fn scope(&self) -> EvaluationScope { EvaluationScope::OnTick }
    fn severity(&self) -> ViolationSeverity { ViolationSeverity::Hard }

    fn description(&self) -> &str {
        "Drawdown limit that trails the peak equity. Terminate if equity falls below (peak - trail%)."
    }

    fn is_enabled(&self, ctx: &RuleContext) -> bool {
        ctx.account.plan.trailing_drawdown_enabled
            && ctx.account.plan.trailing_drawdown_pct.0 > dec!(0)
    }

    fn evaluate(&self, ctx: &RuleContext) -> crate::Result<RuleVerdict> {
        if !ctx.account.plan.trailing_drawdown_enabled {
            return Ok(RuleVerdict::Pass);
        }
        let trail_pct = ctx.account.plan.trailing_drawdown_pct;
        let peak = ctx.account.peak_equity;
        let trail_amount = Money(trail_pct.0 * peak.0);
        let floor = Money(peak.0 - trail_amount.0);
        let equity = ctx.account.equity;
        if equity.0 < floor.0 {
            let breach = Money(floor.0 - equity.0);
            let mut v = build_violation(
                self,
                ctx,
                ViolationSeverity::Liquidate,
                format!(
                    "Trailing drawdown breached: equity {equity} below floor {floor} (trail={trail_amount})"
                ),
            );
            v = v.with_breach(breach, trail_amount);
            return Ok(RuleVerdict::Liquidate(v));
        }
        let warn_floor = Money(floor.0 + trail_amount.0 * dec!(0.2));
        if equity.0 < warn_floor.0 {
            let mut v = build_violation(
                self,
                ctx,
                ViolationSeverity::Warning,
                format!(
                    "Equity approaching trailing drawdown floor: {equity} vs floor {floor}"
                ),
            );
            v = v.with_breach(Money(warn_floor.0 - equity.0), trail_amount);
            return Ok(RuleVerdict::Warn(v));
        }
        Ok(RuleVerdict::Pass)
    }
}
