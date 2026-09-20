//! Take-profit required rule.

use crate::core::ids::RuleId;
use crate::core::violation::{ViolationKind, ViolationSeverity};
use crate::rules::context::{EvaluationScope, RuleContext};
use crate::rules::registry::build_violation;
use crate::rules::traits::{Rule, RuleVerdict};
use crate::rules::params::{ParameterizedRule, RuleParams};

#[derive(Debug, Clone, Default)]
pub struct TakeProfitRequiredRule {
    /// **P0-D fix**: pack-derived parameters. When `Some`, rule reads
    /// `value`/`basis`/`tolerance_cents`/`priority` from here instead of
    /// from `ctx.account.plan` — so a tenant editing the pack actually
    /// changes the verdict. When `None` (constructed via `Default`), the
    /// rule falls back to plan-derived config.
    pub params: Option<RuleParams>,
}

impl Rule for TakeProfitRequiredRule {
    fn id(&self) -> RuleId { RuleId::named("tp_required") }
    fn name(&self) -> &str { "Take-Profit Required" }
    fn kind(&self) -> ViolationKind { ViolationKind::MissingTakeProfit }
    fn scope(&self) -> EvaluationScope { EvaluationScope::PreTrade }
    fn severity(&self) -> ViolationSeverity { ViolationSeverity::Warning }

    fn description(&self) -> &str {
        "Requires every new position to have a take-profit set at submission."
    }

    fn is_enabled(&self, ctx: &RuleContext) -> bool {
        ctx.account.plan.require_take_profit
    }

    fn evaluate(&self, ctx: &RuleContext) -> crate::Result<RuleVerdict> {
        if !ctx.account.plan.require_take_profit {
            return Ok(RuleVerdict::Pass);
        }
        let Some(order) = &ctx.pending_order else {
            return Ok(RuleVerdict::Pass);
        };
        if !matches!(order.kind, crate::core::order::OrderKind::Open) {
            return Ok(RuleVerdict::Pass);
        }
        if order.take_profit.is_none() {
            let v = build_violation(
                self,
                ctx,
                ViolationSeverity::Warning,
                "Take-profit not set on opening order",
            );
            return Ok(RuleVerdict::Warn(v));
        }
        Ok(RuleVerdict::Pass)
    }
}

impl TakeProfitRequiredRule {
    /// Constructs a parameterized rule from a pack entry (P0-D fix).
    pub fn from_entry(entry: &crate::rulepack::RuleEntry) -> Self {
        TakeProfitRequiredRule { params: Some(RuleParams::from_entry(entry)) }
    }
}

impl ParameterizedRule for TakeProfitRequiredRule {
    fn from_entry(entry: &crate::rulepack::RuleEntry) -> Self {
        TakeProfitRequiredRule::from_entry(entry)
    }
}
