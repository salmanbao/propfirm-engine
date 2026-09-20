//! Rule trait and verdict types.
//!
//! The [`Rule`] trait is the core abstraction: every concrete rule
//! (drawdown, profit target, etc.) implements it. The engine iterates over
//! registered rules, calling [`Rule::evaluate`] on each.

use crate::core::ids::RuleId;
use crate::core::violation::{Violation, ViolationKind, ViolationSeverity};
use crate::rules::context::{EvaluationScope, RuleContext};
use crate::rules::outcome::Outcome;

/// The result of a single rule evaluation.
#[derive(Debug, Clone)]
pub enum RuleVerdict {
    /// Rule passed; no action.
    Pass,
    /// Rule produced a soft warning; the account continues.
    Warn(Violation),
    /// Rule produced a hard violation; the account should be terminated.
    Fail(Violation),
    /// Rule produced a liquidation request; all open positions should be
    /// force-closed.
    Liquidate(Violation),
    /// **P0-3 fix**: the rule produced a *positive* outcome distinct from
    /// "nothing happened" — specifically, the profit target was just hit.
    /// Downstream consumers (the platform's LCC module, our own CLI demo)
    /// can distinguish "you just passed" from "everything's fine" without
    /// diffing account state before/after.
    ///
    /// If a `Fail`/`Liquidate` and a `TargetHit` fire on the same
    /// evaluation, the breach always wins — see [`Decision::from_reports`]
    /// for the priority ordering.
    TargetHit(Violation),
    /// **P1-12 fix**: rule produced an emergency-stop request (e.g. an
    /// ops/compliance actor triggered `PipelineEvent::EmergencyStop`).
    /// Short-circuits all other rules. The highest possible priority —
    /// beats even `Liquidate`.
    Emergency(Violation),
    /// Rule produced an early-warning (ops-paged) signal — distinct from a
    /// trader-facing [`Warn`](RuleVerdict::Warn). Emitted at ~80% of the
    /// breach threshold on every breach-capable rule (P1-13 fix) so ops
    /// tooling can subscribe to "page ops now" specifically.
    EarlyWarning(Violation),
    /// Rule is not applicable to this context kind.
    Skip,
}

impl RuleVerdict {
    pub fn is_pass(&self) -> bool { matches!(self, RuleVerdict::Pass) }
    pub fn is_fail(&self) -> bool { matches!(self, RuleVerdict::Fail(_)) }
    pub fn is_liquidate(&self) -> bool { matches!(self, RuleVerdict::Liquidate(_)) }
    pub fn is_target_hit(&self) -> bool { matches!(self, RuleVerdict::TargetHit(_)) }
    pub fn is_emergency(&self) -> bool { matches!(self, RuleVerdict::Emergency(_)) }
    pub fn is_early_warning(&self) -> bool { matches!(self, RuleVerdict::EarlyWarning(_)) }

    /// Returns true if this verdict represents a *terminating* outcome —
    /// i.e. one that should mark the account as failed.
    pub fn is_terminating(&self) -> bool {
        matches!(self, RuleVerdict::Fail(_) | RuleVerdict::Liquidate(_) | RuleVerdict::Emergency(_))
    }

    pub fn violation(&self) -> Option<&Violation> {
        match self {
            RuleVerdict::Warn(v)
            | RuleVerdict::Fail(v)
            | RuleVerdict::Liquidate(v)
            | RuleVerdict::TargetHit(v)
            | RuleVerdict::Emergency(v)
            | RuleVerdict::EarlyWarning(v) => Some(v),
            _ => None,
        }
    }

    pub fn severity(&self) -> Option<ViolationSeverity> {
        self.violation().map(|v| v.severity)
    }

    pub fn into_outcome(self) -> Outcome {
        match self {
            RuleVerdict::Pass => Outcome::Pass,
            RuleVerdict::Skip => Outcome::Skip,
            RuleVerdict::Warn(v) => Outcome::Warn(v),
            RuleVerdict::Fail(v) => Outcome::Fail(v),
            RuleVerdict::Liquidate(v) => Outcome::Liquidate(v),
            RuleVerdict::TargetHit(v) => Outcome::TargetHit(v),
            RuleVerdict::Emergency(v) => Outcome::Emergency(v),
            RuleVerdict::EarlyWarning(v) => Outcome::EarlyWarning(v),
        }
    }
}

/// A rule report produced after evaluation. Includes the verdict plus
/// structured metadata for downstream consumers.
#[derive(Debug, Clone)]
pub struct RuleReport {
    pub rule_id: RuleId,
    pub rule_name: String,
    pub verdict: RuleVerdict,
    pub scope: EvaluationScope,
    /// **P0-4 fix**: explicit numeric priority, used by
    /// [`Decision::from_reports`](crate::engine::decision::Decision::from_reports)
    /// to pick the winning verdict when multiple rules fire on the same
    /// evaluation. Higher number = higher priority. Default is 100.
    pub priority: u32,
    pub evaluated_at: chrono::DateTime<chrono::Utc>,
    /// Free-form metadata (e.g. current drawdown amount, threshold, etc.).
    pub metadata: Vec<(String, String)>,
}

