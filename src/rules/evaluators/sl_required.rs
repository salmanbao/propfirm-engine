//! Stop-loss required rule.

use crate::core::ids::RuleId;
use crate::core::violation::{ViolationKind, ViolationSeverity};
use crate::rules::context::{EvaluationScope, RuleContext};
use crate::rules::params::{ParameterizedRule, RuleParams};
use crate::rules::registry::build_violation;
use crate::rules::traits::{Rule, RuleVerdict};

#[derive(Debug, Clone, Default)]
pub struct StopLossRequiredRule {
    /// **P0-D fix**: pack-derived parameters. When `Some`, rule reads
    /// `value`/`basis`/`tolerance_cents`/`priority` from here instead of
    /// from `ctx.account.plan` — so a tenant editing the pack actually
    /// changes the verdict. When `None` (constructed via `Default`), the
    /// rule falls back to plan-derived config.
    pub params: Option<RuleParams>,
}

impl Rule for StopLossRequiredRule {
    fn id(&self) -> RuleId {
        RuleId::named("sl_required")
    }
    fn name(&self) -> &'static str {
        "Stop-Loss Required"
    }
    fn kind(&self) -> ViolationKind {
        ViolationKind::MissingStopLoss
    }
    fn scope(&self) -> EvaluationScope {
        EvaluationScope::PreTrade
    }
    fn severity(&self) -> ViolationSeverity {
        ViolationSeverity::Hard
    }

    fn description(&self) -> &'static str {
        "Requires every new position to have a stop-loss set at submission."
    }

    fn is_enabled(&self, ctx: &RuleContext) -> bool {
        // P0.4: pack entry's enabled flag overrides the plan.
        if let Some(p) = &self.params {
            return p.enabled;
        }
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

impl StopLossRequiredRule {
    /// Constructs a parameterized rule from a pack entry (P0-D fix).
    #[must_use]
    pub fn from_entry(entry: &crate::rulepack::RuleEntry) -> Self {
        StopLossRequiredRule {
            params: Some(RuleParams::from_entry(entry)),
        }
    }
}

impl ParameterizedRule for StopLossRequiredRule {
    fn from_entry(entry: &crate::rulepack::RuleEntry) -> Self {
        StopLossRequiredRule::from_entry(entry)
    }
}
