//! Cooldown rule.
//!
//! Enforces a minimum interval between trades. Useful for firms that want to
//! discourage high-frequency scalping behavior.

use crate::core::ids::RuleId;
use crate::core::violation::{ViolationKind, ViolationSeverity};
use crate::rules::context::{EvaluationScope, RuleContext};
use crate::rules::params::{ParameterizedRule, RuleParams};
use crate::rules::registry::build_violation;
use crate::rules::traits::{Rule, RuleVerdict};

#[derive(Debug, Clone, Default)]
pub struct CooldownRule {
    /// **P0-D fix**: pack-derived parameters. When `Some`, rule reads
    /// `value`/`basis`/`tolerance_cents`/`priority` from here instead of
    /// from `ctx.account.plan` — so a tenant editing the pack actually
    /// changes the verdict. When `None` (constructed via `Default`), the
    /// rule falls back to plan-derived config.
    pub params: Option<RuleParams>,
}

impl CooldownRule {
    /// **P0.4 fix**: effective cooldown seconds — pack entry's value if
    /// set, else plan.
    fn effective_seconds(&self, ctx: &RuleContext) -> Result<u64, crate::core::Error> {
        if let Some(p) = &self.params {
            if !p.enabled || p.value().is_none() {
                return Ok(0);
            }
            let v = p.value().unwrap_or_default();
            let secs = u64::try_from(v).map_err(|_| {
                crate::core::Error::invalid_config(format!(
                    "cooldown: pack value {v} is not a valid seconds count"
                ))
            })?;
            return Ok(secs);
        }
        Ok(ctx.account.plan.cooldown_seconds)
    }
}

impl Rule for CooldownRule {
    fn id(&self) -> RuleId {
        RuleId::named("cooldown")
    }
    fn name(&self) -> &'static str {
        "Cooldown"
    }
    fn kind(&self) -> ViolationKind {
        ViolationKind::Cooldown
    }
    fn scope(&self) -> EvaluationScope {
        EvaluationScope::PreTrade
    }
    fn severity(&self) -> ViolationSeverity {
        ViolationSeverity::Warning
    }
    // P-failure-policy: expose pack-derived params for registry error mapping.
    fn params(&self) -> Option<&crate::rules::params::RuleParams> {
        self.params.as_ref()
    }

    fn description(&self) -> &'static str {
        "Enforces a minimum interval between consecutive trades."
    }

    fn is_enabled(&self, ctx: &RuleContext) -> bool {
        // P0.4: pack entry's enabled flag overrides the plan.
        if let Some(p) = &self.params {
            if !p.enabled {
                return false;
            }
            return p.value.is_some();
        }
        ctx.account.plan.cooldown_seconds > 0
    }

    fn evaluate(&self, ctx: &RuleContext) -> crate::Result<RuleVerdict> {
        // P0.4: use the effective cooldown (pack entry overrides plan).
        let cooldown = self
            .effective_seconds(ctx)
            .map_err(|e| crate::core::Error::RuleEval(format!("cooldown: {e}")))?;
        if cooldown == 0 {
            return Ok(RuleVerdict::Pass);
        }
        let Some(order) = &ctx.pending_order else {
            return Ok(RuleVerdict::Pass);
        };
        let Some(last_trade) = ctx.today_trades.last() else {
            return Ok(RuleVerdict::Pass);
        };
        let elapsed = (order.submitted_at - last_trade.executed_at).num_seconds();
        if elapsed < cooldown as i64 {
            let remaining = cooldown as i64 - elapsed;
            let v = build_violation(
                self,
                ctx,
                ViolationSeverity::Warning,
                format!("Cooldown violated: only {elapsed}s since last trade (need {cooldown}s); {remaining}s remaining"),
            );
            return Ok(RuleVerdict::Warn(v));
        }
        Ok(RuleVerdict::Pass)
    }
}

impl CooldownRule {
    /// Constructs a parameterized rule from a pack entry (P0-D fix).
    #[must_use]
    pub fn from_entry(entry: &crate::rulepack::RuleEntry) -> Self {
        CooldownRule {
            params: Some(RuleParams::from_entry(entry)),
        }
    }
}

impl ParameterizedRule for CooldownRule {
    fn from_entry(entry: &crate::rulepack::RuleEntry) -> Self {
        CooldownRule::from_entry(entry)
    }
}
