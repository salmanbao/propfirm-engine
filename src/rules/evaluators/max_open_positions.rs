//! Maximum open positions rule.
//!
//! Forbids opening new positions if the total open positions would exceed
//! the configured limit.

use crate::core::ids::RuleId;
use crate::core::violation::{ViolationKind, ViolationSeverity};
use crate::rules::context::{EvaluationScope, RuleContext};
use crate::rules::registry::build_violation;
use crate::rules::traits::{Rule, RuleVerdict};

#[derive(Debug, Clone, Default)]
pub struct MaxOpenPositionsRule;

impl Rule for MaxOpenPositionsRule {
    fn id(&self) -> RuleId { RuleId::named("max_open_positions") }
    fn name(&self) -> &str { "Max Open Positions" }
    fn kind(&self) -> ViolationKind { ViolationKind::MaxOpenPositions }
    fn scope(&self) -> EvaluationScope { EvaluationScope::PreTrade }
    fn severity(&self) -> ViolationSeverity { ViolationSeverity::Hard }

    fn description(&self) -> &str {
        "Forbids opening a new position when the number of currently open positions is at the cap."
    }

    fn is_enabled(&self, ctx: &RuleContext) -> bool {
        ctx.account.plan.max_open_positions.is_some()
    }

    fn evaluate(&self, ctx: &RuleContext) -> crate::Result<RuleVerdict> {
        let max = match ctx.account.plan.max_open_positions {
            Some(v) => v,
            None => return Ok(RuleVerdict::Pass),
        };
        let open = ctx.open_positions.iter().filter(|p| p.is_open()).count() as u32;
        if let Some(order) = &ctx.pending_order {
            // Closing orders don't count.
            if matches!(order.kind, crate::core::order::OrderKind::Close { .. }) {
                return Ok(RuleVerdict::Pass);
            }
            if open + 1 > max {
                let v = build_violation(
                    self,
                    ctx,
                    ViolationSeverity::Hard,
                    format!("Open positions {open}+1 would exceed limit {max}"),
                );
                return Ok(RuleVerdict::Fail(v));
            }
        }
        Ok(RuleVerdict::Pass)
    }
}
