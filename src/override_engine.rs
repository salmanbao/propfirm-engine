//! Manual override / false-positive clearing (P1-11 fix).
//!
//! The binding spec requires a documented, audited way for tenant risk staff
//! to clear a false-positive breach (e.g. a broker glitch tick) **without
//! deleting the original verdict** — the override is itself part of the
//! permanent record. This module models the data shape and the state-machine
//! edge.
//!
//! State machine:
//!
//! ```text
//! Failed/EmergencyStopped ──override──► Active (breach cleared, audit trail preserved)
//! ```
//!
//! Any other source state is rejected with [`Error::InvalidState`].

use crate::core::ids::{AccountId, ViolationId};
use crate::core::types::Timestamp;

/// An audited record that clears a specific violation and reverts the
/// account to `Active`. The original [`Violation`](crate::core::violation::Violation)
/// is never deleted — it stays in the event log forever; the `Override`
/// is the *rebuttal*, not a deletion.
#[derive(Debug, Clone)]
pub struct Override {
    /// Stable identifier of this override record.
    pub id: OverrideId,
    /// Account this override applies to.
    pub account_id: AccountId,
    /// The violation being cleared (must be a breach-terminal violation
    /// produced by a rule on this account).
    pub clears_violation_id: ViolationId,
    /// Free-form reason text. Required — "broker glitch tick on 2024-03-15
    /// 14:23:11 UTC, ticket #4521 with broker XYZ".
    pub reason: String,
    /// Identity of the ops/compliance actor who issued the override. This
    /// is recorded but not authenticated by the engine itself; in
    /// production, the API layer enforces step-up auth before this record
    /// can be created.
    pub actor_id: String,
    /// When the override was issued.
    pub at: Timestamp,
}

/// Stable identifier for an override record.
pub type OverrideId = crate::core::ids::ViolationId;

impl Override {
    /// Constructs a new override record.
    pub fn new(
        account_id: AccountId,
        clears_violation_id: ViolationId,
        reason: impl Into<String>,
        actor_id: impl Into<String>,
        at: Timestamp,
    ) -> Self {
        Override {
            id: OverrideId::new(),
            account_id,
            clears_violation_id,
            reason: reason.into(),
            actor_id: actor_id.into(),
            at,
        }
    }

    /// Validates the override is internally consistent.
    pub fn validate(&self) -> crate::Result<()> {
        if self.reason.trim().is_empty() {
            return Err(crate::Error::InvalidState(
                "override reason cannot be empty — must explain why the breach is a false positive".into(),
            ));
        }
        if self.actor_id.trim().is_empty() {
            return Err(crate::Error::InvalidState(
                "override actor_id cannot be empty — must identify the responsible human".into(),
            ));
        }
        Ok(())
    }
}
