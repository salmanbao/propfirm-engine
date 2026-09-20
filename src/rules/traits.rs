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
    /// Rule is not applicable to this context kind.
    Skip,
}

impl RuleVerdict {
    pub fn is_pass(&self) -> bool { matches!(self, RuleVerdict::Pass) }
    pub fn is_fail(&self) -> bool { matches!(self, RuleVerdict::Fail(_)) }
    pub fn is_liquidate(&self) -> bool { matches!(self, RuleVerdict::Liquidate(_)) }

    pub fn violation(&self) -> Option<&Violation> {
        match self {
            RuleVerdict::Warn(v) | RuleVerdict::Fail(v) | RuleVerdict::Liquidate(v) => Some(v),
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
            evaluated_at: chrono::Utc::now(),
            metadata: Vec::new(),
        }
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
    }
}
