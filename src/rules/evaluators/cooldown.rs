//! Cooldown rule.
//!
//! Enforces a minimum interval between trades. Useful for firms that want to
//! discourage high-frequency scalping behavior.

use crate::core::ids::RuleId;
use crate::core::violation::{ViolationKind, ViolationSeverity};
use crate::rules::context::{EvaluationScope, RuleContext};
use crate::rules::registry::build_violation;
use crate::rules::traits::{Rule, RuleVerdict};

#[derive(Debug, Clone, Default)]
pub struct CooldownRule;

impl Rule for CooldownRule {
    fn id(&self) -> RuleId { RuleId::named("cooldown") }
    fn name(&self) -> &str { "Cooldown" }
    fn kind(&self) -> ViolationKind { ViolationKind::Cooldown }
    fn scope(&self) -> EvaluationScope { EvaluationScope::PreTrade }
    fn severity(&self) -> ViolationSeverity { ViolationSeverity::Warning }

    fn description(&self) -> &str {
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
