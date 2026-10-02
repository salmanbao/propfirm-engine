//! Consistency rule.
//!
//! Prop firms require that no single trading day's profit exceeds a certain
//! percentage (often 30–50%) of a denominator that depends on the firm.
//! This discourages lucky single-day wins and rewards steady performance.
//!
//! **P0#2 fix**: the denominator is now configurable via
//! `ChallengePlan::consistency_type`:
//! - `BestDayPctOfPositiveDays` (default, stricter): denominator is
//!   `sum_positive_days_profit` (only winning days). Used by FTMO 1-Step,
//!   FundingPips Zero, The5%ers 1-Step.
//! - `BestDayPctOfTotal`: denominator is `total_realized_pnl` (all days,
//!   including losing days). Used by HyroTrader (all 4 plans, after the
//!   2026-08-29 correction in the propfirm-rules-dataset).
//! - `None`: rule disabled.
//!
//! The dataset explicitly warns there is no per-trade consistency type —
//! a per-trade rule is frequently attributed to HyroTrader and is wrong.

use crate::core::ids::RuleId;
use crate::core::types::{dec, Money};
use crate::core::violation::{ViolationKind, ViolationSeverity};
use crate::rules::context::{EvaluationScope, RuleContext};
use crate::rules::params::{ParameterizedRule, RuleParams};
use crate::rules::registry::build_violation;
use crate::rules::traits::{Rule, RuleVerdict};

#[derive(Debug, Clone, Default)]
pub struct ConsistencyRule {
    /// **P0-D fix**: pack-derived parameters. When `Some`, rule reads
    /// `value`/`basis`/`tolerance_cents`/`priority` from here instead of
    /// from `ctx.account.plan` — so a tenant editing the pack actually
    /// changes the verdict. When `None` (constructed via `Default`), the
    /// rule falls back to plan-derived config.
    pub params: Option<RuleParams>,
}

impl ConsistencyRule {
    fn effective_cap(
        &self,
        ctx: &RuleContext,
    ) -> Result<Option<rust_decimal::Decimal>, crate::core::Error> {
        if let Some(p) = &self.params {
            if !p.enabled {
                return Ok(None);
            }
            let Some(_) = p.value() else {
                return Ok(None);
            };
            let denom = ctx.account.sum_positive_days_profit;
            if denom.0.is_zero() {
                return Ok(None);
            }
            return Ok(Some(p.effective_pct("consistency", denom)?));
        }
        Ok(ctx.account.plan.consistency_pct.map(|p| p.0))
    }

    fn effective_severity(&self) -> ViolationSeverity {
        if let Some(p) = &self.params {
            if let Some(ref s) = p.severity {
                return match s.as_str() {
                    "hard" | "liquidate" => ViolationSeverity::Hard,
                    _ => ViolationSeverity::Warning,
                };
            }
        }
        ViolationSeverity::Warning
    }
}

impl Rule for ConsistencyRule {
    fn id(&self) -> RuleId {
        RuleId::named("consistency")
    }
    fn name(&self) -> &'static str {
        "Consistency"
    }
    fn kind(&self) -> ViolationKind {
        ViolationKind::Consistency
    }
    fn scope(&self) -> EvaluationScope {
        EvaluationScope::Periodic
    }
    fn severity(&self) -> ViolationSeverity {
        self.effective_severity()
    }
    // P-failure-policy: expose pack-derived params for registry error mapping.
    fn params(&self) -> Option<&crate::rules::params::RuleParams> {
        self.params.as_ref()
    }

    fn description(&self) -> &'static str {
        "Largest single-day profit must not exceed X% of the sum of all positive days' profits."
    }

    fn is_enabled(&self, ctx: &RuleContext) -> bool {
        // P0.4: pack entry's enabled flag overrides the plan.
        if let Some(p) = &self.params {
            if !p.enabled {
                return false;
            }
            return p.value.is_some();
        }
        ctx.account.plan.consistency_pct.is_some()
    }

    fn evaluate(&self, ctx: &RuleContext) -> crate::Result<RuleVerdict> {
        use crate::config::plan::ConsistencyType;
        // P0#2: dispatch on `consistency_type` — the engine previously
        // hardcoded `BestDayPctOfPositiveDays` (denominator =
        // sum_positive_days_profit) for every plan, which silently
        // mis-encoded HyroTrader (all 4 plans use BestDayPctOfTotal,
        // denominator = total_realized_pnl). Now the variant is set
        // per-plan via `ChallengePlan::consistency_type`.
        let consistency_type = ctx.account.plan.consistency_type;
        if matches!(consistency_type, ConsistencyType::None) {
            return Ok(RuleVerdict::Pass);
        }
        // P0.4: use the effective cap (pack entry overrides plan).
        let Some(cap_pct_raw) = self
            .effective_cap(ctx)
            .map_err(|e| crate::core::Error::RuleEval(format!("consistency: {e}")))?
        else {
            return Ok(RuleVerdict::Pass);
        };
        let cap_pct = crate::core::types::Pct(cap_pct_raw);
        // P0#2: pick the denominator per the consistency_type.
        // - BestDayPctOfTotal: total_realized_pnl (includes losing days)
        // - BestDayPctOfPositiveDays: sum_positive_days_profit (winning days only)
        // The latter is stricter — losing days don't dilute the denominator.
        let total_profit = match consistency_type {
            ConsistencyType::None => return Ok(RuleVerdict::Pass),
            ConsistencyType::BestDayPctOfTotal => ctx.account.total_realized_pnl,
            ConsistencyType::BestDayPctOfPositiveDays => ctx.account.sum_positive_days_profit,
        };
        if total_profit.0 <= dec!(0) {
            // No profit yet → nothing to check.
            // For BestDayPctOfTotal: total_realized_pnl <= 0 means net loss or breakeven.
            // For BestDayPctOfPositiveDays: no winning days yet.
            return Ok(RuleVerdict::Pass);
        }
        let largest_day = ctx.account.largest_day_profit;
        if largest_day.0 <= dec!(0) {
            return Ok(RuleVerdict::Pass);
        }
        let cap = Money(cap_pct.0 * total_profit.0);
        if largest_day.0 > cap.0 {
            let mut v = build_violation(
                self,
                ctx,
                ViolationSeverity::Warning,
                format!(
                    "Largest single-day profit {largest_day} exceeds {}% of {} ({cap})",
                    cap_pct.0 * dec!(100),
                    match consistency_type {
                        ConsistencyType::BestDayPctOfTotal => "total_realized_pnl",
                        ConsistencyType::BestDayPctOfPositiveDays => "sum_positive_days_profit",
                        ConsistencyType::None => "n/a",
                    },
                ),
            );
            v = v.with_breach(largest_day, cap);
            return Ok(RuleVerdict::Warn(v));
        }
        Ok(RuleVerdict::Pass)
    }
}

impl ConsistencyRule {
    /// Constructs a parameterized rule from a pack entry (P0-D fix).
    #[must_use]
    pub fn from_entry(entry: &crate::rulepack::RuleEntry) -> Self {
        ConsistencyRule {
            params: Some(RuleParams::from_entry(entry)),
        }
    }
}

impl ParameterizedRule for ConsistencyRule {
    fn from_entry(entry: &crate::rulepack::RuleEntry) -> Self {
        ConsistencyRule::from_entry(entry)
    }
}
