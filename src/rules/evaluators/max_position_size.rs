//! Maximum position size rule.
//!
//! Forbids orders exceeding the maximum lot size per order defined in the
//! plan.

use crate::core::ids::RuleId;
use crate::core::types::dec;
use crate::core::violation::{ViolationKind, ViolationSeverity};
use crate::rules::context::{EvaluationScope, RuleContext};
use crate::rules::params::{ParameterizedRule, RuleParams};
use crate::rules::registry::build_violation;
use crate::rules::traits::{Rule, RuleVerdict};

#[derive(Debug, Clone, Default)]
pub struct MaxPositionSizeRule {
    /// **P0-D fix**: pack-derived parameters. When `Some`, rule reads
    /// `value`/`basis`/`tolerance_cents`/`priority` from here instead of
    /// from `ctx.account.plan` — so a tenant editing the pack actually
    /// changes the verdict. When `None` (constructed via `Default`), the
    /// rule falls back to plan-derived config.
    pub params: Option<RuleParams>,
}

impl Rule for MaxPositionSizeRule {
    fn id(&self) -> RuleId {
        RuleId::named("max_position_size")
    }
    fn name(&self) -> &'static str {
        "Max Position Size"
    }
    fn kind(&self) -> ViolationKind {
        ViolationKind::MaxPositionSize
    }
    fn scope(&self) -> EvaluationScope {
        EvaluationScope::PreTrade
    }
    fn severity(&self) -> ViolationSeverity {
        ViolationSeverity::Hard
    }

    fn description(&self) -> &'static str {
        "Forbids orders exceeding the maximum lot size per order."
    }

    fn is_enabled(&self, ctx: &RuleContext) -> bool {
        ctx.account.plan.max_position_lots.is_some()
    }

    fn evaluate(&self, ctx: &RuleContext) -> crate::Result<RuleVerdict> {
        let max_lots = match ctx.account.plan.max_position_lots {
            Some(v) => v,
            None => return Ok(RuleVerdict::Pass),
        };
        let Some(order) = &ctx.pending_order else {
            return Ok(RuleVerdict::Pass);
        };
        let order_lots = order.quantity.0;
        if order_lots > max_lots {
            let v = build_violation(
                self,
                ctx,
                ViolationSeverity::Hard,
                format!("Order lot size {order_lots} exceeds per-order limit {max_lots}"),
            );
            return Ok(RuleVerdict::Fail(v));
        }
        let warn = max_lots * dec!(0.8);
        if order_lots > warn {
            let v = build_violation(
                self,
                ctx,
                ViolationSeverity::Warning,
                format!("Order lot size {order_lots} approaching limit {max_lots}"),
            );
            return Ok(RuleVerdict::Warn(v));
        }
        Ok(RuleVerdict::Pass)
    }
}

impl MaxPositionSizeRule {
    /// Constructs a parameterized rule from a pack entry (P0-D fix).
    #[must_use]
    pub fn from_entry(entry: &crate::rulepack::RuleEntry) -> Self {
        MaxPositionSizeRule {
            params: Some(RuleParams::from_entry(entry)),
        }
    }
}

impl ParameterizedRule for MaxPositionSizeRule {
    fn from_entry(entry: &crate::rulepack::RuleEntry) -> Self {
        MaxPositionSizeRule::from_entry(entry)
    }
}
