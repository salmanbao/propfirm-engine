//! Time limit rule.
//!
//! Forbids new orders and fails the account when the configured evaluation
//! window has elapsed.
//!
//! **Pack-driven resolution (§B fix)**: the deadline comes from the first
//! of these that is set —
//!
//! 1. the rule-pack entry's `value` (days; interpreted through `unit` via
//!    [`RuleParams::effective_count`] — uninterpretable units fail closed),
//! 2. the account's stamped `deadline` (derived from `plan.time_limit_days`
//!    at `Account::start()`),
//! 3. `plan.time_limit_days` counted from `account.started_at`.
//!
//! When the pack supplies a value, a tenant editing the pack changes the
//! verdict without touching the plan. When nothing is configured the rule
//! passes (a plan with no time limit has no time-limit rule to enforce).

use crate::core::ids::RuleId;
use crate::core::violation::{ViolationKind, ViolationSeverity};
use crate::rules::context::{EvaluationScope, RuleContext};
use crate::rules::params::{ParameterizedRule, RuleParams};
use crate::rules::registry::build_violation;
use crate::rules::traits::{Rule, RuleVerdict};
use rust_decimal::prelude::ToPrimitive;

#[derive(Debug, Clone, Default)]
pub struct TimeLimitRule {
    /// **P0-D fix**: pack-derived parameters. When `Some`, the rule reads
    /// the deadline (in days) from the entry's `value` first and only
    /// falls back to the account/plan when the entry carries no value.
    /// When `None` (constructed via `Default`), the rule reads the
    /// plan-derived config exclusively.
    pub params: Option<RuleParams>,
}

impl TimeLimitRule {
    /// Resolves the effective deadline. See the module docs for the
    /// precedence (pack value → account deadline → plan).
    fn effective_deadline(
        &self,
        ctx: &RuleContext,
    ) -> Result<Option<chrono::DateTime<chrono::Utc>>, crate::core::Error> {
        if let Some(p) = &self.params {
            if !p.enabled {
                return Ok(None);
            }
            if let Some(days) = p.value() {
                let days_i64 = days.to_i64().ok_or_else(|| {
                    crate::core::Error::invalid_config(
                        "time_limit: pack value is not a valid day count",
                    )
                })?;
                let start = ctx.account.started_at.unwrap_or_else(chrono::Utc::now);
                return Ok(Some(start + chrono::Duration::days(days_i64)));
            }
            // Pack entry present but value-less: fall through to the
            // plan-derived deadline below.
        }
        if let Some(deadline) = ctx.account.deadline {
            return Ok(Some(deadline));
        }
        match ctx.account.plan.time_limit_days {
            None => Ok(None),
            Some(days) => {
                let start = ctx.account.started_at.ok_or_else(|| {
                    crate::core::Error::invalid_state(
                        "time_limit: plan has a time limit but the account was never started \
                         (no started_at) — cannot derive a deadline",
                    )
                })?;
                Ok(Some(start + chrono::Duration::days(i64::from(days))))
            }
        }
    }
}

impl Rule for TimeLimitRule {
    fn id(&self) -> RuleId {
        RuleId::named("time_limit")
    }
    fn name(&self) -> &'static str {
        "Time Limit"
    }
    fn kind(&self) -> ViolationKind {
        ViolationKind::TimeLimit
    }
    fn scope(&self) -> EvaluationScope {
        EvaluationScope::OnTick
    }
    fn severity(&self) -> ViolationSeverity {
        ViolationSeverity::Hard
    }

    fn description(&self) -> &'static str {
        "Fails the account when the evaluation time window (pack or plan) has elapsed."
    }

    fn is_enabled(&self, ctx: &RuleContext) -> bool {
        // P0.4: pack entry's enabled flag overrides the plan. A pack
        // entry carrying no value still enables the rule — the deadline
        // then comes from the plan.
        if let Some(p) = &self.params {
            return p.enabled;
        }
        ctx.account.plan.time_limit_days.is_some() || ctx.account.deadline.is_some()
    }

    fn evaluate(&self, ctx: &RuleContext) -> crate::Result<RuleVerdict> {
        let Some(deadline) = self.effective_deadline(ctx)? else {
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

impl TimeLimitRule {
    /// Constructs a parameterized rule from a pack entry (P0-D fix).
    #[must_use]
    pub fn from_entry(entry: &crate::rulepack::RuleEntry) -> Self {
        TimeLimitRule {
            params: Some(RuleParams::from_entry(entry)),
        }
    }
}

impl ParameterizedRule for TimeLimitRule {
    fn from_entry(entry: &crate::rulepack::RuleEntry) -> Self {
        TimeLimitRule::from_entry(entry)
    }
}
