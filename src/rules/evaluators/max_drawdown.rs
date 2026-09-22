//! Maximum (total) drawdown rule.
//!
//! The maximum cumulative drawdown from the account's reference point. The
//! reference point is determined by [`ChallengePlan::max_loss_reference`]
//! (see `account.rs::total_drawdown()`):
//!
//! - `Static` ⇒ measured from `initial_balance` (floor never moves).
//! - `Trailing` ⇒ measured from `peak_balance` (floor floats up).
//!
//! Both `limit` and `dd` are computed against the *same* reference (P0-1 fix)
//! — previously the limit was always `pct × initial_balance` while the dd
//! was `peak_balance - balance`, which silently behaved like an undocumented
//! trailing mode and would misfire against the FTMO-style static preset.
//!
//! **P0-D fix**: the rule now holds an optional [`RuleParams`] populated
//! from a [`RuleEntry`](crate::rulepack::RuleEntry). When present, the
//! rule reads its `value`/`basis`/`tolerance_cents`/`priority` from there
//! instead of from `ctx.account.plan` — so a tenant editing the rule
//! pack through the form actually changes the verdict.

use crate::core::ids::RuleId;
use crate::core::types::{dec, Money};
use crate::core::violation::{ViolationKind, ViolationSeverity};
use crate::rules::context::{EvaluationScope, RuleContext};
use crate::rules::params::{ParameterizedRule, RuleParams};
use crate::rules::registry::build_violation;
use crate::rules::traits::{Rule, RuleVerdict, ViolationBuilder};

#[derive(Debug, Clone, Default)]
pub struct MaxDrawdownRule {
    /// **P0-D fix**: pack-derived parameters. When `Some`, the rule
    /// reads its `value`/`basis`/`tolerance_cents`/`priority` from here
    /// instead of from `ctx.account.plan`. When `None` (constructed via
    /// `Default`), the rule falls back to plan-derived config —
    /// backwards-compatible with the existing pipeline path.
    pub params: Option<RuleParams>,
}

impl MaxDrawdownRule {
    /// Constructs a parameterized rule from a pack entry (P0-D fix).
    #[must_use]
    pub fn from_entry(entry: &crate::rulepack::RuleEntry) -> Self {
        MaxDrawdownRule {
            params: Some(RuleParams::from_entry(entry)),
        }
    }

    /// Effective drawdown pct — pack entry's value if set, else plan.
    ///
    /// **P0.3 fix**: the pack entry's `unit` is honoured: `Percent`
    /// values are fractions of the plan's initial balance; `Money`
    /// values are absolute limits normalized against the reference so
    /// the comparison below stays in money space.
    fn effective_pct(&self, ctx: &RuleContext) -> rust_decimal::Decimal {
        if let Some(p) = &self.params {
            if let Some(v) = p.value() {
                if matches!(p.unit, Some(crate::rulepack::RuleUnit::Money)) {
                    let reference = match self.effective_basis(ctx) {
                        crate::config::plan::LossReference::Static => ctx.account.initial_balance,
                        crate::config::plan::LossReference::Trailing => ctx.account.peak_balance,
                        crate::config::plan::LossReference::EodTrailing => {
                            ctx.account.day_start_balance
                        }
                    };
                    if reference.0.is_zero() {
                        return v;
                    }
                    return v / reference.0;
                }
                return v;
            }
        }
        ctx.account.plan.max_total_drawdown_pct.0
    }

    /// Effective loss reference — pack entry's basis if set, else plan's.
    fn effective_basis(&self, ctx: &RuleContext) -> crate::config::plan::LossReference {
        if let Some(p) = &self.params {
            if let Some(b) = p.basis() {
                return b;
            }
        }
        ctx.account.plan.max_loss_reference
    }
}

