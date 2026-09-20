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
use crate::rules::params::{ParameterizedRule, RuleParams};
use chrono::{Datelike, Timelike, Weekday};

#[derive(Debug, Clone, Default)]
pub struct WeekendHoldingRule {
    /// **P0-D fix**: pack-derived parameters. When `Some`, rule reads
    /// `value`/`basis`/`tolerance_cents`/`priority` from here instead of
    /// from `ctx.account.plan` — so a tenant editing the pack actually
    /// changes the verdict. When `None` (constructed via `Default`), the
    /// rule falls back to plan-derived config.
    pub params: Option<RuleParams>,
}

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

impl WeekendHoldingRule {
    /// Constructs a parameterized rule from a pack entry (P0-D fix).
    pub fn from_entry(entry: &crate::rulepack::RuleEntry) -> Self {
        WeekendHoldingRule { params: Some(RuleParams::from_entry(entry)) }
    }
}

impl ParameterizedRule for WeekendHoldingRule {
    fn from_entry(entry: &crate::rulepack::RuleEntry) -> Self {
        WeekendHoldingRule::from_entry(entry)
    }
}
