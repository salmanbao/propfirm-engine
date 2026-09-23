//! Overnight holding rule.
//!
//! Some plans forbid holding positions during specific overnight hours (e.g.
//! between 22:00–07:00 server time). The rule fires on `OnTick` and
//! `OnOrderSubmit` to enforce this.

use crate::core::ids::RuleId;
use crate::core::violation::{ViolationKind, ViolationSeverity};
use crate::rules::context::{EvaluationScope, RuleContext};
use crate::rules::params::{ParameterizedRule, RuleParams};
use crate::rules::registry::build_violation;
use crate::rules::traits::{Rule, RuleVerdict};
use chrono::Timelike;

#[derive(Debug, Clone, Default)]
pub struct OvernightHoldingRule {
    /// **P0-D fix**: pack-derived parameters. When `Some`, rule reads
    /// `value`/`basis`/`tolerance_cents`/`priority` from here instead of
    /// from `ctx.account.plan` — so a tenant editing the pack actually
    /// changes the verdict. When `None` (constructed via `Default`), the
    /// rule falls back to plan-derived config.
    pub params: Option<RuleParams>,
}

impl Rule for OvernightHoldingRule {
    fn id(&self) -> RuleId {
        RuleId::named("overnight_holding")
    }
    fn name(&self) -> &'static str {
        "Overnight Holding"
    }
    fn kind(&self) -> ViolationKind {
        ViolationKind::OvernightHolding
    }
    fn scope(&self) -> EvaluationScope {
        // P0.6 fix: this rule inspects *held open positions* during
        // forbidden overnight hours, so it must run on the tick path
        // too — declaring only `PreTrade` meant the stateless evaluate
        // endpoint (OnTick) could never observe an overnight-held
        // position.
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
        "Forbids holding positions during specified overnight hours."
    }

    fn is_enabled(&self, ctx: &RuleContext) -> bool {
        // P0.4: pack entry's enabled flag overrides the plan. An
        // overnight pack entry means "overnight holding forbidden"
        // when enabled.
        if let Some(p) = &self.params {
            return p.enabled && !ctx.account.plan.overnight_holding_allowed;
        }
        !ctx.account.plan.overnight_holding_allowed
    }

    fn evaluate(&self, ctx: &RuleContext) -> crate::Result<RuleVerdict> {
        if ctx.account.plan.overnight_holding_allowed {
            return Ok(RuleVerdict::Pass);
        }
        let cfg = match ctx.rule_config.overnight_holding.as_ref() {
            Some(c) => c,
            None => return Ok(RuleVerdict::Pass),
        };
        let now = ctx.server_time.ts();
        let hour = now.time().hour();
        let from = u32::from(cfg.forbidden_from_hour);
        let to = u32::from(cfg.forbidden_to_hour);
        let in_forbidden = if from < to {
            hour >= from && hour < to
        } else {
            hour >= from || hour < to
        };
        if !in_forbidden {
            return Ok(RuleVerdict::Pass);
        }
        // Check open positions / pending order
        let has_open = !ctx.open_positions.is_empty();
        let pending = ctx.pending_order.is_some();
        if !has_open && !pending {
            return Ok(RuleVerdict::Pass);
        }
        if pending {
            let v = build_violation(
                self,
                ctx,
                ViolationSeverity::Hard,
                format!(
                    "Opening a position during forbidden overnight hours ({}:00–{}:00)",
                    cfg.forbidden_from_hour, cfg.forbidden_to_hour
                ),
            );
            return Ok(RuleVerdict::Fail(v));
        }
        // Has open positions during overnight → warn (the position will need to be closed).
        let v = build_violation(
            self,
            ctx,
            ViolationSeverity::Warning,
            format!(
                "Position held during forbidden overnight hours ({}:00–{}:00)",
                cfg.forbidden_from_hour, cfg.forbidden_to_hour
            ),
        );
        Ok(RuleVerdict::Warn(v))
    }
}

impl OvernightHoldingRule {
    /// Constructs a parameterized rule from a pack entry (P0-D fix).
    #[must_use]
    pub fn from_entry(entry: &crate::rulepack::RuleEntry) -> Self {
        OvernightHoldingRule {
            params: Some(RuleParams::from_entry(entry)),
        }
    }
}

impl ParameterizedRule for OvernightHoldingRule {
    fn from_entry(entry: &crate::rulepack::RuleEntry) -> Self {
        OvernightHoldingRule::from_entry(entry)
    }
}
