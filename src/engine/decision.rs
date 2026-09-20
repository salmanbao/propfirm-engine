//! Decision: the engine-level outcome combining all rule verdicts.

use crate::core::account::AccountStatus;
use crate::core::violation::{Violation, ViolationSeverity};
use crate::rules::traits::{RuleReport, RuleVerdict};

/// Final decision kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DecisionKind {
    /// All rules passed.
    Pass,
    /// One or more rules produced warnings; account continues.
    Warn,
    /// One or more rules produced a hard violation; account terminates.
    Fail,
    /// A rule requested liquidation of all open positions.
    Liquidate,
}

impl DecisionKind {
    pub fn is_pass(self) -> bool { matches!(self, DecisionKind::Pass) }
    pub fn is_warn(self) -> bool { matches!(self, DecisionKind::Warn) }
    pub fn is_fail(self) -> bool { matches!(self, DecisionKind::Fail) }
    pub fn is_liquidate(self) -> bool { matches!(self, DecisionKind::Liquidate) }
    pub fn is_terminating(self) -> bool { matches!(self, DecisionKind::Fail | DecisionKind::Liquidate) }
}

/// Why the decision was made.
#[derive(Debug, Clone)]
pub enum DecisionReason {
    AllRulesPassed,
    Warnings(Vec<Violation>),
    HardViolation(Violation),
    LiquidationRequested(Violation),
}

/// The decision produced by the evaluator.
#[derive(Debug, Clone)]
pub struct Decision {
    pub kind: DecisionKind,
    pub reason: DecisionReason,
}

impl Decision {
    /// Aggregates a set of rule reports into a single decision.
    pub fn from_reports(reports: &[RuleReport]) -> Self {
        let mut warnings = Vec::new();
        let mut hard = None;
        let mut liquidate = None;
        for r in reports {
            match &r.verdict {
                RuleVerdict::Warn(v) => warnings.push(v.clone()),
                RuleVerdict::Fail(v) if hard.is_none() => hard = Some(v.clone()),
                RuleVerdict::Liquidate(v) if liquidate.is_none() => liquidate = Some(v.clone()),
                _ => {}
            }
        }
        if let Some(v) = liquidate {
            return Decision {
                kind: DecisionKind::Liquidate,
                reason: DecisionReason::LiquidationRequested(v),
            };
        }
        if let Some(v) = hard {
            return Decision {
                kind: DecisionKind::Fail,
                reason: DecisionReason::HardViolation(v),
            };
        }
        if !warnings.is_empty() {
            return Decision {
                kind: DecisionKind::Warn,
                reason: DecisionReason::Warnings(warnings),
            };
        }
        Decision {
            kind: DecisionKind::Pass,
            reason: DecisionReason::AllRulesPassed,
        }
    }

    pub fn is_pass(&self) -> bool { self.kind.is_pass() }
    pub fn is_terminating(&self) -> bool { self.kind.is_terminating() }

    /// Returns the severity level that should be applied to the account
    /// status based on this decision.
    pub fn account_status_target(&self) -> Option<AccountStatus> {
        match self.kind {
            DecisionKind::Fail => Some(AccountStatus::Failed),
            DecisionKind::Liquidate => Some(AccountStatus::Failed),
            _ => None,
        }
    }

    /// Returns the violations produced by the decision (warnings + hard).
    pub fn violations(&self) -> Vec<&Violation> {
        match &self.reason {
            DecisionReason::AllRulesPassed => Vec::new(),
            DecisionReason::Warnings(vs) => vs.iter().collect(),
            DecisionReason::HardViolation(v) => vec![v],
            DecisionReason::LiquidationRequested(v) => vec![v],
        }
    }

    /// Returns the highest severity level across all violations.
    pub fn max_severity(&self) -> Option<ViolationSeverity> {
        self.violations().iter().map(|v| v.severity).max()
    }
}

impl Default for Decision {
    fn default() -> Self {
        Decision {
            kind: DecisionKind::Pass,
            reason: DecisionReason::AllRulesPassed,
        }
    }
}
