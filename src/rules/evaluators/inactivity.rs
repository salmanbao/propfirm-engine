//! Inactivity termination rule (P1.4 / P1.5 fix).
//!
//! Several 2026 programs replace the fixed evaluation deadline with
//! "unlimited time + inactivity termination": the account must show
//! trading activity at least once every N days or it is terminated.
//!
//! The rule fires on periodic / on-demand evaluation. It reads:
//! - the pack entry's `value` (days) when bound, else
//! - the plan's `inactivity_days` field.
//!
//! **P0.1 fix**: disabled unless the plan or a pack entry enables it.

use crate::core::ids::RuleId;
use crate::core::violation::{ViolationKind, ViolationSeverity};
use crate::rules::context::{EvaluationScope, RuleContext};
use crate::rules::params::{ParameterizedRule, RuleParams};
use crate::rules::registry::build_violation;
use crate::rules::traits::{Rule, RuleVerdict};

#[derive(Debug, Clone, Default)]
pub struct InactivityRule {
    /// Pack-derived parameters (P0-D pattern).
    pub params: Option<RuleParams>,
}

impl InactivityRule {
    #[must_use]
    pub fn from_entry(entry: &crate::rulepack::RuleEntry) -> Self {
        InactivityRule {
            params: Some(RuleParams::from_entry(entry)),
        }
    }

    /// Effective inactivity window in days: pack entry's `value`
    /// interpreted per `effective_count` (fail-closed on a bad unit),
    /// else the plan's `inactivity_days`. `None` = not configured.
    fn effective_days(&self, ctx: &RuleContext) -> Result<Option<u32>, crate::core::Error> {
        if let Some(p) = &self.params {
            let v = p.effective_count("inactivity")?;
            let days = u32::try_from(v).map_err(|_| {
                crate::core::Error::invalid_config(format!(
                    "inactivity: pack value {v} is not a valid day count"
                ))
            })?;
            return Ok(Some(days));
        }
        Ok(ctx.account.plan.inactivity_days)
    }
}

impl Rule for InactivityRule {
    fn id(&self) -> RuleId {
        RuleId::named("inactivity")
    }
    fn name(&self) -> &'static str {
        "Inactivity Termination"
    }
    fn kind(&self) -> ViolationKind {
        ViolationKind::Custom
    }
    fn scope(&self) -> EvaluationScope {
        EvaluationScope::Periodic
    }
    fn severity(&self) -> ViolationSeverity {
        ViolationSeverity::Hard
    }
    fn priority(&self) -> u32 {
        self.params
            .as_ref()
            .and_then(super::super::params::RuleParams::priority)
            .unwrap_or(500)
    }
    fn tolerance_cents(&self) -> i64 {
        0
    }

    fn description(&self) -> &'static str {
        "Terminates accounts with no trading activity for N days \
         (unlimited-time programs). Disabled unless the plan or a pack \
         entry enables it."
    }

    /// **P0.1 fix**: disabled unless the plan or the pack entry
    /// explicitly enables it.
    fn is_enabled(&self, ctx: &RuleContext) -> bool {
        if let Some(p) = &self.params {
            if !p.enabled {
                return false;
            }
            return true;
        }
        ctx.account.plan.inactivity_days.is_some()
    }

    fn evaluate(&self, ctx: &RuleContext) -> crate::Result<RuleVerdict> {
        let Some(days) = self.effective_days(ctx)? else {
            return Ok(RuleVerdict::Pass);
        };
        if days == 0 {
            return Ok(RuleVerdict::Pass);
        }
        let Some(last_activity) = ctx.account.last_trade_at else {
            // Never traded: only relevant once the account has started
            // and the inactivity window from *start* has elapsed.
            let Some(started) = ctx.account.started_at else {
                return Ok(RuleVerdict::Pass);
            };
            let idle = ctx.server_time.ts() - started;
            if idle.num_days() >= i64::from(days) {
                let v = build_violation(
                    self,
                    ctx,
                    ViolationSeverity::Hard,
                    format!(
                        "Account never traded for {days} days since start (inactivity termination)"
                    ),
                );
                return Ok(RuleVerdict::Fail(v));
            }
            return Ok(RuleVerdict::Pass);
        };
        let idle = ctx.server_time.ts() - last_activity;
        if idle.num_days() >= i64::from(days) {
            let v = build_violation(
                self,
                ctx,
                ViolationSeverity::Hard,
                format!(
                    "No trading activity for {idle_days} days (limit {days}) — inactivity termination",
                    idle_days = idle.num_days()
                ),
            );
            return Ok(RuleVerdict::Fail(v));
        }
        // Warn one day before termination.
        if idle.num_days() >= i64::from(days.saturating_sub(1)) {
            let v = build_violation(
                self,
                ctx,
                ViolationSeverity::Warning,
                format!(
                    "Inactivity warning: {idle_days} days without a trade (termination at {days})",
                    idle_days = idle.num_days()
                ),
            );
            return Ok(RuleVerdict::Warn(v));
        }
        Ok(RuleVerdict::Pass)
    }
}

impl ParameterizedRule for InactivityRule {
    fn from_entry(entry: &crate::rulepack::RuleEntry) -> Self {
        InactivityRule::from_entry(entry)
    }
}
