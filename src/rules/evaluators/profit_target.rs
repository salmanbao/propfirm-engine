//! Profit target rule.
//!
//! Returns a `Pass` verdict when the profit target has been reached. Returns
//! `Info` when below target (no severity). Returns `Warning` when the time
//! limit has expired without hitting the target.

use crate::core::ids::RuleId;
use crate::core::types::{dec, Money};
use crate::core::violation::{ViolationKind, ViolationSeverity};
use crate::rules::context::{EvaluationScope, RuleContext};
use crate::rules::registry::build_violation;
use crate::rules::traits::{Rule, RuleVerdict};

#[derive(Debug, Clone, Default)]
pub struct ProfitTargetRule;

impl Rule for ProfitTargetRule {
    fn id(&self) -> RuleId { RuleId::named("profit_target") }
    fn name(&self) -> &str { "Profit Target" }
    fn kind(&self) -> ViolationKind { ViolationKind::ProfitTargetMissed }
    fn scope(&self) -> EvaluationScope { EvaluationScope::OnTick }
    fn severity(&self) -> ViolationSeverity { ViolationSeverity::Info }

    fn description(&self) -> &str {
        "Verifies the account has reached its profit target for the current phase."
    }

    fn evaluate(&self, ctx: &RuleContext) -> crate::Result<RuleVerdict> {
        let target_pct = ctx.account.plan.profit_target_pct;
        if target_pct.0 <= dec!(0) {
            // No target (e.g. funded phase) – always pass.
            return Ok(RuleVerdict::Pass);
        }
        let target = ctx.account.profit_target();
        let net = ctx.account.net_profit();
        if net.0 >= target.0 {
            return Ok(RuleVerdict::Pass);
        }
        // Check time limit
        if let Some(deadline) = ctx.account.deadline {
            if ctx.server_time.ts() > deadline {
                let mut v = build_violation(
                    self,
                    ctx,
                    ViolationSeverity::Hard,
                    format!(
                        "Time limit expired without reaching profit target: net {net} < target {target}"
                    ),
                );
                v = v.with_breach(target, target);
                return Ok(RuleVerdict::Fail(v));
            }
        }
        // Below target but within time limit – not a violation, just pass.
        Ok(RuleVerdict::Pass)
    }
}