impl Rule for MaxDrawdownRule {
    fn id(&self) -> RuleId {
        RuleId::named("max_drawdown")
    }
    fn name(&self) -> &'static str {
        "Maximum Drawdown"
    }
    fn kind(&self) -> ViolationKind {
        ViolationKind::MaxDrawdown
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
            .unwrap_or(1000)
    }
    /// P0-D: pack entry's tolerance overrides default.
    fn tolerance_cents(&self) -> i64 {
        self.params
            .as_ref()
            .and_then(super::super::params::RuleParams::tolerance_cents)
            .unwrap_or(1)
    }

    fn description(&self) -> &'static str {
        "Maximum cumulative drawdown permitted over the life of the account. \
         Reference point (static vs trailing vs eod_trailing) is set by \
         ChallengePlan::max_loss_reference or the rule-pack entry's basis \
         field — so limit and drawdown are always measured against the \
         same baseline."
    }

    fn is_enabled(&self, ctx: &RuleContext) -> bool {
        // P0-D: if the pack entry explicitly disabled this rule, honor it.
        if let Some(p) = &self.params {
            if !p.enabled {
                return false;
            }
        }
        // Otherwise: enabled iff there's a non-zero threshold to check
        // (either from pack or plan).
        self.effective_pct(ctx) > dec!(0)
    }

    fn evaluate(&self, ctx: &RuleContext) -> crate::Result<RuleVerdict> {
        let plan_pct = self.effective_pct(ctx);
        if plan_pct <= dec!(0) {
            return Ok(RuleVerdict::Pass);
        }
        if let Err(msg) = ctx.require_started() {
            let v = build_violation(self, ctx, ViolationSeverity::Info, msg);
            return Ok(RuleVerdict::GapFlagged(v));
        }
        let Ok(equity) = ctx.require_broker_equity() else {
            let v = build_violation(
                self,
                ctx,
                ViolationSeverity::Info,
                ctx.require_broker_equity().unwrap_err(),
            );
            return Ok(RuleVerdict::GapFlagged(v));
        };
        // P0-D + P1.6: compute the limit and drawdown against the
        // *effective* basis (pack entry overrides plan; supports static,
        // trailing, and eod_trailing). Both use the same reference.
        let basis = self.effective_basis(ctx);
        let reference = match basis {
            crate::config::plan::LossReference::Static => ctx.account.initial_balance,
            crate::config::plan::LossReference::Trailing => ctx.account.peak_balance,
            crate::config::plan::LossReference::EodTrailing => ctx.account.day_start_balance,
        };
        let limit = Money(plan_pct * reference.0);
        let current = if ctx.account.plan.drawdown_on_balance {
            ctx.account.balance
        } else {
            equity
        };
        let dd = Money((reference.0 - current.0).max(dec!(0)));

        // P2 fix: tolerance to absorb broker rounding noise at the boundary.
        let tolerance = self.tolerance_money();
        if dd.0 > limit.0 + tolerance.0 {
            let mut v = build_violation(
                self,
                ctx,
                ViolationSeverity::Liquidate,
                format!(
                    "Maximum drawdown breach ({:?} mode): {dd} > {limit}+{tolerance} ({}%)",
                    basis,
                    plan_pct * dec!(100)
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
            .unwrap_or(dec!(0.8));
        let warn = limit.0 * warn_pct;
        if dd.0 >= warn {
            let mut v = build_violation(
                self,
                ctx,
                ViolationSeverity::Warning,
                format!(
                    "Maximum drawdown at {dd}/{limit} ({}%) [{:?}]",
                    (dd.0 / limit.0 * dec!(100)).round_dp(2),
                    basis,
                ),
            );
            v = v.with_breach(dd, limit);
            return Ok(RuleVerdict::EarlyWarning(v));
        }
        Ok(RuleVerdict::Pass)
    }
}

impl ParameterizedRule for MaxDrawdownRule {
    fn from_entry(entry: &crate::rulepack::RuleEntry) -> Self {
        MaxDrawdownRule::from_entry(entry)
    }
}

/// Helper to compute current drawdown from peak balance, as `Money`.
#[deprecated(note = "use Account::total_drawdown() which respects the static/trailing reference")]
#[must_use]
pub fn peak_drawdown(current: Money, peak: Money) -> Money {
    Money((peak.0 - current.0).max(dec!(0)))
}
