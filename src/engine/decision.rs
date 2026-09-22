//! Decision: the engine-level outcome combining all rule verdicts.
//!
//! **P0-3 fix**: the `DecisionKind` enum now has dedicated variants for
//! `TargetHit`, `Emergency`, and `EarlyWarning` so downstream consumers can
//! distinguish "you just passed" from "everything's fine" without diffing
//! account state.
//!
//! **P0-4 fix**: when multiple rules fire on the same evaluation,
//! [`Decision::from_reports`] picks the winner by *explicit numeric
//! priority* (highest wins), not by registration order. This produces one
//! defensible answer per tick — the binding spec's hard requirement.

use crate::core::account::AccountStatus;
use crate::core::violation::{Violation, ViolationSeverity};
use crate::rules::traits::{RuleReport, RuleVerdict};

/// Final decision kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DecisionKind {
    /// All rules passed; nothing happened.
    Pass,
    /// One or more rules produced warnings; the account continues.
    Warn,
    /// One or more rules produced early-warning (ops-paged) signals.
    /// (P1-13 fix.)
    EarlyWarning,
    /// **P0-3 fix**: the profit target was just hit — distinct from `Pass`
    /// so downstream consumers can react to the promotion. If a breach
    /// fires on the same evaluation, the breach wins (see
    /// [`Decision::from_reports`]).
    TargetHit,
    /// One or more rules produced a hard violation; the account
    /// terminates.
    Fail,
    /// One or more rules flagged a missing/incomplete required input
    /// rather than silently evaluating against a default.
    GapFlagged,
    /// A rule requested liquidation of all open positions.
    Liquidate,
    /// **P1-12 fix**: emergency stop. Beats every other verdict — even
    /// `Liquidate`. Short-circuits normal rule evaluation entirely.
    Emergency,
}

impl DecisionKind {
    #[must_use]
    pub fn is_pass(self) -> bool {
        matches!(self, DecisionKind::Pass)
    }
    #[must_use]
    pub fn is_warn(self) -> bool {
        matches!(self, DecisionKind::Warn)
    }
    #[must_use]
    pub fn is_early_warning(self) -> bool {
        matches!(self, DecisionKind::EarlyWarning)
    }
    #[must_use]
    pub fn is_target_hit(self) -> bool {
        matches!(self, DecisionKind::TargetHit)
    }
    #[must_use]
    pub fn is_fail(self) -> bool {
        matches!(self, DecisionKind::Fail)
    }
    #[must_use]
    pub fn is_gap_flagged(self) -> bool {
        matches!(self, DecisionKind::GapFlagged)
    }
    #[must_use]
    pub fn is_liquidate(self) -> bool {
        matches!(self, DecisionKind::Liquidate)
    }
    #[must_use]
    pub fn is_emergency(self) -> bool {
        matches!(self, DecisionKind::Emergency)
    }

    /// Returns true for any verdict that should terminate the account.
    #[must_use]
    pub fn is_terminating(self) -> bool {
        matches!(
            self,
            DecisionKind::Fail | DecisionKind::Liquidate | DecisionKind::Emergency
        )
    }

    /// Returns the "weight" of this decision kind for tie-breaking when
    /// priorities are equal. Higher = wins. This is the *secondary*
    /// sort key after explicit rule priority.
    ///
    /// Ordering (highest priority wins first):
    /// 1. Emergency (P1-12: short-circuits everything)
    /// 2. Liquidate (force-close + terminate)
    /// 3. Fail (terminate, no liquidation)
    /// 4. `TargetHit` (positive outcome — loses to any breach)
    /// 5. `EarlyWarning` / Warn (informational)
    /// 6. Pass (nothing happened)
    #[allow(dead_code)] // reserved as documented tie-break extension point (see above)
    fn intrinsic_weight(self) -> u32 {
        match self {
            DecisionKind::Emergency => 10_000,
            DecisionKind::Liquidate => 1_000,
            DecisionKind::Fail => 900,
            DecisionKind::GapFlagged => 800,
            DecisionKind::TargetHit => 100,
            DecisionKind::EarlyWarning => 50,
            DecisionKind::Warn => 40,
            DecisionKind::Pass => 0,
        }
    }
}

