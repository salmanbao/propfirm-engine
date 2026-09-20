//! Performance report builder.

use crate::core::account::AccountSnapshot;
use crate::core::types::{Money, Timestamp};
use crate::engine::decision::Decision;
use crate::engine::snapshot::Snapshot;
use crate::risk::metrics::RiskMetrics;

/// A structured performance report combining account snapshot, risk metrics,
/// and decision.
#[derive(Debug, Clone)]
pub struct PerformanceReport {
    pub account: AccountSnapshot,
    pub risk: RiskMetrics,
    pub decision: Decision,
    pub generated_at: Timestamp,
}

impl PerformanceReport {
    #[must_use]
    pub fn new(snapshot: &Snapshot, risk: RiskMetrics) -> Self {
        PerformanceReport {
            account: snapshot.account.clone(),
            risk,
            decision: snapshot.decision.clone(),
            generated_at: chrono::Utc::now(),
        }
    }

    /// One-line executive summary.
    #[must_use]
    pub fn summary_line(&self) -> String {
        let profit = self.account.net_profit;
        let dd = self.account.total_drawdown;
        format!(
            "balance={} equity={} net_pnl={} dd={}/{} ({:.2}%) profit_target={:.2}% decision={:?}",
            self.account.balance,
            self.account.equity,
            profit,
            dd,
            self.account.initial_balance,
            self.account.max_dd_utilization.0 * rust_decimal::Decimal::ONE_HUNDRED,
            self.account.profit_target_utilization.0 * rust_decimal::Decimal::ONE_HUNDRED,
            self.decision.kind,
        )
    }

    /// Estimated payout amount (only if account is funded and profitable).
    #[must_use]
    pub fn estimated_payout(&self, split_pct: rust_decimal::Decimal) -> Money {
        let net = self.account.net_profit;
        if net.0.is_sign_negative() {
            return Money::ZERO;
        }
        Money(net.0 * split_pct)
    }
}
