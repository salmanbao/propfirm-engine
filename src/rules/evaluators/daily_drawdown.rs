//! Daily drawdown rule.
//!
//! The maximum amount the account equity can fall in a single trading day,
//! measured from the day's starting balance (or balance at session open).
//!
//! `severity = Hard` because exceeding daily drawdown typically terminates
//! the account immediately.

use crate::core::ids::RuleId;
use crate::core::violation::{ViolationKind, ViolationSeverity};
use crate::rules::context::{EvaluationScope, RuleContext};
use crate::rules::registry::build_violation;
use crate::rules::traits::{Rule, RuleVerdict};

/// Maximum daily drawdown rule.
#[derive(Debug, Clone, Default)]
pub struct DailyDrawdownRule;

impl Rule for DailyDrawdownRule {
    fn id(&self) -> RuleId { RuleId::named("daily_drawdown") }
    fn name(&self) -> &str { "Daily Drawdown" }
    fn kind(&self) -> ViolationKind { ViolationKind::DailyDrawdown }
    fn scope(&self) -> EvaluationScope { EvaluationScope::OnTick }
    fn severity(&self) -> ViolationSeverity { ViolationSeverity::Hard }

    fn description(&self) -> &str {
        "Maximum drawdown permitted within a single trading day, measured from the day-start balance."
    }

    fn is_enabled(&self, ctx: &RuleContext) -> bool {
        ctx.account.plan.max_daily_drawdown_pct.0 > rust_decimal::Decimal::ZERO
    }

    fn evaluate(&self, ctx: &RuleContext) -> crate::Result<RuleVerdict> {
        let plan_pct = ctx.account.plan.max_daily_drawdown_pct;
        if plan_pct.0 <= rust_decimal::Decimal::ZERO {
            return Ok(RuleVerdict::Pass);
        }
        let limit = ctx.account.daily_dd_limit();
        let dd = ctx.account.daily_drawdown();
        if dd.0 > limit.0 {
            let mut v = build_violation(
                self,
                ctx,
                ViolationSeverity::Liquidate,
                format!(
                    "Daily drawdown breached: {dd} > {limit} ({}%)",
                    plan_pct.0 * rust_decimal::Decimal::ONE_HUNDRED
                ),
            );
            v = v.with_breach(dd, limit);
            return Ok(RuleVerdict::Liquidate(v));
        }
        // Warn at 80% utilization
        let warn_threshold = limit.0 * rust_decimal::Decimal::new(8, 1); // 0.8
        if dd.0 >= warn_threshold {
            let mut v = build_violation(
                self,
                ctx,
                ViolationSeverity::Warning,
                format!(
                    "Daily drawdown at {}/{} ({}%)",
                    dd,
                    limit,
                    (dd.0 / limit.0 * rust_decimal::Decimal::ONE_HUNDRED).round_dp(2)
                ),
            );
            v = v.with_breach(dd, limit);
            return Ok(RuleVerdict::Warn(v));
        }
        Ok(RuleVerdict::Pass)
    }
}
