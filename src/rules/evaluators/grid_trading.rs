//! Grid / martingale trading rule.
//!
//! Detects grid patterns: multiple entry orders on the same symbol with
//! evenly spaced prices (low coefficient of variation of the price gaps).
//!
//! **Pack-driven resolution (§B fix)**: two thresholds are pack-settable —
//!
//! - the entry's `value` is the **minimum entry count** that constitutes a
//!   grid (interpreted through `unit` via [`RuleParams::effective_count`];
//!   uninterpretable units fail closed);
//! - the coefficient-of-variation threshold may additionally be set via
//!   `params_json: {"cv_threshold": 0.05}` (default 0.05).
//!
//! When `None` (constructed via `Default`), the rule falls back to the
//! built-in defaults (3 entries, CV < 0.05).

use crate::core::ids::RuleId;
use crate::core::types::dec;
use crate::core::violation::{ViolationKind, ViolationSeverity};
use crate::rules::context::{EvaluationScope, RuleContext};
use crate::rules::params::{ParameterizedRule, RuleParams};
use crate::rules::registry::build_violation;
use crate::rules::traits::{Rule, RuleVerdict};
use rust_decimal::prelude::ToPrimitive;
use rust_decimal::MathematicalOps;

#[derive(Debug, Clone, Default)]
pub struct GridTradingRule {
    /// **P0-D fix**: pack-derived parameters. When `Some`, the rule reads
    /// the min-entry count from the entry's `value` and the CV threshold
    /// from `params_json`. When `None` (constructed via `Default`), the
    /// rule falls back to the built-in defaults.
    pub params: Option<RuleParams>,
}

/// Default coefficient-of-variation threshold below which entry spacing is
/// considered "uniform" (grid-like).
pub const DEFAULT_CV_THRESHOLD: rust_decimal::Decimal = dec!(0.05);
/// Default minimum number of same-symbol entries that constitute a grid.
pub const DEFAULT_MIN_ENTRIES: usize = 3;

impl GridTradingRule {
    /// Resolves the effective minimum-entry count (pack value → default).
    fn effective_min_entries(&self) -> Result<usize, crate::core::Error> {
        if let Some(p) = &self.params {
            let n = p.effective_count("grid_trading")?;
            return n.to_i64().map(|v| v as usize).ok_or_else(|| {
                crate::core::Error::invalid_config(
                    "grid_trading: pack value is not a valid entry count",
                )
            });
        }
        Ok(DEFAULT_MIN_ENTRIES)
    }

    /// Resolves the effective CV threshold (pack `params_json` → default).
    #[must_use]
    fn effective_cv_threshold(&self) -> rust_decimal::Decimal {
        self.params
            .as_ref()
            .and_then(|p| p.json_decimal("cv_threshold"))
            .unwrap_or(DEFAULT_CV_THRESHOLD)
    }
}

impl Rule for GridTradingRule {
    fn id(&self) -> RuleId {
        RuleId::named("grid_trading")
    }
    fn name(&self) -> &'static str {
        "Grid Trading"
    }
    fn kind(&self) -> ViolationKind {
        ViolationKind::GridTrading
    }
    fn scope(&self) -> EvaluationScope {
        EvaluationScope::PreTrade
    }
    fn severity(&self) -> ViolationSeverity {
        ViolationSeverity::Warning
    }

    fn description(&self) -> &'static str {
        "Detects suspicious grid patterns: many same-symbol entries at uniform price spacing."
    }

    fn is_enabled(&self, ctx: &RuleContext) -> bool {
        // P0.4: pack entry's enabled flag overrides the plan. A grid
        // pack entry means "grid forbidden" when enabled.
        if let Some(p) = &self.params {
            return p.enabled && !ctx.account.plan.grid_trading_allowed;
        }
        !ctx.account.plan.grid_trading_allowed
    }

    fn evaluate(&self, ctx: &RuleContext) -> crate::Result<RuleVerdict> {
        if ctx.account.plan.grid_trading_allowed {
            return Ok(RuleVerdict::Pass);
        }
        let min_entries = self.effective_min_entries()?;
        let cv_threshold = self.effective_cv_threshold();
        // We look at recent entry trades on the same symbol as the pending
        // order. If we see >= min_entries at evenly-spaced prices (CV of the
        // price gaps below the pack-configured threshold), call it a grid.
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
        if entries.len() < min_entries {
            return Ok(RuleVerdict::Pass);
        }
        // Take the last 5 entries.
        let last: Vec<&crate::core::trade::Trade> = entries
            .iter()
            .rev()
            .take(5)
            .copied()
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        let prices: Vec<rust_decimal::Decimal> = last.iter().map(|t| t.price.0).collect();
        let gaps: Vec<rust_decimal::Decimal> =
            prices.windows(2).map(|w| (w[1] - w[0]).abs()).collect();
        if gaps.is_empty() {
            return Ok(RuleVerdict::Pass);
        }
        // Coefficient of variation (CV) = std/mean.
        let mean =
            gaps.iter().sum::<rust_decimal::Decimal>() / rust_decimal::Decimal::from(gaps.len());
        if mean.is_zero() {
            return Ok(RuleVerdict::Pass);
        }
        let var = gaps
            .iter()
            .map(|g| {
                let d = *g - mean;
                d * d
            })
            .sum::<rust_decimal::Decimal>()
            / rust_decimal::Decimal::from(gaps.len());
        let std = var.sqrt().unwrap_or(dec!(0));
        let cv = std / mean;
        if cv < cv_threshold {
            // Very uniform spacing → grid-like
            let v = build_violation(
                self,
                ctx,
                ViolationSeverity::Warning,
                format!(
                    "Potential grid trading detected on {symbol}: {} uniform entries (CV={cv})",
                    last.len()
                ),
            );
            return Ok(RuleVerdict::Warn(v));
        }
        Ok(RuleVerdict::Pass)
    }
}

impl GridTradingRule {
    /// Constructs a parameterized rule from a pack entry (P0-D fix).
    #[must_use]
    pub fn from_entry(entry: &crate::rulepack::RuleEntry) -> Self {
        GridTradingRule {
            params: Some(RuleParams::from_entry(entry)),
        }
    }
}

impl ParameterizedRule for GridTradingRule {
    fn from_entry(entry: &crate::rulepack::RuleEntry) -> Self {
        GridTradingRule::from_entry(entry)
    }
}
