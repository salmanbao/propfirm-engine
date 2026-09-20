//! Hedging rule.
//!
//! Forbids holding opposing positions on the same symbol simultaneously
//! (i.e. hedging).

use crate::core::ids::RuleId;
use crate::core::violation::{ViolationKind, ViolationSeverity};
use crate::rules::context::{EvaluationScope, RuleContext};
use crate::rules::registry::build_violation;
use crate::rules::traits::{Rule, RuleVerdict};

#[derive(Debug, Clone, Default)]
pub struct HedgingRule;

impl Rule for HedgingRule {
    fn id(&self) -> RuleId { RuleId::named("hedging") }
    fn name(&self) -> &str { "Hedging" }
    fn kind(&self) -> ViolationKind { ViolationKind::Hedging }
    fn scope(&self) -> EvaluationScope { EvaluationScope::PreTrade }
    fn severity(&self) -> ViolationSeverity { ViolationSeverity::Hard }

    fn description(&self) -> &str {
        "Forbids holding opposing positions on the same symbol simultaneously."
    }

    fn is_enabled(&self, ctx: &RuleContext) -> bool {
        !ctx.account.plan.hedging_allowed
    }

    fn evaluate(&self, ctx: &RuleContext) -> crate::Result<RuleVerdict> {
        if ctx.account.plan.hedging_allowed {
            return Ok(RuleVerdict::Pass);
        }
        let Some(order) = &ctx.pending_order else {
            return Ok(RuleVerdict::Pass);
        };
        // Only opening a new position matters
        if !matches!(order.kind, crate::core::order::OrderKind::Open) {
            return Ok(RuleVerdict::Pass);
        }
        // Check existing open positions on the same symbol
        let has_opposite = ctx.open_positions.iter().any(|p| {
            p.is_open() && p.symbol == order.symbol && p.side != crate::core::position::PositionSide::from_order(order.side)
        });
        if has_opposite {
            let v = build_violation(
                self,
                ctx,
                ViolationSeverity::Hard,
                format!("Hedging detected: opposing position exists on {}", order.symbol),
            );
            return Ok(RuleVerdict::Fail(v));
        }
        Ok(RuleVerdict::Pass)
    }
}
