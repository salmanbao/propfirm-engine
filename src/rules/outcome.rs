//! Outcome of rule evaluation. Mirrors [`RuleVerdict`] but is exposed at the
//! engine level (combined from all rule outcomes).

use crate::core::violation::Violation;

#[derive(Debug, Clone)]
pub enum Outcome {
    Pass,
    Skip,
    Warn(Violation),
    Fail(Violation),
    Liquidate(Violation),
    /// Positive outcome: profit target was just hit. (P0-3 fix.)
    TargetHit(Violation),
    /// Emergency stop. (P1-12 fix.)
    Emergency(Violation),
    /// Ops-paged early warning. (P1-13 fix.)
    EarlyWarning(Violation),
}

impl Outcome {
    #[must_use]
    pub fn is_pass(&self) -> bool {
        matches!(self, Outcome::Pass | Outcome::Skip)
    }
    #[must_use]
    pub fn is_fail(&self) -> bool {
        matches!(
            self,
            Outcome::Fail(_) | Outcome::Liquidate(_) | Outcome::Emergency(_)
        )
    }
    #[must_use]
    pub fn is_warn(&self) -> bool {
        matches!(self, Outcome::Warn(_))
    }
    #[must_use]
    pub fn is_target_hit(&self) -> bool {
        matches!(self, Outcome::TargetHit(_))
    }
    #[must_use]
    pub fn is_emergency(&self) -> bool {
        matches!(self, Outcome::Emergency(_))
    }
    #[must_use]
    pub fn is_early_warning(&self) -> bool {
        matches!(self, Outcome::EarlyWarning(_))
    }
}
