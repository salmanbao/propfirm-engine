//! Log notifier: writes notifications to stdout/stderr (or a buffer).

use crate::core::ids::AccountId;
use crate::core::violation::Violation;
use crate::core::Error;
use crate::notifications::traits::Notifier;
use parking_lot::Mutex;
use std::fmt::Write as _;
use std::sync::Arc;

/// Notifier that buffers messages in memory. Useful for testing.
#[derive(Clone, Default)]
pub struct LogNotifier {
    buf: Arc<Mutex<Vec<String>>>,
}

impl LogNotifier {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn drain(&self) -> Vec<String> {
        std::mem::take(&mut *self.buf.lock())
    }

    #[must_use]
    pub fn snapshot(&self) -> Vec<String> {
        self.buf.lock().clone()
    }
}

impl Notifier for LogNotifier {
    fn notify_violation(&self, v: &Violation) -> Result<(), Error> {
        let mut s = String::new();
        let _ = write!(
            s,
            "[{}] account={} rule={} kind={} severity={} msg={}",
            v.occurred_at.to_rfc3339(),
            v.account_id,
            v.rule_name,
            v.kind,
            v.severity,
            v.message,
        );
        if let Some(breach) = v.breach_value {
            let _ = write!(
                s,
                " breach={} threshold={}",
                breach,
                v.threshold_value.unwrap_or(crate::core::types::Money::ZERO)
            );
        }
        self.buf.lock().push(s);
        Ok(())
    }

    fn notify_account_event(
        &self,
        account_id: AccountId,
        kind: &str,
        msg: &str,
    ) -> Result<(), Error> {
        let s = format!(
            "[{}] account={} kind={} msg={}",
            chrono::Utc::now().to_rfc3339(),
            account_id,
            kind,
            msg
        );
        self.buf.lock().push(s);
        Ok(())
    }
}
