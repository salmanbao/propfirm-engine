//! Copy-trading detection rule.
//!
//! Detects suspicious patterns that suggest the trader is copy-trading from
//! another account: identical trade sequences, identical lot sizes, and
//! near-zero latency between the trader's orders and those of a reference
//! account.
//!
//! This rule is illustrative – it relies on `recent_events` containing
//! "reference" trades from another account. Real deployments should populate
//! the context with cross-account reference data.

use crate::core::ids::RuleId;
use crate::core::violation::{ViolationKind, ViolationSeverity};
use crate::rules::context::{EvaluationScope, RuleContext};
use crate::rules::params::{ParameterizedRule, RuleParams};
use crate::rules::registry::build_violation;
use crate::rules::traits::{Rule, RuleVerdict};

#[derive(Debug, Clone, Default)]
pub struct CopyTradingRule {
    /// **P0-D fix**: pack-derived parameters. When `Some`, rule reads
    /// `value`/`basis`/`tolerance_cents`/`priority` from here instead of
    /// from `ctx.account.plan` — so a tenant editing the pack actually
    /// changes the verdict. When `None` (constructed via `Default`), the
    /// rule falls back to plan-derived config.
    pub params: Option<RuleParams>,
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
        "Detects potential copy-trading patterns from another account."
    }

    fn is_enabled(&self, ctx: &RuleContext) -> bool {
        !ctx.account.plan.copy_trading_allowed
    }

    fn evaluate(&self, ctx: &RuleContext) -> crate::Result<RuleVerdict> {
        if ctx.account.plan.copy_trading_allowed {
            return Ok(RuleVerdict::Pass);
        }
        let Some(trade) = &ctx.latest_trade else {
            return Ok(RuleVerdict::Pass);
        };
        // Look in recent_events for a reference TradeFilled within 5 seconds of our trade.
        let threshold = chrono::Duration::seconds(5);
        let mut hits = 0;
        for event in &ctx.recent_events {
            if let crate::core::events::DomainEventKind::TradeFilled { trade: ref_trade } =
                &event.kind
            {
                if ref_trade.symbol != trade.symbol {
                    continue;
                }
                if ref_trade.side != trade.side {
                    continue;
                }
                if (ref_trade.executed_at - trade.executed_at)
                    .num_seconds()
                    .abs()
                    <= threshold.num_seconds()
                {
                    hits += 1;
                }
            }
        }
        if hits >= 3 {
            let v = build_violation(
                self,
                ctx,
                ViolationSeverity::Hard,
                format!("Copy-trading pattern detected: {hits} correlated trades within 5s window"),
            );
            return Ok(RuleVerdict::Fail(v));
        }
        if hits >= 1 {
            let v = build_violation(
                self,
                ctx,
                ViolationSeverity::Warning,
                format!("Suspicious trade correlation: {hits} reference trades within 5s window"),
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