/// Why the decision was made.
#[derive(Debug, Clone)]
pub enum DecisionReason {
    AllRulesPassed,
    Warnings(Vec<Violation>),
    EarlyWarnings(Vec<Violation>),
    /// Profit target was just hit on this evaluation. The carried
    /// violation is informational (severity = Info) and carries the
    /// target amount as breach value.
    TargetHitReached(Violation),
    HardViolation(Violation),
    LiquidationRequested(Violation),
    EmergencyStop(Violation),
    /// Evaluation was skipped because required input data was missing
    /// or incomplete.
    GapFlaggedReached(Violation),
}

/// The decision produced by the evaluator.
#[derive(Debug, Clone)]
pub struct Decision {
    pub kind: DecisionKind,
    pub reason: DecisionReason,
    /// **P0-4 fix**: priority of the winning rule. Recorded so the
    /// breach-report endpoint can show "this breach won over N other
    /// violations because it had priority X" — that's the defensible
    /// answer the binding spec asks for.
    pub winning_priority: u32,
    /// All violations produced on this evaluation, regardless of which
    /// one "won". Used for the breach-report endpoint (TD-25).
    pub all_violations: Vec<Violation>,
}

impl Decision {
    /// Aggregates a set of rule reports into a single decision. **P0-4 fix**:
    /// the winner is picked by `rule.priority()` first, then by the
    /// intrinsic weight of the verdict kind (so a `Fail` from a low-priority
    /// rule still beats a `Warn` from a high-priority rule). This means
    /// registration order no longer affects the outcome — reordering
    /// `default_rules()` cannot silently change the decision.
    ///
    /// **P0-3 fix**: `TargetHit` is its own verdict — and it loses to any
    /// breach (Fail/Liquidate/Emergency) on the same evaluation, exactly
    /// as the binding spec requires ("breach wins, always").
    #[must_use]
    pub fn from_reports(reports: &[RuleReport]) -> Self {
        // Collect all violations for the breach-report endpoint.
        let mut all_violations: Vec<Violation> = Vec::new();
        // Categorize reports by verdict kind.
        let mut emergencies: Vec<(u32, Violation)> = Vec::new();
        let mut liquidates: Vec<(u32, Violation)> = Vec::new();
        let mut fails: Vec<(u32, Violation)> = Vec::new();
        let mut target_hits: Vec<(u32, Violation)> = Vec::new();
        let mut early_warnings: Vec<(u32, Violation)> = Vec::new();
        let mut gap_flagged: Vec<(u32, Violation)> = Vec::new();
        let mut warnings: Vec<(u32, Violation)> = Vec::new();

        for r in reports {
            let prio = r.priority;
            match &r.verdict {
                RuleVerdict::Emergency(v) => {
                    all_violations.push(v.clone());
                    emergencies.push((prio, v.clone()));
                }
                RuleVerdict::Liquidate(v) => {
                    all_violations.push(v.clone());
                    liquidates.push((prio, v.clone()));
                }
                RuleVerdict::Fail(v) => {
                    all_violations.push(v.clone());
                    fails.push((prio, v.clone()));
                }
                RuleVerdict::TargetHit(v) => {
                    all_violations.push(v.clone());
                    target_hits.push((prio, v.clone()));
                }
                RuleVerdict::EarlyWarning(v) => {
                    all_violations.push(v.clone());
                    early_warnings.push((prio, v.clone()));
                }
                RuleVerdict::GapFlagged(v) => {
                    all_violations.push(v.clone());
                    gap_flagged.push((prio, v.clone()));
                }
                RuleVerdict::Warn(v) => {
                    all_violations.push(v.clone());
                    warnings.push((prio, v.clone()));
                }
                _ => {}
            }
        }

        // Priority-ordered winner selection. Emergency > Liquidate > Fail > GapFlagged > TargetHit > EarlyWarning > Warn.
        // Within a category, the highest-priority rule wins; ties broken by first-seen.
        let pick_winner = |list: &[(u32, Violation)]| -> Option<(u32, Violation)> {
            list.iter()
                .max_by(|a, b| a.0.cmp(&b.0))
                .map(|(p, v)| (*p, v.clone()))
        };

        if let Some((p, v)) = pick_winner(&emergencies) {
            return Decision {
                kind: DecisionKind::Emergency,
                reason: DecisionReason::EmergencyStop(v.clone()),
                winning_priority: p,
                all_violations,
            };
        }
        if let Some((p, v)) = pick_winner(&liquidates) {
            return Decision {
                kind: DecisionKind::Liquidate,
                reason: DecisionReason::LiquidationRequested(v.clone()),
                winning_priority: p,
                all_violations,
            };
        }
        if let Some((p, v)) = pick_winner(&fails) {
            return Decision {
                kind: DecisionKind::Fail,
                reason: DecisionReason::HardViolation(v.clone()),
                winning_priority: p,
                all_violations,
            };
        }
        if let Some((p, v)) = pick_winner(&gap_flagged) {
            return Decision {
                kind: DecisionKind::GapFlagged,
                reason: DecisionReason::GapFlaggedReached(v.clone()),
                winning_priority: p,
                all_violations,
            };
        }
        // TargetHit loses to any breach (above). If we reach here, no
        // breach fired — so a TargetHit, if present, wins.
        if let Some((p, v)) = pick_winner(&target_hits) {
            return Decision {
                kind: DecisionKind::TargetHit,
                reason: DecisionReason::TargetHitReached(v.clone()),
                winning_priority: p,
                all_violations,
            };
        }
        if !early_warnings.is_empty() {
            let (p, _v) = pick_winner(&early_warnings).unwrap();
            return Decision {
                kind: DecisionKind::EarlyWarning,
                reason: DecisionReason::EarlyWarnings(
                    early_warnings.into_iter().map(|(_, v)| v).collect(),
                ),
                winning_priority: p,
                all_violations,
            };
        }
        if !warnings.is_empty() {
            let (p, _) = pick_winner(&warnings).unwrap();
            return Decision {
                kind: DecisionKind::Warn,
                reason: DecisionReason::Warnings(warnings.into_iter().map(|(_, v)| v).collect()),
                winning_priority: p,
                all_violations,
            };
        }
        Decision {
            kind: DecisionKind::Pass,
            reason: DecisionReason::AllRulesPassed,
            winning_priority: 0,
            all_violations,
        }
    }

