//! Copy-trading detection rule.
//!
//! Detects cross-account copy-trading: an account repeatedly trading the
//! same symbol, side and near-identical size at (nearly) the same time as
//! *other* accounts — the signature of a copied signal.
//!
//! **Why the old logic was deleted (§A.2)**: the previous implementation
//! scanned `ctx.recent_events` for `TradeFilled` records belonging to the
//! *same account* — structurally incapable of detecting cross-account
//! copying. It has been replaced with a comparison against the cross-account
//! reference trades supplied on [`RuleContext::cross_reference_trades`]
//! (populated by the platform bridge with trades from other accounts in the
//! same window; never this account's own trades).
//!
//! **Pack-driven resolution (§B fix)**: the entry's `value` is the number
//! of correlated reference trades required for a `Fail` (≥1 correlated
//! trade produces a `Warn`), and `params_json: {"window_seconds": 5}`
//! sets the correlation window (default 5 s). Both fail closed on
//! uninterpretable values.

use crate::core::ids::RuleId;
use crate::core::violation::{ViolationKind, ViolationSeverity};
use crate::rules::context::{EvaluationScope, RuleContext};
use crate::rules::params::{ParameterizedRule, RuleParams};
use crate::rules::registry::build_violation;
use crate::rules::traits::{Rule, RuleVerdict};
use rust_decimal::prelude::ToPrimitive;

#[derive(Debug, Clone, Default)]
pub struct CopyTradingRule {
    /// **P0-D fix**: pack-derived parameters. When `Some`, the rule reads
    /// the fail threshold from the entry's `value` and the correlation
    /// window from `params_json`. When `None` (constructed via
    /// `Default`), the rule falls back to the built-in defaults.
    pub params: Option<RuleParams>,
}

/// Default number of correlated reference trades required for a `Fail`.
pub const DEFAULT_FAIL_THRESHOLD: usize = 3;
/// Default correlation window in seconds.
pub const DEFAULT_WINDOW_SECONDS: i64 = 5;

/// Allowed relative quantity deviation between the trader's fill and a
/// reference fill for the two to count as "the same size" (±10%).
pub const QUANTITY_TOLERANCE: rust_decimal::Decimal = crate::core::types::dec!(0.10);

impl CopyTradingRule {
    /// Resolves the effective fail threshold (pack value → default).
    fn effective_fail_threshold(&self) -> Result<usize, crate::core::Error> {
        if let Some(p) = &self.params {
            let n = p.effective_count("copy_trading")?;
            return n.to_i64().map(|v| v as usize).ok_or_else(|| {
                crate::core::Error::invalid_config(
                    "copy_trading: pack value is not a valid trade count",
                )
            });
        }
        Ok(DEFAULT_FAIL_THRESHOLD)
    }

    /// Resolves the effective correlation window (pack `params_json` →
    /// default).
    #[must_use]
    fn effective_window_seconds(&self) -> i64 {
        self.params
            .as_ref()
            .and_then(|p| p.json_decimal("window_seconds"))
            .and_then(|d| d.to_i64())
            .unwrap_or(DEFAULT_WINDOW_SECONDS)
    }
}

impl Rule for CopyTradingRule {
    fn id(&self) -> RuleId {
        RuleId::named("copy_trading")
    }
    fn name(&self) -> &'static str {
        "Copy Trading"
    }
    fn kind(&self) -> ViolationKind {
        ViolationKind::CopyTrading
    }
    fn scope(&self) -> EvaluationScope {
        EvaluationScope::PostTrade
    }
    fn severity(&self) -> ViolationSeverity {
        ViolationSeverity::Hard
    }

    fn description(&self) -> &'static str {
        "Detects cross-account copy-trading: same symbol/side/size as other accounts within a tight time window."
    }

    fn is_enabled(&self, ctx: &RuleContext) -> bool {
        // P0.4: pack entry's enabled flag overrides the plan. A copy
        // pack entry means "copy trading forbidden" when enabled.
        if let Some(p) = &self.params {
            return p.enabled && !ctx.account.plan.copy_trading_allowed;
        }
        !ctx.account.plan.copy_trading_allowed
    }

    fn evaluate(&self, ctx: &RuleContext) -> crate::Result<RuleVerdict> {
        if ctx.account.plan.copy_trading_allowed {
            return Ok(RuleVerdict::Pass);
        }
        let fail_threshold = self.effective_fail_threshold()?;
        let window_seconds = self.effective_window_seconds();
        let Some(trade) = &ctx.latest_trade else {
            return Ok(RuleVerdict::Pass);
        };
        // Compare the trader's fill against the *cross-account reference*
        // feed. These are trades from other accounts (see
        // `RuleContext::cross_reference_trades`); the pipeline never puts
        // this account's own trades in that list.
        let mut hits = 0usize;
        for reference in &ctx.cross_reference_trades {
            if reference.account_id == trade.account_id {
                continue; // defensive: never correlate with self
            }
            if reference.symbol != trade.symbol {
                continue;
            }
            if reference.side != trade.side {
                continue;
            }
            if (reference.executed_at - trade.executed_at)
                .num_seconds()
                .abs()
                > window_seconds
            {
                continue;
            }
            // Quantity similarity: reference ± tolerance contains trade qty.
            let lower = reference.quantity.0 * (rust_decimal::Decimal::ONE - QUANTITY_TOLERANCE);
            let upper = reference.quantity.0 * (rust_decimal::Decimal::ONE + QUANTITY_TOLERANCE);
            if trade.quantity.0 >= lower && trade.quantity.0 <= upper {
                hits += 1;
            }
        }
        if hits >= fail_threshold {
            let v = build_violation(
                self,
                ctx,
                ViolationSeverity::Hard,
                format!(
                    "Copy-trading pattern detected: {hits} correlated trades from other accounts within {window_seconds}s window"
                ),
            );
            return Ok(RuleVerdict::Fail(v));
        }
        if hits >= 1 {
            let v = build_violation(
                self,
                ctx,
                ViolationSeverity::Warning,
                format!(
                    "Suspicious trade correlation: {hits} reference trades from other accounts within {window_seconds}s window"
                ),
            );
            return Ok(RuleVerdict::Warn(v));
        }
        Ok(RuleVerdict::Pass)
    }
}

impl CopyTradingRule {
    /// Constructs a parameterized rule from a pack entry (P0-D fix).
    #[must_use]
    pub fn from_entry(entry: &crate::rulepack::RuleEntry) -> Self {
        CopyTradingRule {
            params: Some(RuleParams::from_entry(entry)),
        }
    }
}

impl ParameterizedRule for CopyTradingRule {
    fn from_entry(entry: &crate::rulepack::RuleEntry) -> Self {
        CopyTradingRule::from_entry(entry)
    }
}
