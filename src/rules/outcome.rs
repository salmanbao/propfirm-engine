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
}

impl Outcome {
    pub fn is_pass(&self) -> bool { matches!(self, Outcome::Pass | Outcome::Skip) }
    pub fn is_fail(&self) -> bool { matches!(self, Outcome::Fail(_) | Outcome::Liquidate(_)) }
    pub fn is_warn(&self) -> bool { matches!(self, Outcome::Warn(_)) }
}
