//! Grid / martingale trading rule.
//!
//! Two detections (§D.1):
//!
//! 1. **Grid**: many same-symbol entries at evenly spaced prices — the
//!    coefficient of variation (CV) of the price gaps below a threshold,
//!    over at least `min_entries` entries, all within a recent time
//!    window (`max_window_minutes`).
//! 2. **Martingale lot escalation**: position size growing after
//!    consecutive losing trades — the martingale signature the old CV
//!    heuristic never inspected. Fires when a run of `escalation_losses`
//!    consecutive losing exits is followed by an entry whose size is at
//!    least `escalation_multiplier` × the size of the first loss in the
//!    run.
//!
//! **Pack-driven resolution (§B/§D.1)**: the entry's `value` is the min
//! entry count (grid check, fail-closed via [`RuleParams::effective_count`]);
//! `params_json` keys: `cv_threshold` (default 0.05),
//! `max_window_minutes` (default 120), `escalation_losses` (default 2),
//! `escalation_multiplier` (default 1.5), `severity` ("warning" or
//! "hard" — default warning). When `None` (constructed via `Default`),
//! the rule falls back to the built-in defaults.

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
    /// the min-entry count from the entry's `value` and the detection
    /// knobs from `params_json`. When `None` (constructed via `Default`),
    /// the rule falls back to the built-in defaults.
    pub params: Option<RuleParams>,
}

