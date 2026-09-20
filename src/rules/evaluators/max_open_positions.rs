//! Maximum open positions rule.
//!
//! Forbids opening new positions if the total open positions would exceed
//! the configured limit.

use crate::core::ids::RuleId;
use crate::core::violation::{ViolationKind, ViolationSeverity};
use crate::rules::context::{EvaluationScope, RuleContext};
use crate::rules::params::{ParameterizedRule, RuleParams};
use crate::rules::registry::build_violation;
use crate::rules::traits::{Rule, RuleVerdict};

#[derive(Debug, Clone, Default)]
pub struct MaxOpenPositionsRule {
    /// **P0-D fix**: pack-derived parameters. When `Some`, rule reads
    /// `value`/`basis`/`tolerance_cents`/`priority` from here instead of
    /// from `ctx.account.plan` — so a tenant editing the pack actually
    /// changes the verdict. When `None` (constructed via `Default`), the
    /// rule falls back to plan-derived config.
    pub params: Option<RuleParams>,
}

impl MaxOpenPositionsRule {
    /// **P0.4 fix**: effective max open positions — pack entry's value
    /// if set (count semantics, fail-closed), else plan.
    fn effective_max(&self, ctx: &RuleContext) -> Result<Option<u32>, crate::core::Error> {
        if let Some(p) = &self.params {
            if !p.enabled {
                return Ok(None);
            }
            let Some(v) = p.value() else {
                return Ok(None);
            };
            let n = u32::try_from(v).map_err(|_| {
                crate::core::Error::invalid_config(format!(
                    "max_open_positions: pack value {v} is not a valid count"
                ))
            })?;
            return Ok(Some(n));
        }
        Ok(ctx.account.plan.max_open_positions)
    }
}

impl Rule for MaxOpenPositionsRule {
    fn id(&self) -> RuleId {
        RuleId::named("max_open_positions")
    }
    fn name(&self) -> &'static str {
        "Max Open Positions"
    }
    fn kind(&self) -> ViolationKind {
        ViolationKind::MaxOpenPositions
    }
    fn scope(&self) -> EvaluationScope {
        EvaluationScope::PreTrade
    }
    fn severity(&self) -> ViolationSeverity {
        ViolationSeverity::Hard
    }

    fn description(&self) -> &'static str {
        "Forbids opening a new position when the number of currently open positions is at the cap."
    }

    fn is_enabled(&self, ctx: &RuleContext) -> bool {
        // P0.4: pack entry's enabled flag overrides the plan.
        if let Some(p) = &self.params {
            if !p.enabled {
                return false;
            }
            return p.value.is_some();
        }
        ctx.account.plan.max_open_positions.is_some()
    }

    fn evaluate(&self, ctx: &RuleContext) -> crate::Result<RuleVerdict> {
        // P0.4: use the effective limit (pack entry overrides plan).
        let Some(max) = self
            .effective_max(ctx)
            .map_err(|e| crate::core::Error::RuleEval(format!("max_open_positions: {e}")))?
        else {
            return Ok(RuleVerdict::Pass);
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

impl MaxOpenPositionsRule {
    /// Constructs a parameterized rule from a pack entry (P0-D fix).
    #[must_use]
    pub fn from_entry(entry: &crate::rulepack::RuleEntry) -> Self {
        MaxOpenPositionsRule {
            params: Some(RuleParams::from_entry(entry)),
        }
    }
}

impl ParameterizedRule for MaxOpenPositionsRule {
    fn from_entry(entry: &crate::rulepack::RuleEntry) -> Self {
        MaxOpenPositionsRule::from_entry(entry)
    }
}
