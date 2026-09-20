//! HFT / scalping rule (P2.15 fix).
//!
//! Many 2026 prop firms explicitly ban high-frequency trading and
//! scalping — round trips (open + close on the same symbol) faster
//! than X seconds, or more than N closes per minute. This rule
//! detects both patterns from recent trades.
//!
//! **P0.1 fix**: this rule is DISABLED by default. It runs only when:
//! - the plan enables it (`plan.hft_ban_enabled = true`), or
//! - a rule-pack entry binds it (`RuleParams` present, `enabled: true`).
//!
//! The default registry previously registered it unconditionally.

use crate::core::ids::RuleId;
use crate::core::violation::{ViolationKind, ViolationSeverity};
use crate::rules::context::{EvaluationScope, RuleContext};
use crate::rules::params::{ParameterizedRule, RuleParams};
use crate::rules::registry::build_violation;
use crate::rules::traits::{Rule, RuleVerdict};

#[derive(Debug, Clone, Default)]
pub struct HftScalpingRule {
    pub params: Option<RuleParams>,
}

impl HftScalpingRule {
    #[must_use]
    pub fn from_entry(entry: &crate::rulepack::RuleEntry) -> Self {
        HftScalpingRule {
            params: Some(RuleParams::from_entry(entry)),
        }
    }

    /// Effective min round-trip time in seconds: pack value if bound,
    /// else the plan's `hft_min_round_trip_seconds` (default 60s).
    fn effective_min_round_trip_seconds(&self, ctx: &RuleContext) -> i64 {
        if let Some(p) = &self.params {
            if let Some(v) = p.value() {
                if let Ok(secs) = i64::try_from(v) {
                    return secs;
                }
            }
        }
        i64::try_from(ctx.account.plan.hft_min_round_trip_seconds).unwrap_or(60)
    }

    /// Effective max closes per minute (default 10). Stored in `params_json`.
    fn effective_max_closes_per_minute(&self) -> u32 {
        // Parse from params_json if present; default 10.
        if let Some(p) = &self.params {
            if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&p.params_json) {
                if let Some(n) = parsed
                    .get("max_closes_per_minute")
                    .and_then(serde_json::Value::as_u64)
                {
                    return n as u32;
                }
            }
        }
        10
    }
}

impl Rule for HftScalpingRule {
    fn id(&self) -> RuleId {
        RuleId::named("hft_scalping")
    }
    fn name(&self) -> &'static str {
        "HFT / Scalping"
    }
    fn kind(&self) -> ViolationKind {
        ViolationKind::Custom
    }
    fn scope(&self) -> EvaluationScope {
        EvaluationScope::PostTrade
    }
    fn severity(&self) -> ViolationSeverity {
        ViolationSeverity::Warning
    }
    fn priority(&self) -> u32 {
        self.params
            .as_ref()
            .and_then(super::super::params::RuleParams::priority)
            .unwrap_or(100)
    }
    fn tolerance_cents(&self) -> i64 {
        self.params
            .as_ref()
            .and_then(super::super::params::RuleParams::tolerance_cents)
            .unwrap_or(0)
    }

    fn description(&self) -> &'static str {
        "Detects high-frequency trading / scalping patterns: \
         round trips faster than X seconds, or more than N closes per minute. \
         Many 2026 prop firms explicitly ban this trading style. \
         Disabled unless the plan or a pack entry enables it (P0.1)."
    }

    /// **P0.1 fix**: disabled unless the plan or the pack entry
    /// explicitly enables it. Never relies on the trait default `true`.
    fn is_enabled(&self, ctx: &RuleContext) -> bool {
        if let Some(p) = &self.params {
            if !p.enabled {
                return false;
            }
            // A bound pack entry IS the enablement.
            return true;
        }
        ctx.account.plan.hft_ban_enabled
    }

    fn evaluate(&self, ctx: &RuleContext) -> crate::Result<RuleVerdict> {
        // Look at the latest trade — if it's an exit, check the time since
        // the corresponding entry on the same symbol.
        let Some(trade) = &ctx.latest_trade else {
            return Ok(RuleVerdict::Pass);
        };

        // Find recent entries on the same symbol.
        let min_round_trip = self.effective_min_round_trip_seconds(ctx);
        let max_closes = self.effective_max_closes_per_minute();

        // Pattern 1: round-trip time < min_round_trip_seconds.
        // Look for an entry on the same symbol within the last min_round_trip
        // seconds before this exit.
        if trade.trade_side == crate::core::trade::TradeSide::Exit {
            let now = trade.executed_at;
            let window_start = now - chrono::Duration::seconds(min_round_trip);
            let mut fast_round_trips = 0u32;
            for t in &ctx.today_trades {
                if t.symbol != trade.symbol {
                    continue;
                }
                if t.executed_at < window_start || t.executed_at > now {
                    continue;
                }
                if t.trade_side == crate::core::trade::TradeSide::Entry {
                    fast_round_trips += 1;
                }
            }
            if fast_round_trips > 0 {
                let v = build_violation(
                    self,
                    ctx,
                    ViolationSeverity::Warning,
                    format!(
                        "HFT pattern: round-trip on {} faster than {}s ({} entries in window)",
                        trade.symbol, min_round_trip, fast_round_trips
                    ),
                );
                return Ok(RuleVerdict::Warn(v));
            }
        }

        // Pattern 2: more than max_closes per minute.
        let now = trade.executed_at;
        let window_start = now - chrono::Duration::minutes(1);
        let recent_closes = ctx
            .today_trades
            .iter()
            .filter(|t| t.executed_at >= window_start && t.executed_at <= now)
            .filter(|t| t.trade_side == crate::core::trade::TradeSide::Exit)
            .count() as u32;
        if recent_closes > max_closes {
            let v = build_violation(
                self,
                ctx,
                ViolationSeverity::Warning,
                format!(
                    "Scalping pattern: {recent_closes} closes in the last minute (cap {max_closes})"
                ),
            );
            return Ok(RuleVerdict::Warn(v));
        }

        Ok(RuleVerdict::Pass)
    }
}

impl ParameterizedRule for HftScalpingRule {
    fn from_entry(entry: &crate::rulepack::RuleEntry) -> Self {
        HftScalpingRule::from_entry(entry)
    }
}
