//! Per-trade max loss rule (P2.15 fix).
//!
//! Topstep and other futures-oriented prop firms enforce a per-trade
//! loss limit — no single closed trade may lose more than X% of the
//! account balance (or an absolute dollar amount). This is distinct
//! from daily/max drawdown: a trader can blow the entire account on
//! one bad trade if the per-trade rule isn't enforced.

use crate::core::ids::RuleId;
use crate::core::types::dec;
use crate::core::violation::{ViolationKind, ViolationSeverity};
use crate::rules::context::{EvaluationScope, RuleContext};
use crate::rules::registry::build_violation;
use crate::rules::traits::{Rule, RuleVerdict, ViolationBuilder};
use crate::rules::params::{ParameterizedRule, RuleParams};

#[derive(Debug, Clone, Default)]
pub struct PerTradeMaxLossRule {
    pub params: Option<RuleParams>,
}

impl PerTradeMaxLossRule {
    pub fn from_entry(entry: &crate::rulepack::RuleEntry) -> Self {
        PerTradeMaxLossRule { params: Some(RuleParams::from_entry(entry)) }
    }

    /// Effective max per-trade loss as a fraction of balance (default 0.02 = 2%).
    fn effective_max_loss_pct(&self, ctx: &RuleContext) -> rust_decimal::Decimal {
        if let Some(p) = &self.params {
            if let Some(v) = p.value() {
                return v;
            }
        }
        // Default 2% of balance per trade.
        dec!(0.02)
    }
}

impl Rule for PerTradeMaxLossRule {
    fn id(&self) -> RuleId { RuleId::named("per_trade_max_loss") }
    fn name(&self) -> &str { "Per-Trade Max Loss" }
    fn kind(&self) -> ViolationKind { ViolationKind::Custom }
    fn scope(&self) -> EvaluationScope { EvaluationScope::PostTrade }
    fn severity(&self) -> ViolationSeverity { ViolationSeverity::Hard }
    fn priority(&self) -> u32 {
        self.params.as_ref().and_then(|p| p.priority()).unwrap_or(800)
    }
    fn tolerance_cents(&self) -> i64 {
        self.params.as_ref().and_then(|p| p.tolerance_cents()).unwrap_or(1)
    }

    fn description(&self) -> &str {
        "Forbids any single closed trade from losing more than X% of \
         the account balance. Distinct from daily/max drawdown: catches \
         a single bad trade before it blows the account."
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
        let max_loss = crate::core::types::Money(self.effective_max_loss_pct(ctx) * ctx.account.balance.0);
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
                self, ctx, severity,
                format!(
                    "Per-trade max loss breach: trade lost {} > max {} ({}% of balance)",
                    loss_abs, max_loss, self.effective_max_loss_pct(ctx) * dec!(100)
                ),
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
                self, ctx, ViolationSeverity::Warning,
                format!("Per-trade loss approaching limit: {}/{}", loss_abs, max_loss),
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
