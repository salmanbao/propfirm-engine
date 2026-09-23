//! Per-trade max loss rule (P2.15 fix).
//!
//! Topstep and other futures-oriented prop firms enforce a per-trade
//! loss limit — no single closed trade may lose more than X% of the
//! account balance (or an absolute dollar amount). This is distinct
//! from daily/max drawdown: a trader can blow the entire account on
//! one bad trade if the per-trade rule isn't enforced.
//!
//! **P0.1 fix**: this rule is DISABLED by default. It runs only when:
//! - the plan enables it explicitly (`plan.per_trade_max_loss_pct` /
//!   `plan.per_trade_max_loss_money` — see `topstep_futures()`), or
//! - a rule-pack entry binds it (`RuleParams` present with
//!   `enabled: true`).
//!
//! The default registry previously registered it unconditionally with
//! a 2% limit at Liquidate severity — a rule that does not exist in
//! most prop-firm programs — liquidating accounts out of nowhere.
//!
//! **P0.3 fix**: the pack entry's `unit` is honoured via
//! `effective_money`: `Percent` → `value × balance`, `Money` → value
//! as an absolute dollar limit. A mis-encoded unit fails closed.

use crate::core::ids::RuleId;
use crate::core::types::dec;
use crate::core::violation::{ViolationKind, ViolationSeverity};
use crate::rules::context::{EvaluationScope, RuleContext};
use crate::rules::params::{ParameterizedRule, RuleParams};
use crate::rules::registry::build_violation;
use crate::rules::traits::{Rule, RuleVerdict, ViolationBuilder};

#[derive(Debug, Clone, Default)]
pub struct PerTradeMaxLossRule {
    pub params: Option<RuleParams>,
}

impl PerTradeMaxLossRule {
    #[must_use]
    pub fn from_entry(entry: &crate::rulepack::RuleEntry) -> Self {
        PerTradeMaxLossRule {
            params: Some(RuleParams::from_entry(entry)),
        }
    }

    /// **P0.3 fix**: effective per-trade loss limit as `Money`.
    /// Interprets the pack entry's `value` according to its `unit`:
    /// `Percent` → fraction of current balance; `Money` → absolute.
    /// Falls back to the plan fields when no pack entry is bound.
    ///
    /// `Ok(None)` = the rule has no limit configured (caller should
    /// pass); `Err` = mis-encoded pack (fail closed).
    fn effective_limit_money(
        &self,
        ctx: &RuleContext,
    ) -> Result<Option<crate::core::types::Money>, crate::core::Error> {
        if let Some(p) = &self.params {
            // Percent-unit packs interpret against the current balance.
            let reference = ctx.account.balance;
            return match p.unit {
                Some(crate::rulepack::RuleUnit::Money) => {
                    Ok(Some(crate::core::types::Money(p.value.unwrap_or_default())))
                }
                _ => p.effective_money("per_trade_max_loss", reference),
            };
        }
        // Plan fallback (P0.1): the tighter of pct-of-balance and the
        // absolute money cap applies.
        let pct_limit = ctx
            .account
            .plan
            .per_trade_max_loss_pct
            .map(|p| crate::core::types::Money(p.0 * ctx.account.balance.0));
        let money_limit = ctx.account.plan.per_trade_max_loss_money;
        Ok(match (pct_limit, money_limit) {
            (Some(a), Some(b)) => Some(crate::core::types::Money(a.0.min(b.0))),
            (Some(a), None) => Some(a),
            (None, Some(b)) => Some(b),
            (None, None) => None,
        })
    }
}