/// Default coefficient-of-variation threshold below which entry spacing is
/// considered "uniform" (grid-like).
pub const DEFAULT_CV_THRESHOLD: rust_decimal::Decimal = dec!(0.05);
/// Default minimum number of same-symbol entries that constitute a grid.
pub const DEFAULT_MIN_ENTRIES: usize = 3;
/// Default maximum age (minutes) of the oldest entry in a detected grid —
/// an old uniform sequence is history, not an active grid.
pub const DEFAULT_MAX_WINDOW_MINUTES: i64 = 120;
/// Default number of consecutive losing exits that trigger escalation
/// checking.
pub const DEFAULT_ESCALATION_LOSSES: usize = 2;
/// Default size multiple (vs the first loss in the run) that counts as
/// escalation.
pub const DEFAULT_ESCALATION_MULTIPLIER: rust_decimal::Decimal = dec!(1.5);

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

    /// Reads a decimal knob from `params_json` with a default fallback.
    fn knob(&self, key: &str, default: rust_decimal::Decimal) -> rust_decimal::Decimal {
        self.params
            .as_ref()
            .and_then(|p| p.json_decimal(key))
            .unwrap_or(default)
    }

    /// Resolves the effective CV threshold.
    #[must_use]
    fn effective_cv_threshold(&self) -> rust_decimal::Decimal {
        self.knob("cv_threshold", DEFAULT_CV_THRESHOLD)
    }

    /// Resolves the effective grid time window in minutes.
    #[must_use]
    fn effective_max_window_minutes(&self) -> i64 {
        self.knob(
            "max_window_minutes",
            rust_decimal::Decimal::from(DEFAULT_MAX_WINDOW_MINUTES),
        )
        .to_i64()
        .unwrap_or(DEFAULT_MAX_WINDOW_MINUTES)
    }

    /// Resolves the required consecutive-loss count for escalation.
    #[must_use]
    fn effective_escalation_losses(&self) -> usize {
        self.knob(
            "escalation_losses",
            rust_decimal::Decimal::from(DEFAULT_ESCALATION_LOSSES),
        )
        .to_i64()
        .map_or(DEFAULT_ESCALATION_LOSSES, |v| v.max(1) as usize)
    }

    /// Resolves the escalation size multiple.
    #[must_use]
    fn effective_escalation_multiplier(&self) -> rust_decimal::Decimal {
        self.knob("escalation_multiplier", DEFAULT_ESCALATION_MULTIPLIER)
    }

    /// Resolves the effective severity for a confirmed detection. The
    /// pack's top-level `severity` field wins; `params_json.severity` is
    /// also accepted; default Warning.
    #[must_use]
    fn effective_severity(&self) -> ViolationSeverity {
        if let Some(p) = &self.params {
            let sev = p.severity.as_deref().map(str::to_ascii_lowercase);
            let sev = match sev.as_deref() {
                Some(s) if !s.is_empty() => Some(s.to_string()),
                _ => p.json_decimal("severity_hard").map(|v| {
                    if v >= dec!(1) {
                        "hard".to_string()
                    } else {
                        "warning".to_string()
                    }
                }),
            };
            if sev.as_deref() == Some("hard") {
                return ViolationSeverity::Hard;
            }
        }
        ViolationSeverity::Warning
    }

    /// Grid detection: uniform price spacing across enough recent entries.
    fn detect_grid(
        &self,
        ctx: &RuleContext,
        symbol: &crate::core::types::Symbol,
        min_entries: usize,
    ) -> Option<(Vec<rust_decimal::Decimal>, rust_decimal::Decimal)> {
        let cv_threshold = self.effective_cv_threshold();
        let now = ctx.server_time.ts();
        let window = chrono::Duration::minutes(self.effective_max_window_minutes());
        let mut entries: Vec<&crate::core::trade::Trade> = ctx
            .today_trades
            .iter()
            .filter(|t| {
                t.symbol == *symbol
                    && t.trade_side == crate::core::trade::TradeSide::Entry
                    && now.signed_duration_since(t.executed_at) <= window
            })
            .collect();
        entries.sort_by_key(|t| t.executed_at);
        if entries.len() < min_entries {
            return None;
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
            return None;
        }
        // Coefficient of variation (CV) = std/mean.
        let mean =
            gaps.iter().sum::<rust_decimal::Decimal>() / rust_decimal::Decimal::from(gaps.len());
        if mean.is_zero() {
            return None;
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
            return Some((gaps, cv));
        }
        None
    }

    /// Martingale detection: a run of consecutive losing exits followed by
    /// an entry of escalated size.
    fn detect_escalation(
        &self,
        ctx: &RuleContext,
        symbol: &crate::core::types::Symbol,
    ) -> Option<(usize, rust_decimal::Decimal)> {
        let need_losses = self.effective_escalation_losses();
        let multiplier = self.effective_escalation_multiplier();
        // Chronological exits + entries for this symbol.
        let mut exits: Vec<&crate::core::trade::Trade> = ctx
            .today_trades
            .iter()
            .filter(|t| t.symbol == *symbol && t.trade_side == crate::core::trade::TradeSide::Exit)
            .collect();
        exits.sort_by_key(|t| t.executed_at);
        // Walk backwards from the most recent exit; count consecutive
        // losses and record their POSITION SIZES (quantities) — the
        // escalation comparison is quantity-vs-quantity, not
        // quantity-vs-loss-amount.
        let mut loss_sizes: Vec<rust_decimal::Decimal> = Vec::new();
        for t in exits.iter().rev() {
            let net = t.net_pnl().0;
            if net < rust_decimal::Decimal::ZERO {
                loss_sizes.push(t.quantity.0);
            } else {
                break;
            }
        }
        if loss_sizes.len() < need_losses {
            return None;
        }
        // `loss_sizes` is reverse-chronological: last element is the
        // FIRST loss of the run. Escalation = newest entry larger than
        // multiplier × the first loss's position size.
        let first_loss_size = *loss_sizes.last()?;
        let newest_entry_size = ctx
            .today_trades
            .iter()
            .filter(|t| t.symbol == *symbol && t.trade_side == crate::core::trade::TradeSide::Entry)
            .map(|t| t.quantity.0)
            .fold(rust_decimal::Decimal::ZERO, rust_decimal::Decimal::max);
        if newest_entry_size >= first_loss_size * multiplier {
            return Some((
                loss_sizes.len(),
                newest_entry_size / first_loss_size.max(dec!(0.0001)),
            ));
        }
        None
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
        self.effective_severity()
    }

    fn description(&self) -> &'static str {
        "Detects grid patterns (uniform entry spacing) and martingale lot escalation after consecutive losses."
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
        let Some(order) = &ctx.pending_order else {
            return Ok(RuleVerdict::Pass);
        };
        let symbol = &order.symbol;
        // Grid check (uniform spacing, recent window).
        if let Some((gaps, cv)) = self.detect_grid(ctx, symbol, min_entries) {
            let v = build_violation(
                self,
                ctx,
                self.effective_severity(),
                format!(
                    "Potential grid trading detected on {symbol}: uniform entry spacing over {} entries (CV={cv}, mean gap {mean})",
                    gaps.len() + 1,
                    mean = gaps.iter().sum::<rust_decimal::Decimal>()
                        / rust_decimal::Decimal::from(gaps.len())
                ),
            );
            return Ok(RuleVerdict::Warn(v));
        }
        // Martingale check (size escalation after consecutive losses).
        if let Some((losses, ratio)) = self.detect_escalation(ctx, symbol) {
            let v = build_violation(
                self,
                ctx,
                self.effective_severity(),
                format!(
                    "Martingale pattern detected on {symbol}: {losses} consecutive losses followed by an escalated entry ({ratio:.2}× the first loss size)"
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
