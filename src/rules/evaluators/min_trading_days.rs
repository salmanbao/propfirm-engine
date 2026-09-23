//! Minimum trading days rule.
//!
//! Many prop firms require the trader to be active on at least N distinct
//! days before passing a phase. The rule is evaluated periodically and on
//! demand; it produces a warning if the trader hasn't yet met the day count.

use crate::core::ids::RuleId;
use crate::core::violation::{ViolationKind, ViolationSeverity};
use crate::rules::context::{EvaluationScope, RuleContext};
use crate::rules::params::{ParameterizedRule, RuleParams};
use crate::rules::registry::build_violation;
use crate::rules::traits::{Rule, RuleVerdict};

#[derive(Debug, Clone, Default)]
pub struct MinTradingDaysRule {
    /// **P0-D fix**: pack-derived parameters. When `Some`, rule reads
    /// `value`/`basis`/`tolerance_cents`/`priority` from here instead of
    /// from `ctx.account.plan` — so a tenant editing the pack actually
    /// changes the verdict. When `None` (constructed via `Default`), the
    /// rule falls back to plan-derived config.
    pub params: Option<RuleParams>,
}

impl MinTradingDaysRule {
    /// **P0.4 fix**: effective required days — pack entry's value if
    /// set (fail-closed on a bad unit), else plan.
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
                    "min_trading_days: pack value {v} is not a valid day count"
                ))
            })?;
            return Ok(Some(days));
        }
        Ok(Some(ctx.account.plan.min_trading_days))
    }
}

impl Rule for MinTradingDaysRule {
    fn id(&self) -> RuleId {
        RuleId::named("min_trading_days")
    }
    fn name(&self) -> &'static str {
        "Minimum Trading Days"
    }
    fn kind(&self) -> ViolationKind {
        ViolationKind::MinTradingDays
    }
    fn scope(&self) -> EvaluationScope {
        EvaluationScope::Periodic
    }
    fn severity(&self) -> ViolationSeverity {
        ViolationSeverity::Warning
    }
    // P-failure-policy: expose pack-derived params for registry error mapping.
    fn params(&self) -> Option<&crate::rules::params::RuleParams> {
        self.params.as_ref()
    }

    fn description(&self) -> &'static str {
        "Requires a minimum number of distinct active trading days before a phase can be passed."
    }

    fn is_enabled(&self, ctx: &RuleContext) -> bool {
        // P0.4: pack entry's enabled flag overrides the plan.
        if let Some(p) = &self.params {
            if !p.enabled {
                return false;
            }
            return p.value.is_some();
        }
        ctx.account.plan.min_trading_days > 0
    }

    fn evaluate(&self, ctx: &RuleContext) -> crate::Result<RuleVerdict> {
        // P0.4: use the effective required days (pack entry overrides plan).
        let Some(required) = self
            .effective_days(ctx)
            .map_err(|e| crate::core::Error::RuleEval(format!("min_trading_days: {e}")))?
        else {
            return Ok(RuleVerdict::Pass);
        };
        if required == 0 {
            return Ok(RuleVerdict::Pass);
        }
        let actual = ctx.account.active_trading_days;
        if actual >= required {
            return Ok(RuleVerdict::Pass);
        }
        // Only fail if deadline has been reached
        if let Some(deadline) = ctx.account.deadline {
            if ctx.server_time.ts() > deadline && actual < required {
                let v = build_violation(
                    self,
                    ctx,
                    ViolationSeverity::Hard,
                    format!("Deadline reached with only {actual}/{required} active trading days"),
                );
                return Ok(RuleVerdict::Fail(v));
            }
        }
        let v = build_violation(
            self,
            ctx,
            ViolationSeverity::Warning,
            format!("Active trading days {actual}/{required} – phase not yet complete"),
        );
        Ok(RuleVerdict::Warn(v))
    }
}

impl MinTradingDaysRule {
    /// Constructs a parameterized rule from a pack entry (P0-D fix).
    #[must_use]
    pub fn from_entry(entry: &crate::rulepack::RuleEntry) -> Self {
        MinTradingDaysRule {
            params: Some(RuleParams::from_entry(entry)),
        }
    }
}

impl ParameterizedRule for MinTradingDaysRule {
    fn from_entry(entry: &crate::rulepack::RuleEntry) -> Self {
        MinTradingDaysRule::from_entry(entry)
    }
}