impl Rule for PerTradeMaxLossRule {
    fn id(&self) -> RuleId {
        RuleId::named("per_trade_max_loss")
    }
    fn name(&self) -> &'static str {
        "Per-Trade Max Loss"
    }
    fn kind(&self) -> ViolationKind {
        ViolationKind::Custom
    }
    fn scope(&self) -> EvaluationScope {
        EvaluationScope::PostTrade
    }
    fn severity(&self) -> ViolationSeverity {
        ViolationSeverity::Hard
    }
    // P-failure-policy: expose pack-derived params for registry error mapping.
    fn params(&self) -> Option<&crate::rules::params::RuleParams> {
        self.params.as_ref()
    }

    fn priority(&self) -> u32 {
        self.params
            .as_ref()
            .and_then(super::super::params::RuleParams::priority)
            .unwrap_or(800)
    }
    fn tolerance_cents(&self) -> i64 {
        self.params
            .as_ref()
            .and_then(super::super::params::RuleParams::tolerance_cents)
            .unwrap_or(1)
    }

    fn description(&self) -> &'static str {
        "Forbids any single closed trade from losing more than X% of \
         the account balance (or an absolute money amount). Distinct \
         from daily/max drawdown: catches a single bad trade before it \
         blows the account. Disabled unless the plan or a pack entry \
         enables it (P0.1)."
    }

    /// **P0.1 fix**: disabled unless the plan or the pack entry
    /// explicitly enables it. Never relies on the trait default `true`.
    fn is_enabled(&self, ctx: &RuleContext) -> bool {
        if let Some(p) = &self.params {
            if !p.enabled {
                return false;
            }
            // A bound pack entry IS the enablement.
            return p.value.is_some();
        }
        ctx.account.plan.per_trade_max_loss_pct.is_some()
            || ctx.account.plan.per_trade_max_loss_money.is_some()
    }

    fn evaluate(&self, ctx: &RuleContext) -> crate::Result<RuleVerdict> {
        let Some(trade) = &ctx.latest_trade else {
            return Ok(RuleVerdict::Pass);
        };
        // Only check exits with realized P&L info.
        if trade.trade_side != crate::core::trade::TradeSide::Exit {
            return Ok(RuleVerdict::Pass);
        }
        let Some(exit_info) = &trade.exit_info else {
            return Ok(RuleVerdict::Pass);
        };
        let loss = exit_info.realized_pnl;
        // Only fires on losses.
        if loss.0 >= dec!(0) {
            return Ok(RuleVerdict::Pass);
        }
        // P0.3: a mis-encoded unit must fail loudly, not silently pass.
        let Some(max_loss) = self.effective_limit_money(ctx)? else {
            return Ok(RuleVerdict::Pass);
        };
        let loss_abs = loss.abs();
        if loss_abs.0 > max_loss.0 + self.tolerance_money().0 {
            // P1-5 fix: refuse to terminate on estimated equity. Realized
            // P&L is always broker-reported (it's the fill from the broker),
            // so this should always be broker-reported in practice.
            let severity = if ctx.equity_is_broker_reported() {
                ViolationSeverity::Liquidate
            } else {
                ViolationSeverity::Warning
            };
            let v = build_violation(
                self,
                ctx,
                severity,
                format!("Per-trade max loss breach: trade lost {loss_abs} > max {max_loss}"),
            );
            return Ok(match severity {
                ViolationSeverity::Liquidate => RuleVerdict::Liquidate(v),
                _ => RuleVerdict::Warn(v),
            });
        }
        // P1-13: EarlyWarning at 80% of per-trade limit.
        let warn_threshold = max_loss.0 * dec!(0.8);
        if loss_abs.0 >= warn_threshold {
            let v = build_violation(
                self,
                ctx,
                ViolationSeverity::Warning,
                format!("Per-trade loss approaching limit: {loss_abs}/{max_loss}"),
            );
            return Ok(RuleVerdict::EarlyWarning(v));
        }
        Ok(RuleVerdict::Pass)
    }
}

impl ParameterizedRule for PerTradeMaxLossRule {
    fn from_entry(entry: &crate::rulepack::RuleEntry) -> Self {
        PerTradeMaxLossRule::from_entry(entry)
    }
}
