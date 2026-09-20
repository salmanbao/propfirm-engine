//! Maximum daily trades rule.
//!
//! Forbids new orders when the trader has already submitted the maximum
//! number of trades today.

use crate::core::ids::RuleId;
use crate::core::violation::{ViolationKind, ViolationSeverity};
use crate::rules::context::{EvaluationScope, RuleContext};
use crate::rules::params::{ParameterizedRule, RuleParams};
use crate::rules::registry::build_violation;
use crate::rules::traits::{Rule, RuleVerdict};

#[derive(Debug, Clone, Default)]
pub struct MaxDailyTradesRule {
    /// **P0-D fix**: pack-derived parameters. When `Some`, rule reads
    /// `value`/`basis`/`tolerance_cents`/`priority` from here instead of
    /// from `ctx.account.plan` — so a tenant editing the pack actually
    /// changes the verdict. When `None` (constructed via `Default`), the
    /// rule falls back to plan-derived config.
    pub params: Option<RuleParams>,
}

impl MaxDailyTradesRule {
    /// **P0.4 fix**: effective max daily trades — pack entry's value
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
                    "max_daily_trades: pack value {v} is not a valid count"
                ))
            })?;
            return Ok(Some(n));
        }
        Ok(ctx.account.plan.max_daily_trades)
    }
}

impl Rule for MaxDailyTradesRule {
    fn id(&self) -> RuleId {
        RuleId::named("max_daily_trades")
    }
    fn name(&self) -> &'static str {
        "Max Daily Trades"
    }
    fn kind(&self) -> ViolationKind {
        ViolationKind::MaxDailyTrades
    }
    fn scope(&self) -> EvaluationScope {
        EvaluationScope::PreTrade
    }
    fn severity(&self) -> ViolationSeverity {
        ViolationSeverity::Hard
    }

    fn description(&self) -> &'static str {
        "Forbids new orders when the number of trades today has reached the daily cap."
    }

    fn is_enabled(&self, ctx: &RuleContext) -> bool {
        // P0.4: pack entry's enabled flag overrides the plan.
        if let Some(p) = &self.params {
            if !p.enabled {
                return false;
            }
            return p.value.is_some();
        }
        ctx.account.plan.max_daily_trades.is_some()
    }

    fn evaluate(&self, ctx: &RuleContext) -> crate::Result<RuleVerdict> {
        // P0.4: use the effective limit (pack entry overrides plan).
        let Some(max) = self
            .effective_max(ctx)
            .map_err(|e| crate::core::Error::RuleEval(format!("max_daily_trades: {e}")))?
        else {
            return Ok(RuleVerdict::Pass);
        };
        let today = ctx.today_trades.len() as u32;
        if let Some(order) = &ctx.pending_order {
            if matches!(order.kind, crate::core::order::OrderKind::Close { .. }) {
                return Ok(RuleVerdict::Pass);
            }
            if today + 1 > max {
                let v = build_violation(
                    self,
                    ctx,
                    ViolationSeverity::Hard,
                    format!("Daily trade count {today}+1 would exceed limit {max}"),
                );
                return Ok(RuleVerdict::Fail(v));
            }
        }
        Ok(RuleVerdict::Pass)
    }
}

impl MaxDailyTradesRule {
    /// Constructs a parameterized rule from a pack entry (P0-D fix).
    #[must_use]
    pub fn from_entry(entry: &crate::rulepack::RuleEntry) -> Self {
        MaxDailyTradesRule {
            params: Some(RuleParams::from_entry(entry)),
        }
    }
}

impl ParameterizedRule for MaxDailyTradesRule {
    fn from_entry(entry: &crate::rulepack::RuleEntry) -> Self {
        MaxDailyTradesRule::from_entry(entry)
    }
}
