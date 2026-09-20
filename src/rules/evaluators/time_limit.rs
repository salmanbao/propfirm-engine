//! Time limit rule.
//!
//! Forbids new orders and fails the account when the configured evaluation
//! window has elapsed.

use crate::core::ids::RuleId;
use crate::core::violation::{ViolationKind, ViolationSeverity};
use crate::rules::context::{EvaluationScope, RuleContext};
use crate::rules::registry::build_violation;
use crate::rules::traits::{Rule, RuleVerdict};

#[derive(Debug, Clone, Default)]
pub struct TimeLimitRule;

impl Rule for TimeLimitRule {
    fn id(&self) -> RuleId { RuleId::named("time_limit") }
    fn name(&self) -> &str { "Time Limit" }
    fn kind(&self) -> ViolationKind { ViolationKind::TimeLimit }
    fn scope(&self) -> EvaluationScope { EvaluationScope::OnTick }
    fn severity(&self) -> ViolationSeverity { ViolationSeverity::Hard }

    fn description(&self) -> &str {
        "Fails the account when the evaluation time window has elapsed."
    }

    fn is_enabled(&self, ctx: &RuleContext) -> bool {
        ctx.account.plan.time_limit_days.is_some()
    }

    fn evaluate(&self, ctx: &RuleContext) -> crate::Result<RuleVerdict> {
        let Some(deadline) = ctx.account.deadline else {
            return Ok(RuleVerdict::Pass);
        };
        let now = ctx.server_time.ts();
        if now > deadline {
            let v = build_violation(
                self,
                ctx,
                ViolationSeverity::Hard,
                format!("Account time limit exceeded: deadline was {deadline}"),
            );
            return Ok(RuleVerdict::Fail(v));
        }
        // Warn at 90% time elapsed
        if let Some(start) = ctx.account.started_at {
            let total = deadline - start;
            let elapsed = now - start;
            if elapsed.num_seconds() as f64 / total.num_seconds().max(1) as f64 > 0.9 {
                let v = build_violation(
                    self,
                    ctx,
                    ViolationSeverity::Warning,
                    format!("Time limit approaching: deadline {deadline}"),
                );
                return Ok(RuleVerdict::Warn(v));
            }
        }
        Ok(RuleVerdict::Pass)
    }
}
