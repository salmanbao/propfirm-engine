//! Notifier trait.

use crate::core::violation::Violation;
use crate::core::Error;

/// Pluggable notifier for delivering alerts on rule violations.
pub trait Notifier: Send + Sync {
    fn notify_violation(&self, v: &Violation) -> Result<(), Error>;
    fn notify_account_event(&self, account_id: crate::core::ids::AccountId, kind: &str, msg: &str) -> Result<(), Error>;
}
