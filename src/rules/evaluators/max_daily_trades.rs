//! Maximum daily trades rule.
//!
//! Forbids new orders when the trader has already submitted the maximum
//! number of trades today.

use crate::core::ids::RuleId;
use crate::core::violation::{ViolationKind, ViolationSeverity};
use crate::rules::context::{EvaluationScope, RuleContext};
use crate::rules::registry::build_violation;
use crate::rules::traits::{Rule, RuleVerdict};

#[derive(Debug, Clone, Default)]
pub struct MaxDailyTradesRule;

impl Rule for MaxDailyTradesRule {
    fn id(&self) -> RuleId { RuleId::named("max_daily_trades") }
    fn name(&self) -> &str { "Max Daily Trades" }
    fn kind(&self) -> ViolationKind { ViolationKind::MaxDailyTrades }
    fn scope(&self) -> EvaluationScope { EvaluationScope::PreTrade }
    fn severity(&self) -> ViolationSeverity { ViolationSeverity::Hard }

    fn description(&self) -> &str {
        "Forbids new orders when the number of trades today has reached the daily cap."
    }

    fn is_enabled(&self, ctx: &RuleContext) -> bool {
        ctx.account.plan.max_daily_trades.is_some()
    }

    fn evaluate(&self, ctx: &RuleContext) -> crate::Result<RuleVerdict> {
        let max = match ctx.account.plan.max_daily_trades {
            Some(v) => v,
            None => return Ok(RuleVerdict::Pass),
        };
        let today = ctx.today_trades.len() as u32;
        if let Some(order) = &ctx.pending_order {
            if matches!(order.kind, crate::core::order::OrderKind::Close { .. }) {
                return Ok(RuleVerdict::Pass);
            }
            if today + 1 > max {
                let v = build_violation(
                    self,
                    ctx,
                    ViolationSeverity::Hard,
                    format!("Daily trade count {today}+1 would exceed limit {max}"),
                );
                return Ok(RuleVerdict::Fail(v));
            }
        }
        Ok(RuleVerdict::Pass)
    }
}
