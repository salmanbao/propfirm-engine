//! Daily drawdown rule.
//!
//! The maximum amount the account equity can fall in a single trading day,
//! measured from the day's starting balance (or balance at session open).
//!
//! `severity = Hard` because exceeding daily drawdown typically terminates
//! the account immediately.

use crate::core::ids::RuleId;
use crate::core::violation::{ViolationKind, ViolationSeverity};
use crate::rules::context::{EvaluationScope, RuleContext};
use crate::rules::params::{ParameterizedRule, RuleParams};
use crate::rules::registry::build_violation;
use crate::rules::traits::{Rule, RuleVerdict, ViolationBuilder};

/// Maximum daily drawdown rule.
#[derive(Debug, Clone, Default)]
pub struct DailyDrawdownRule {
    /// **P0-D fix**: pack-derived parameters. When `Some`, rule reads
    /// `value`/`basis`/`tolerance_cents`/`priority` from here instead of
    /// from `ctx.account.plan` — so a tenant editing the pack actually
    /// changes the verdict. When `None` (constructed via `Default`), the
    /// rule falls back to plan-derived config.
    pub params: Option<RuleParams>,
}

impl Rule for DailyDrawdownRule {
    fn id(&self) -> RuleId {
        RuleId::named("daily_drawdown")
    }
    fn name(&self) -> &'static str {
        "Daily Drawdown"
    }
    fn kind(&self) -> ViolationKind {
        ViolationKind::DailyDrawdown
    }
    fn scope(&self) -> EvaluationScope {
        EvaluationScope::OnTick
    }
    fn severity(&self) -> ViolationSeverity {
        ViolationSeverity::Hard
    }
    /// P0-D: pack entry's priority overrides default.
    fn priority(&self) -> u32 {
        self.params
            .as_ref()
            .and_then(super::super::params::RuleParams::priority)
            .unwrap_or(900)
    }
    /// P0-D: pack entry's tolerance overrides default.
    fn tolerance_cents(&self) -> i64 {
        self.params
            .as_ref()
            .and_then(super::super::params::RuleParams::tolerance_cents)
            .unwrap_or(1)
    }

    fn description(&self) -> &'static str {
        "Maximum drawdown permitted within a single trading day, measured from the day-start balance."
    }

    fn is_enabled(&self, ctx: &RuleContext) -> bool {
        if let Some(p) = &self.params {
            if !p.enabled {
                return false;
            }
        }
        self.effective_pct(ctx) > rust_decimal::Decimal::ZERO
    }

    fn evaluate(&self, ctx: &RuleContext) -> crate::Result<RuleVerdict> {
        let plan_pct = self.effective_pct(ctx);
        if plan_pct <= rust_decimal::Decimal::ZERO {
            return Ok(RuleVerdict::Pass);
        }
        if let Err(msg) = ctx.require_started() {
            let v = build_violation(self, ctx, ViolationSeverity::Info, msg);
            return Ok(RuleVerdict::GapFlagged(v));
        }
        if ctx.require_broker_equity().is_err() {
            let v = build_violation(
                self,
                ctx,
                ViolationSeverity::Info,
                ctx.require_broker_equity().unwrap_err(),
            );
            return Ok(RuleVerdict::GapFlagged(v));
        }
        // P0-D: compute limit using the pack entry's value (overrides plan).
        // P1.1: when drawdown_on_balance is false (default), the limit is
        // anchored on day_start_equity, not day_start_balance, matching the
        // drawdown computation in Account::daily_drawdown().
        let day_start = if ctx.account.plan.drawdown_on_balance {
            ctx.account.day_start_balance
        } else {
            ctx.account.day_start_equity
        };
        let limit = crate::core::types::Money(plan_pct * day_start.0);
        let dd = ctx.account.daily_drawdown();
        // P2 fix: tolerance to absorb broker rounding noise at the boundary.
        let tolerance = self.tolerance_money();
        if dd.0 > limit.0 + tolerance.0 {
            let mut v = build_violation(
                self,
                ctx,
                ViolationSeverity::Liquidate,
                format!(
                    "Daily drawdown breach: {dd} > {limit}+{tolerance} ({}%)",
                    plan_pct * rust_decimal::Decimal::ONE_HUNDRED
                ),
            );
            v = v.with_breach(dd, limit);
            return Ok(RuleVerdict::Liquidate(v));
        }
        // P1-13: warn at 80% utilization (or pack entry's early_warning_pct).
        let warn_pct = self
            .params
            .as_ref()
            .and_then(super::super::params::RuleParams::early_warning_pct)
            .unwrap_or(rust_decimal::Decimal::new(8, 1));
        let warn_threshold = limit.0 * warn_pct;
        if dd.0 >= warn_threshold {
            let mut v = build_violation(
                self,
                ctx,
                ViolationSeverity::Warning,
                format!(
                    "Daily drawdown at {}/{} ({}%)",
                    dd,
                    limit,
                    (dd.0 / limit.0 * rust_decimal::Decimal::ONE_HUNDRED).round_dp(2)
                ),
            );
            v = v.with_breach(dd, limit);
            return Ok(RuleVerdict::EarlyWarning(v));
        }
        Ok(RuleVerdict::Pass)
    }
}

impl DailyDrawdownRule {
    /// Constructs a parameterized rule from a pack entry (P0-D fix).
    #[must_use]
    pub fn from_entry(entry: &crate::rulepack::RuleEntry) -> Self {
        DailyDrawdownRule {
            params: Some(RuleParams::from_entry(entry)),
        }
    }

    /// Effective daily DD pct — pack entry's value if set, else plan.
    fn effective_pct(&self, ctx: &RuleContext) -> rust_decimal::Decimal {
        if let Some(p) = &self.params {
            if let Some(v) = p.value() {
                return v;
            }
        }
        ctx.account.plan.max_daily_drawdown_pct.0
    }
}

impl ParameterizedRule for DailyDrawdownRule {
    fn from_entry(entry: &crate::rulepack::RuleEntry) -> Self {
        DailyDrawdownRule::from_entry(entry)
    }
}
