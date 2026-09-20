//! Stop-loss required rule.

use crate::core::ids::RuleId;
use crate::core::violation::{ViolationKind, ViolationSeverity};
use crate::rules::context::{EvaluationScope, RuleContext};
use crate::rules::registry::build_violation;
use crate::rules::traits::{Rule, RuleVerdict};

#[derive(Debug, Clone, Default)]
pub struct StopLossRequiredRule;

impl Rule for StopLossRequiredRule {
    fn id(&self) -> RuleId { RuleId::named("sl_required") }
    fn name(&self) -> &str { "Stop-Loss Required" }
    fn kind(&self) -> ViolationKind { ViolationKind::MissingStopLoss }
    fn scope(&self) -> EvaluationScope { EvaluationScope::PreTrade }
    fn severity(&self) -> ViolationSeverity { ViolationSeverity::Hard }

    fn description(&self) -> &str {
        "Requires every new position to have a stop-loss set at submission."
    }

    fn is_enabled(&self, ctx: &RuleContext) -> bool {
        ctx.account.plan.require_stop_loss
    }

    fn evaluate(&self, ctx: &RuleContext) -> crate::Result<RuleVerdict> {
        if !ctx.account.plan.require_stop_loss {
            return Ok(RuleVerdict::Pass);
        }
        let Some(order) = &ctx.pending_order else {
            return Ok(RuleVerdict::Pass);
        };
        if !matches!(order.kind, crate::core::order::OrderKind::Open) {
            return Ok(RuleVerdict::Pass);
        }
        if order.stop_loss.is_none() {
            let v = build_violation(
                self,
                ctx,
                ViolationSeverity::Hard,
                "Stop-loss not set on opening order",
            );
            return Ok(RuleVerdict::Fail(v));
        }
        Ok(RuleVerdict::Pass)
    }
}
