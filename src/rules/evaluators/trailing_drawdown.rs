//! Trailing drawdown rule.
//!
//! Unlike static max drawdown (anchored at the initial balance or a fixed
//! peak), trailing drawdown trails the peak by a fixed percentage and
//! terminates the account if equity falls below the trail.

use crate::core::ids::RuleId;
use crate::core::types::{dec, Money};
use crate::core::violation::{ViolationKind, ViolationSeverity};
use crate::rules::context::{EvaluationScope, RuleContext};
use crate::rules::params::{ParameterizedRule, RuleParams};
use crate::rules::registry::build_violation;
use crate::rules::traits::{Rule, RuleVerdict};

#[derive(Debug, Clone, Default)]
pub struct TrailingDrawdownRule {
    /// **P0-D fix**: pack-derived parameters. When `Some`, rule reads
    /// `value`/`basis`/`tolerance_cents`/`priority` from here instead of
    /// from `ctx.account.plan` — so a tenant editing the pack actually
    /// changes the verdict. When `None` (constructed via `Default`), the
    /// rule falls back to plan-derived config.
    pub params: Option<RuleParams>,
}

impl TrailingDrawdownRule {
    /// Constructs a parameterized rule from a pack entry (P0-D fix).
    #[must_use]
    pub fn from_entry(entry: &crate::rulepack::RuleEntry) -> Self {
        TrailingDrawdownRule {
            params: Some(RuleParams::from_entry(entry)),
        }
    }

    /// **P0.4 fix**: effective trail pct — pack entry's value if set
    /// (unit-aware: Percent → fraction of peak; Money → absolute),
    /// else plan.
    fn effective_trail_money(
        &self,
        ctx: &RuleContext,
    ) -> Result<Option<crate::core::types::Money>, crate::core::Error> {
        if let Some(p) = &self.params {
            if !p.enabled {
                return Ok(None);
            }
            let Some(v) = p.value() else {
                return Ok(None);
            };
            return match p.unit {
                Some(crate::rulepack::RuleUnit::Money) => Ok(Some(crate::core::types::Money(v))),
                _ => Ok(Some(crate::core::types::Money(
                    v * ctx.account.peak_equity.0,
                ))),
            };
        }
        if !ctx.account.plan.trailing_drawdown_enabled {
            return Ok(None);
        }
        let pct = ctx.account.plan.trailing_drawdown_pct;
        Ok(Some(crate::core::types::Money(
            pct.0 * ctx.account.peak_equity.0,
        )))
    }
}

impl Rule for TrailingDrawdownRule {
    fn id(&self) -> RuleId {
        RuleId::named("trailing_drawdown")
    }
    fn name(&self) -> &'static str {
        "Trailing Drawdown"
    }
    fn kind(&self) -> ViolationKind {
        ViolationKind::TrailingDrawdown
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

    fn description(&self) -> &'static str {
        "Drawdown limit that trails the peak equity. Terminate if equity falls below (peak - trail%)."
    }

    fn is_enabled(&self, ctx: &RuleContext) -> bool {
        // P0.4: pack entry's enabled flag overrides the plan.
        if let Some(p) = &self.params {
            if !p.enabled {
                return false;
            }
            return p.value.is_some();
        }
        ctx.account.plan.trailing_drawdown_enabled
            && ctx.account.plan.trailing_drawdown_pct.0 > dec!(0)
    }

    fn evaluate(&self, ctx: &RuleContext) -> crate::Result<RuleVerdict> {
        // P0.4: use the effective trail (pack entry overrides plan).
        // A mis-encoded unit fails closed with a Hard violation.
        let Some(trail_amount) = self
            .effective_trail_money(ctx)
            .map_err(|e| crate::core::Error::RuleEval(format!("trailing_drawdown: {e}")))?
        else {
            return Ok(RuleVerdict::Pass);
        };
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
        let peak = ctx.account.peak_equity;
        let floor = Money(peak.0 - trail_amount.0);
        if equity.0 < floor.0 {
            let breach = Money(floor.0 - equity.0);
            let mut v = build_violation(
                self,
                ctx,
                ViolationSeverity::Liquidate,
                format!(
                    "Trailing drawdown breach: equity {equity} below floor {floor} (trail={trail_amount})",
                ),
            );
            v = v.with_breach(breach, trail_amount);
            return Ok(RuleVerdict::Liquidate(v));
        }
        // P1-13: EarlyWarning at 80% of trail distance.
        let warn_floor = Money(floor.0 + trail_amount.0 * dec!(0.2));
        if equity.0 < warn_floor.0 {
            let mut v = build_violation(
                self,
                ctx,
                ViolationSeverity::Warning,
                format!("Equity approaching trailing drawdown floor: {equity} vs floor {floor}"),
            );
            v = v.with_breach(Money(warn_floor.0 - equity.0), trail_amount);
            return Ok(RuleVerdict::EarlyWarning(v));
        }
        Ok(RuleVerdict::Pass)
    }
}

impl ParameterizedRule for TrailingDrawdownRule {
    fn from_entry(entry: &crate::rulepack::RuleEntry) -> Self {
        TrailingDrawdownRule::from_entry(entry)
    }
}