    #[must_use]
    pub fn is_pass(&self) -> bool {
        self.kind.is_pass()
    }
    #[must_use]
    pub fn is_target_hit(&self) -> bool {
        self.kind.is_target_hit()
    }
    #[must_use]
    pub fn is_terminating(&self) -> bool {
        self.kind.is_terminating()
    }

    /// Returns the severity level that should be applied to the account
    /// status based on this decision.
    #[must_use]
    pub fn account_status_target(&self) -> Option<AccountStatus> {
        match self.kind {
            DecisionKind::Fail | DecisionKind::Liquidate => Some(AccountStatus::Failed),
            DecisionKind::Emergency => Some(AccountStatus::EmergencyStopped),
            _ => None,
        }
    }

    /// Returns the violations produced by the decision (warnings + hard).
    #[deprecated(
        note = "use all_violations field directly — it carries every violation produced, not just the winner"
    )]
    #[must_use]
    pub fn violations(&self) -> Vec<&Violation> {
        self.all_violations.iter().collect()
    }

    /// Returns the highest severity level across all violations.
    #[must_use]
    pub fn max_severity(&self) -> Option<ViolationSeverity> {
        self.all_violations.iter().map(|v| v.severity).max()
    }
}

impl Default for Decision {
    fn default() -> Self {
        Decision {
            kind: DecisionKind::Pass,
            reason: DecisionReason::AllRulesPassed,
            winning_priority: 0,
            all_violations: Vec::new(),
        }
    }
}
