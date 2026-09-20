//! Weekend holding rule.
//!
//! Forbids holding open positions over the weekend (Friday close to Sunday
//! open, in server time). Pre-trade scope checks pending orders on Friday
//! after the cutoff; periodic scope checks open positions approaching the
//! weekend.

use crate::core::ids::RuleId;
use crate::core::violation::{ViolationKind, ViolationSeverity};
use crate::rules::context::{EvaluationScope, RuleContext};
use crate::rules::registry::build_violation;
use crate::rules::traits::{Rule, RuleVerdict};
use chrono::{Datelike, Timelike, Weekday};

#[derive(Debug, Clone, Default)]
pub struct WeekendHoldingRule;

impl Rule for WeekendHoldingRule {
    fn id(&self) -> RuleId { RuleId::named("weekend_holding") }
    fn name(&self) -> &str { "Weekend Holding" }
    fn kind(&self) -> ViolationKind { ViolationKind::WeekendHolding }
    fn scope(&self) -> EvaluationScope { EvaluationScope::PreTrade }
    fn severity(&self) -> ViolationSeverity { ViolationSeverity::Hard }

    fn description(&self) -> &str {
        "Forbids holding positions over the weekend."
    }

    fn is_enabled(&self, ctx: &RuleContext) -> bool {
        !ctx.account.plan.weekend_holding_allowed
    }

    fn evaluate(&self, ctx: &RuleContext) -> crate::Result<RuleVerdict> {
        if ctx.account.plan.weekend_holding_allowed {
            return Ok(RuleVerdict::Pass);
        }
        let cfg = match ctx.rule_config.weekend_holding.as_ref() {
            Some(c) => c,
            None => return Ok(RuleVerdict::Pass),
        };
        let now = ctx.server_time.ts();
        let weekday = now.weekday();
        let hour = now.time().hour();
        // Friday after forbidden_from_hour → weekend starts
        let approaching_weekend = matches!(weekday, Weekday::Fri) && hour >= cfg.forbidden_from_hour as u32;
        let on_weekend = matches!(weekday, Weekday::Sat | Weekday::Sun);
        if !approaching_weekend && !on_weekend {
            return Ok(RuleVerdict::Pass);
        }
        if let Some(order) = &ctx.pending_order {
            let v = build_violation(
                self,
                ctx,
                ViolationSeverity::Hard,
                "Order submitted during weekend hold window",
            );
            return Ok(RuleVerdict::Fail(v));
        }
        if !ctx.open_positions.is_empty() {
            let v = build_violation(
                self,
                ctx,
                ViolationSeverity::Warning,
                "Open positions held during weekend window",
            );
            return Ok(RuleVerdict::Warn(v));
        }
        Ok(RuleVerdict::Pass)
    }
}
