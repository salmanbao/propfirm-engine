//! Minimum trading days rule.
//!
//! Many prop firms require the trader to be active on at least N distinct
//! days before passing a phase. The rule is evaluated periodically and on
//! demand; it produces a warning if the trader hasn't yet met the day count.

use crate::core::ids::RuleId;
use crate::core::types::dec;
use crate::core::violation::{ViolationKind, ViolationSeverity};
use crate::rules::context::{EvaluationScope, RuleContext};
use crate::rules::registry::build_violation;
use crate::rules::traits::{Rule, RuleVerdict};

#[derive(Debug, Clone, Default)]
pub struct MinTradingDaysRule;

impl Rule for MinTradingDaysRule {
    fn id(&self) -> RuleId { RuleId::named("min_trading_days") }
    fn name(&self) -> &str { "Minimum Trading Days" }
    fn kind(&self) -> ViolationKind { ViolationKind::MinTradingDays }
    fn scope(&self) -> EvaluationScope { EvaluationScope::Periodic }
    fn severity(&self) -> ViolationSeverity { ViolationSeverity::Warning }

    fn description(&self) -> &str {
        "Requires a minimum number of distinct active trading days before a phase can be passed."
    }

    fn is_enabled(&self, ctx: &RuleContext) -> bool {
        ctx.account.plan.min_trading_days > 0
    }

    fn evaluate(&self, ctx: &RuleContext) -> crate::Result<RuleVerdict> {
        let required = ctx.account.plan.min_trading_days;
        if required == 0 {
            return Ok(RuleVerdict::Pass);
        }
        let actual = ctx.account.active_trading_days;
        if actual >= required {
            return Ok(RuleVerdict::Pass);
        }
        // Only fail if deadline has been reached
        if let Some(deadline) = ctx.account.deadline {
            if ctx.server_time.ts() > deadline && actual < required {
                let v = build_violation(
                    self,
                    ctx,
                    ViolationSeverity::Hard,
                    format!("Deadline reached with only {actual}/{required} active trading days"),
                );
                return Ok(RuleVerdict::Fail(v));
            }
        }
        let v = build_violation(
            self,
            ctx,
            ViolationSeverity::Warning,
            format!("Active trading days {actual}/{required} – phase not yet complete"),
        );
        Ok(RuleVerdict::Warn(v))
    }
}
