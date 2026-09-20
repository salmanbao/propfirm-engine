//! Grid / martingale trading rule.
//!
//! Detects grid patterns: multiple orders on the same symbol with evenly
//! spaced prices and increasing (martingale) or uniform (grid) lot sizes.
//! Uses a simple heuristic on the recent trades.

use crate::core::ids::RuleId;
use crate::core::types::dec;
use crate::core::violation::{ViolationKind, ViolationSeverity};
use crate::rules::context::{EvaluationScope, RuleContext};
use crate::rules::registry::build_violation;
use crate::rules::traits::{Rule, RuleVerdict};
use rust_decimal::MathematicalOps;

#[derive(Debug, Clone, Default)]
pub struct GridTradingRule;

impl Rule for GridTradingRule {
    fn id(&self) -> RuleId { RuleId::named("grid_trading") }
    fn name(&self) -> &str { "Grid Trading" }
    fn kind(&self) -> ViolationKind { ViolationKind::GridTrading }
    fn scope(&self) -> EvaluationScope { EvaluationScope::PreTrade }
    fn severity(&self) -> ViolationSeverity { ViolationSeverity::Warning }

    fn description(&self) -> &str {
        "Detects suspicious grid / martingale patterns from recent trades."
    }

    fn is_enabled(&self, ctx: &RuleContext) -> bool {
        !ctx.account.plan.grid_trading_allowed
    }

    fn evaluate(&self, ctx: &RuleContext) -> crate::Result<RuleVerdict> {
        if ctx.account.plan.grid_trading_allowed {
            return Ok(RuleVerdict::Pass);
        }
        // We look at recent entry trades on the same symbol as the pending
        // order. If we see >= 3 entries at evenly-spaced prices (within 5%
        // coefficient of variation of price gaps), call it a grid.
        let Some(order) = &ctx.pending_order else {
            return Ok(RuleVerdict::Pass);
        };
        let symbol = &order.symbol;
        let mut entries: Vec<&crate::core::trade::Trade> = ctx
            .today_trades
            .iter()
            .filter(|t| t.symbol == *symbol && t.trade_side == crate::core::trade::TradeSide::Entry)
            .collect();
        entries.sort_by_key(|t| t.executed_at);
        if entries.len() < 3 {
            return Ok(RuleVerdict::Pass);
        }
        // Take the last 5 entries.
        let last: Vec<&crate::core::trade::Trade> = entries.iter().rev().take(5).cloned().collect::<Vec<_>>().into_iter().rev().collect();
        let prices: Vec<rust_decimal::Decimal> = last.iter().map(|t| t.price.0).collect();
        let gaps: Vec<rust_decimal::Decimal> = prices.windows(2).map(|w| (w[1] - w[0]).abs()).collect();
        if gaps.is_empty() {
            return Ok(RuleVerdict::Pass);
        }
        // Coefficient of variation (CV) = std/mean.
        let mean = gaps.iter().sum::<rust_decimal::Decimal>() / rust_decimal::Decimal::from(gaps.len());
        if mean.is_zero() {
            return Ok(RuleVerdict::Pass);
        }
        let var = gaps.iter().map(|g| {
            let d = *g - mean;
            d * d
        }).sum::<rust_decimal::Decimal>() / rust_decimal::Decimal::from(gaps.len());
        let std = var.sqrt().unwrap_or(dec!(0));
        let cv = std / mean;
        if cv < dec!(0.05) {
            // Very uniform spacing → grid-like
            let v = build_violation(
                self,
                ctx,
                ViolationSeverity::Warning,
                format!("Potential grid trading detected on {symbol}: {} uniform entries (CV={cv})", last.len()),
            );
            return Ok(RuleVerdict::Warn(v));
        }
        Ok(RuleVerdict::Pass)
    }
}
