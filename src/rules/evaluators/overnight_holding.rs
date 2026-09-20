//! Overnight holding rule.
//!
//! Some plans forbid holding positions during specific overnight hours (e.g.
//! between 22:00–07:00 server time). The rule fires on `OnTick` and
//! `OnOrderSubmit` to enforce this.

use crate::core::ids::RuleId;
use crate::core::violation::{ViolationKind, ViolationSeverity};
use crate::rules::context::{EvaluationScope, RuleContext};
use crate::rules::registry::build_violation;
use crate::rules::traits::{Rule, RuleVerdict};
use chrono::Timelike;

#[derive(Debug, Clone, Default)]
pub struct OvernightHoldingRule;

impl Rule for OvernightHoldingRule {
    fn id(&self) -> RuleId { RuleId::named("overnight_holding") }
    fn name(&self) -> &str { "Overnight Holding" }
    fn kind(&self) -> ViolationKind { ViolationKind::OvernightHolding }
    fn scope(&self) -> EvaluationScope { EvaluationScope::PreTrade }
    fn severity(&self) -> ViolationSeverity { ViolationSeverity::Hard }

    fn description(&self) -> &str {
        "Forbids holding positions during specified overnight hours."
    }

    fn is_enabled(&self, ctx: &RuleContext) -> bool {
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
        let from = cfg.forbidden_from_hour as u32;
        let to = cfg.forbidden_to_hour as u32;
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