impl RuleReport {
    pub fn new(rule_id: RuleId, rule_name: impl Into<String>, verdict: RuleVerdict, scope: EvaluationScope) -> Self {
        RuleReport {
            rule_id,
            rule_name: rule_name.into(),
            verdict,
            scope,
            priority: 100,
            evaluated_at: chrono::Utc::now(),
            metadata: Vec::new(),
        }
    }

    pub fn with_priority(mut self, p: u32) -> Self {
        self.priority = p;
        self
    }

    pub fn with_metadata(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.metadata.push((key.into(), value.into()));
        self
    }
}

/// Outcome alias for backward compatibility.
pub type RuleOutcome = RuleVerdict;

/// The Rule trait. Implement this for any custom rule and register it with
/// the [`RuleRegistry`](crate::rules::registry::RuleRegistry).
pub trait Rule: Send + Sync {
    /// Stable identifier for this rule.
    fn id(&self) -> RuleId;

    /// Human-readable name (used in violations and reports).
    fn name(&self) -> &str;

    /// Category of violation produced.
    fn kind(&self) -> ViolationKind;

    /// When does this rule evaluate?
    fn scope(&self) -> EvaluationScope;

    /// The default severity if the rule is violated.
    fn severity(&self) -> ViolationSeverity;

    /// **P0-4 fix**: numeric priority of this rule when multiple rules fire
    /// on the same evaluation. Higher number = wins. Defaults to 100 (the
    /// "standard" priority for most rules). Override to declare a higher
    /// priority for rules whose verdict must win in a dispute — e.g.
    /// `MaxDrawdownRule` returns 1000, `EmergencyStop` returns 10_000.
    /// The engine uses this to produce *one* defensible answer rather than
    /// relying on registration order (which would silently change if
    /// someone reordered `default_rules()`).
    fn priority(&self) -> u32 { 100 }

    /// **P2 fix**: per-rule tolerance, in cents, to absorb broker rounding
    /// noise at the exact breach boundary. The binding spec calls for a
    /// default of 1¢ — i.e. if the broker reports equity as
    /// `10000.005`, we treat it as `10000.01` for breach purposes so a
    /// 0.5¢ rounding doesn't decide a pass vs. fail. Override per-rule
    /// if a particular rule needs a larger tolerance (e.g. for swap-heavy
    /// assets where 5¢ of slippage is normal).
    ///
    /// Returning 0 disables tolerance — exact `>=`/`>` comparisons are used.
    fn tolerance_cents(&self) -> i64 { 1 }

    /// Evaluate the rule against the given context.
    fn evaluate(&self, ctx: &RuleContext) -> crate::Result<RuleVerdict>;

    /// Optional human-readable description of the rule.
    fn description(&self) -> &str { "" }

    /// Whether the rule is enabled in the current plan. Default: true.
    fn is_enabled(&self, ctx: &RuleContext) -> bool {
        // Implementations can override to read from rule_config.
        let _ = ctx;
        true
    }
}

/// Helper trait for building violations in rule implementations.
pub trait ViolationBuilder {
    fn build_violation(
        &self,
        ctx: &RuleContext,
        severity: ViolationSeverity,
        message: impl Into<String>,
    ) -> Violation;

    /// **P2 fix**: returns the rule's tolerance as a `Money` value (in the
    /// same currency as the account). Used to absorb broker rounding noise
    /// at the exact breach boundary.
    fn tolerance_money(&self) -> crate::core::types::Money {
        let cents = self.tolerance_cents();
        if cents == 0 {
            return crate::core::types::Money(crate::core::types::Decimal::ZERO);
        }
        // 1¢ = 0.01 in the account's currency.
        let decimal_cents = rust_decimal::Decimal::from(cents);
        crate::core::types::Money(decimal_cents / rust_decimal::Decimal::from(100))
    }

    fn tolerance_cents(&self) -> i64;
}

impl<T: Rule> ViolationBuilder for T {
    fn build_violation(
        &self,
        ctx: &RuleContext,
        severity: ViolationSeverity,
        message: impl Into<String>,
    ) -> Violation {
        Violation::new(
            ctx.account.id,
            self.id(),
            self.name(),
            self.kind(),
            severity,
            message,
            ctx.server_time.ts(),
        )
        // P1-9: stamp tenant from the account.
        .with_tenant(ctx.account.tenant_id)
    }

    fn tolerance_cents(&self) -> i64 {
        Rule::tolerance_cents(self)
    }
}
