//! Minimum profitable trading days rule (P1#6 fix).
//!
//! Distinct from [`MinTradingDaysRule`](super::min_trading_days::MinTradingDaysRule):
//! counts only **profitable** days (days where `today_realized_pnl > 0` at
//! rollover), not any trade-day. Used by FundingPips Zero (requires 7
//! profitable days). Mirrors the `min_profitable_days` field in the
//! propfirm-rules-dataset schema.
//!
//! The rule is evaluated periodically and on demand; it produces a warning
//! if the trader hasn't yet met the profitable-day count, and a hard fail
//! if the deadline has been reached without meeting it (mirroring the
//! MinTradingDaysRule's behavior).

use crate::core::ids::RuleId;
use crate::core::violation::{ViolationKind, ViolationSeverity};
use crate::rules::context::{EvaluationScope, RuleContext};
use crate::rules::params::{ParameterizedRule, RuleParams};
use crate::rules::registry::build_violation;
use crate::rules::traits::{Rule, RuleVerdict};

#[derive(Debug, Clone, Default)]
pub struct MinProfitableDaysRule {
    /// Pack-derived parameters. When `Some`, rule reads `value` from here
    /// instead of `ctx.account.plan.min_profitable_days`. When `None`
    /// (constructed via `Default`), the rule falls back to plan-derived
    /// config.
    pub params: Option<RuleParams>,
}

impl MinProfitableDaysRule {
    /// Effective required profitable days — pack entry's value if set,
    /// else plan's `min_profitable_days`.
    fn effective_days(&self, ctx: &RuleContext) -> Result<Option<u32>, crate::core::Error> {
        if let Some(p) = &self.params {
            if !p.enabled {
                return Ok(None);
            }
            let Some(v) = p.value() else {
                return Ok(None);
            };
            let days = u32::try_from(v).map_err(|_| {
                crate::core::Error::invalid_config(format!(
                    "min_profitable_days: pack value {v} is not a valid day count"
                ))
            })?;
            return Ok(Some(days));
        }
        Ok(ctx.account.plan.min_profitable_days)
    }
}

impl Rule for MinProfitableDaysRule {
    fn id(&self) -> RuleId {
        RuleId::named("min_profitable_days")
    }
    fn name(&self) -> &'static str {
        "Minimum Profitable Trading Days"
    }
    fn kind(&self) -> ViolationKind {
        ViolationKind::MinProfitableDays
    }
    fn scope(&self) -> EvaluationScope {
        EvaluationScope::Periodic
    }
    fn severity(&self) -> ViolationSeverity {
        ViolationSeverity::Warning
    }
    fn params(&self) -> Option<&crate::rules::params::RuleParams> {
        self.params.as_ref()
    }
    fn description(&self) -> &'static str {
        "Requires a minimum number of profitable trading days (days with \
         positive net P&L at rollover) before a phase can be passed. \
         Distinct from min_trading_days, which counts any trade-day."
    }
    fn is_enabled(&self, ctx: &RuleContext) -> bool {
        if let Some(p) = &self.params {
            if !p.enabled {
                return false;
            }
            return p.value.is_some();
        }
        ctx.account.plan.min_profitable_days.is_some()
    }

    fn evaluate(&self, ctx: &RuleContext) -> crate::Result<RuleVerdict> {
        let Some(required) = self
            .effective_days(ctx)
            .map_err(|e| crate::core::Error::RuleEval(format!("min_profitable_days: {e}")))?
        else {
            return Ok(RuleVerdict::Pass);
        };
        if required == 0 {
            return Ok(RuleVerdict::Pass);
        }
        let actual = ctx.account.profitable_days_count;
        if actual >= required {
            return Ok(RuleVerdict::Pass);
        }
        // Hard-fail only if the deadline has been reached without meeting
        // the profitable-day count. Mirrors MinTradingDaysRule behavior.
        if let Some(deadline) = ctx.account.deadline {
            if ctx.server_time.ts() > deadline && actual < required {
                let v = build_violation(
                    self,
                    ctx,
                    ViolationSeverity::Hard,
                    format!(
                        "Deadline reached with only {actual}/{required} profitable trading days"
                    ),
                );
                return Ok(RuleVerdict::Fail(v));
            }
        }
        let v = build_violation(
            self,
            ctx,
            ViolationSeverity::Warning,
            format!("Profitable trading days {actual}/{required} – phase not yet complete"),
        );
        Ok(RuleVerdict::Warn(v))
    }
}

impl MinProfitableDaysRule {
    /// Constructs a parameterized rule from a pack entry.
    #[must_use]
    pub fn from_entry(entry: &crate::rulepack::RuleEntry) -> Self {
        MinProfitableDaysRule {
            params: Some(RuleParams::from_entry(entry)),
        }
    }
}

impl ParameterizedRule for MinProfitableDaysRule {
    fn from_entry(entry: &crate::rulepack::RuleEntry) -> Self {
        MinProfitableDaysRule::from_entry(entry)
    }
}
