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
                        crate::config::plan::LossReference::IntradayTrail => {
                            if ctx.account.plan.drawdown_on_balance {
                                ctx.account.peak_balance
                            } else {
                                ctx.account.peak_equity
                            }
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
    // P-failure-policy: expose pack-derived params for registry error mapping.
    fn params(&self) -> Option<&crate::rules::params::RuleParams> {
        self.params.as_ref()
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
        // P0-D + P1.6 + P1#3 + P1#4: compute the floor and drawdown
        // against the *effective* basis. The engine previously used
        // `reference` (= peak/anchor) + `limit = pct × reference` and
        // checked `dd > limit`. That doesn't fit the dataset's
        // `eod_trail` (fixed dollar buffer = pct × initial, not pct × peak)
        // or the `locks_at: start_balance` cap (floor can't exceed
        // initial). Restructured to compute the floor directly.
        //
        // Basis → floor formula:
        //   - Static:           floor = initial × (1 - pct)
        //   - Trailing:         floor = peak_balance × (1 - pct)
        //   - EodTrailing:      floor = day_start - pct × initial
        //                        (with `locks_at_start`: floor = min(unlocked, initial))
        //   - IntradayTrail:    floor = peak_equity × (1 - pct)
        // Breach when `current < floor - tolerance`.
        let basis = self.effective_basis(ctx);
        let current = if ctx.account.plan.drawdown_on_balance {
            ctx.account.balance
        } else {
            equity
        };
        let (floor, reference_for_breach_msg) = match basis {
            crate::config::plan::LossReference::Static => {
                let floor =
                    ctx.account.initial_balance.0 - (plan_pct * ctx.account.initial_balance.0);
                (Money(floor), ctx.account.initial_balance)
            }
            crate::config::plan::LossReference::Trailing => {
                let floor = ctx.account.peak_balance.0 - (plan_pct * ctx.account.peak_balance.0);
                (Money(floor), ctx.account.peak_balance)
            }
            crate::config::plan::LossReference::EodTrailing => {
                // P1#3 + dataset formula: the EOD-trail buffer is a
                // FIXED dollar amount = pct × initial (per the dataset's
                // `eod_trail` description: "A $50,000 account with a 4%
                // trailing drawdown starts with a floor at $48,000.
                // Close the day at $51,000 and the floor moves to
                // $49,000." — $50k→$48k = $2k buffer = 4% × $50k
                // initial; $51k→$49k = $2k buffer, NOT 4% × $51k).
                //
                // This is a behavior change from the previous engine
                // formula (pct × day_start, scaled buffer). The new
                // formula matches every verified EOD-trail firm in the
                // dataset (FTMO 1-Step, TopStep, Apex EOD).
                let buffer = plan_pct * ctx.account.initial_balance.0;
                let unlocked_floor = ctx.account.day_start_balance.0 - buffer;
                let floor = if ctx.account.plan.eod_trail_locks_at_start {
                    // P1#3: floor caps at the starting balance. Once
                    // `unlocked_floor > initial` (i.e., peak > initial ×
                    // (1 + pct)), the floor stops trailing and stays at
                    // `initial`. After the lock engages, the worst case
                    // is returning to breakeven rather than being
                    // breached. Used by TopStep (all 3 Combines),
                    // Breakout 2-Step, FundedNext Stellar Instant,
                    // FundingPips Zero.
                    unlocked_floor.min(ctx.account.initial_balance.0)
                } else {
                    unlocked_floor
                };
                (Money(floor), ctx.account.day_start_balance)
            }
            crate::config::plan::LossReference::IntradayTrail => {
                // P1#4: floor follows the highest **unrealised equity**
                // peak (Account::peak_equity), not the closed-balance
                // peak. Used by Apex's Intraday Trail variant,
                // FundingPips Zero, Breakout 2-Step. Harshest max-DD
                // mechanism in use — an open position that runs into
                // profit and back out can breach you with no closed
                // losing trade.
                let peak = if ctx.account.plan.drawdown_on_balance {
                    ctx.account.peak_balance
                } else {
                    ctx.account.peak_equity
                };
                let floor = peak.0 - (plan_pct * peak.0);
                (Money(floor), peak)
            }
        };
        // dd = drawdown from the reference peak/anchor (for breach
        // messaging + the warning threshold). The breach itself is
        // `current < floor - tolerance`.
        let dd = Money((reference_for_breach_msg.0 - current.0).max(dec!(0)));
        let limit_dollars = Money((reference_for_breach_msg.0 - floor.0).max(dec!(0)));

        // P2 fix: tolerance to absorb broker rounding noise at the boundary.
        let tolerance = self.tolerance_money();
        if current.0 < floor.0 - tolerance.0 {
            let mut v = build_violation(
                self,
                ctx,
                ViolationSeverity::Liquidate,
                format!(
                    "Maximum drawdown breach ({basis:?} mode): current {current} < floor {floor}-tol {tolerance} ({}%)",
                    plan_pct * dec!(100)
                ),
            );
            v = v.with_breach(dd, limit_dollars);
            return Ok(RuleVerdict::Liquidate(v));
        }
        // P1-13: warn at 80% utilization (or pack entry's early_warning_pct).
        let warn_pct = self
            .params
            .as_ref()
            .and_then(super::super::params::RuleParams::early_warning_pct)
            .unwrap_or(dec!(0.8));
        let warn = floor.0 + (limit_dollars.0 * warn_pct);
        if current.0 <= warn {
            let mut v = build_violation(
                self,
                ctx,
                ViolationSeverity::Warning,
                format!(
                    "Maximum drawdown at {dd}/{limit_dollars} ({}%) [{basis:?}]",
                    (dd.0 / limit_dollars.0.max(dec!(1)) * dec!(100)).round_dp(2),
                ),
            );
            v = v.with_breach(dd, limit_dollars);
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
