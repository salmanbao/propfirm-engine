//! Snapshot: point-in-time account state captured after each evaluation.

use crate::core::account::AccountSnapshot;
use crate::core::ids::AccountId;
use crate::core::types::Timestamp;
use crate::engine::decision::Decision;

/// A snapshot pairs the account state at evaluation time with the engine's
/// decision and the timestamp it was produced.
#[derive(Debug, Clone)]
pub struct Snapshot {
    pub account_id: AccountId,
    pub at: Timestamp,
    pub account: AccountSnapshot,
    pub decision: Decision,
}

impl Snapshot {
    pub fn new(account: &crate::core::account::Account, decision: Decision) -> Self {
        let snap: AccountSnapshot = account.into();
        Snapshot {
            account_id: account.id,
            at: chrono::Utc::now(),
            account: snap,
            decision,
        }
    }
}
