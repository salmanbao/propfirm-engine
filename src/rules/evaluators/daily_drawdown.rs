//! Daily drawdown rule.
//!
//! The maximum amount the account equity can fall in a single trading day,
//! measured from the day's starting balance (or balance at session open).
//!
//! `severity = Hard` because exceeding daily drawdown typically terminates
//! the account immediately.

use crate::core::ids::RuleId;
use crate::core::types::{dec, Money};
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
    // P-failure-policy: expose pack-derived params for registry error mapping.
    fn params(&self) -> Option<&crate::rules::params::RuleParams> {
        self.params.as_ref()
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
        use crate::config::plan::DailyLossType;
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

        // P0#1: dispatch on `daily_loss_type` — the engine previously
        // hardcoded `PctPriorDay` (plan_pct × day_start) for every plan,
        // which silently mis-encoded 6 of 9 verified firms in the
        // propfirm-rules-dataset. Now the variant is set per-plan via
        // `ChallengePlan::daily_loss_type`.
        let loss_type = ctx.account.plan.daily_loss_type;
        if matches!(loss_type, DailyLossType::None) {
            return Ok(RuleVerdict::Pass);
        }

        // Pick the reference point + compute the (limit, dd) pair.
        // The reference is what the floor is anchored against:
        //   - PctInitial: initial_balance (limit never moves)
        //   - PctPriorDay: day_start (limit moves with the daily reset)
        //   - TrailingIntradayHigh: intraday_peak_equity (limit trails intraday peak)
        let current = if ctx.account.plan.drawdown_on_balance {
            ctx.account.balance
        } else {
            ctx.account.equity
        };
        let (reference, dd) = match loss_type {
            DailyLossType::None => return Ok(RuleVerdict::Pass),
            DailyLossType::PctInitial => {
                let day_start = if ctx.account.plan.drawdown_on_balance {
                    ctx.account.day_start_balance
                } else {
                    ctx.account.day_start_equity
                };
                (ctx.account.initial_balance, Money((day_start.0 - current.0).max(dec!(0))))
            }
            DailyLossType::PctPriorDay => {
                let day_start = if ctx.account.plan.drawdown_on_balance {
                    ctx.account.day_start_balance
                } else {
                    ctx.account.day_start_equity
                };
                (day_start, Money((day_start.0 - current.0).max(dec!(0))))
            }
            DailyLossType::TrailingIntradayHigh => {
                // For TrailingIntradayHigh, the floor follows the intraday
                // peak equity. The "drawdown" is the drop from that peak,
                // not from day-start. This is the harshest daily form:
                // an open position that runs into profit and back out
                // can breach you with no closed losing trade. Used by
                // HyroTrader Standard.
                //
                // Note: when `drawdown_on_balance = true`, fall back to
                // the all-time `peak_balance` (no intraday_peak_balance
                // tracked today). Conservative — no verified firm uses
                // TrailingIntradayHigh with balance-based drawdown.
                let intraday_peak = if ctx.account.plan.drawdown_on_balance {
                    ctx.account.peak_balance
                } else {
                    ctx.account.intraday_peak_equity
                };
                (
                    intraday_peak,
                    Money((intraday_peak.0 - current.0).max(dec!(0))),
                )
            }
        };
        let limit = crate::core::types::Money(plan_pct * reference.0);
        // P2 fix: tolerance to absorb broker rounding noise at the boundary.
        let tolerance = self.tolerance_money();
        if dd.0 > limit.0 + tolerance.0 {
            // P1#5: when `daily_loss_soft = true` (Apex EOD Trail — the
            // only verified firm whose daily loss is soft), downgrade
            // from `Liquidate` to `Warning`. The breach is recorded
            // but doesn't terminate the account.
            let severity = if ctx.account.plan.daily_loss_soft {
                ViolationSeverity::Warning
            } else {
                ViolationSeverity::Liquidate
            };
            let mut v = build_violation(
                self,
                ctx,
                severity,
                format!(
                    "Daily drawdown breach ({loss_type}): {dd} > {limit}+{tolerance} ({}%)",
                    plan_pct * rust_decimal::Decimal::ONE_HUNDRED
                ),
            );
            v = v.with_breach(dd, limit);
            return Ok(if ctx.account.plan.daily_loss_soft {
                RuleVerdict::EarlyWarning(v)
            } else {
                RuleVerdict::Liquidate(v)
            });
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
                    "Daily drawdown ({loss_type}) at {}/{}/{} ({}%)",
                    dd,
                    limit,
                    reference,
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
