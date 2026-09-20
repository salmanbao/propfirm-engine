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

use crate::core::ids::RuleId;
use crate::core::types::{dec, Money};
use crate::core::violation::{ViolationKind, ViolationSeverity};
use crate::rules::context::{EvaluationScope, RuleContext};
use crate::rules::registry::build_violation;
use crate::rules::traits::{Rule, RuleVerdict, ViolationBuilder};

#[derive(Debug, Clone, Default)]
pub struct MaxDrawdownRule;

impl Rule for MaxDrawdownRule {
    fn id(&self) -> RuleId { RuleId::named("max_drawdown") }
    fn name(&self) -> &str { "Maximum Drawdown" }
    fn kind(&self) -> ViolationKind { ViolationKind::MaxDrawdown }
    fn scope(&self) -> EvaluationScope { EvaluationScope::OnTick }
    fn severity(&self) -> ViolationSeverity { ViolationSeverity::Hard }
    /// Max drawdown is the most severe single-rule breach: it terminates
    /// the account immediately with no grace period. We give it the
    /// highest priority so it always wins when fired alongside another
    /// rule on the same evaluation (P0-4 fix).
    fn priority(&self) -> u32 { 1000 }

    fn description(&self) -> &str {
        "Maximum cumulative drawdown permitted over the life of the account. \
         Reference point (static vs trailing) is set by ChallengePlan::max_loss_reference \
         so limit and drawdown are always measured against the same baseline."
    }

    fn is_enabled(&self, ctx: &RuleContext) -> bool {
        ctx.account.plan.max_total_drawdown_pct.0 > dec!(0)
    }

    fn evaluate(&self, ctx: &RuleContext) -> crate::Result<RuleVerdict> {
        let plan_pct = ctx.account.plan.max_total_drawdown_pct;
        if plan_pct.0 <= dec!(0) {
            return Ok(RuleVerdict::Pass);
        }
        // P0-1 fix: both `limit` and `dd` use the same reference point.
        let limit = ctx.account.max_dd_limit();
        let dd = ctx.account.total_drawdown();
        // P2 fix: tolerance to absorb broker rounding noise at the boundary.
        // Breach is only triggered if dd > limit + tolerance.
        let tolerance = self.tolerance_money();
        if dd.0 > limit.0 + tolerance.0 {
            // P1-5 fix: refuse to terminate on an *estimated* equity. If
            // the broker didn't report equity on this tick, the breach
            // verdict is downgraded to a Warning so ops gets paged but
            // the account isn't terminated on a number that might be a
            // shadow-ledger drift.
            let severity = if ctx.equity_is_broker_reported() {
                ViolationSeverity::Liquidate
            } else {
                ViolationSeverity::Warning
            };
            let mut v = build_violation(
                self,
                ctx,
                severity,
                format!(
                    "Maximum drawdown breach{} ({:?} mode): {dd} > {limit}+{tolerance} ({}%)",
                    if ctx.equity_is_broker_reported() { "" } else { " [ESTIMATED — not terminating]" },
                    ctx.account.plan.max_loss_reference,
                    plan_pct.0 * dec!(100)
                ),
            );
            v = v.with_breach(dd, limit);
            return Ok(match severity {
                ViolationSeverity::Liquidate => RuleVerdict::Liquidate(v),
                _ => RuleVerdict::Warn(v),
            });
        }
        // P1-13: warn at 80% utilization. Emitted as an `EarlyWarning`
        // (ops-paged) — distinct from a trader-facing `Warn`.
        let warn = limit.0 * dec!(0.8);
        if dd.0 >= warn {
            let mut v = build_violation(
                self,
                ctx,
                ViolationSeverity::Warning,
                format!(
                    "Maximum drawdown at {dd}/{limit} ({}%) [{:?}]",
                    (dd.0 / limit.0 * dec!(100)).round_dp(2),
                    ctx.account.plan.max_loss_reference,
                ),
            );
            v = v.with_breach(dd, limit);
            return Ok(RuleVerdict::EarlyWarning(v));
        }
        Ok(RuleVerdict::Pass)
    }
}

/// Helper to compute current drawdown from peak balance, as `Money`.
#[deprecated(note = "use Account::total_drawdown() which respects the static/trailing reference")]
pub fn peak_drawdown(current: Money, peak: Money) -> Money {
    Money((peak.0 - current.0).max(dec!(0)))
}
