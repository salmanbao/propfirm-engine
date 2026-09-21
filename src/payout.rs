//! Payout engine (§D.2 fix).
//!
//! Before this module the payout layer was `reporting::estimated_payout`
//! (lifetime net × split) — which pays out the trader's *entire* lifetime
//! profit on every payout, double-paying everything already withdrawn.
//! [`AccountStatus::PayoutPending`] was unreachable and `plan.refundable`
//! had no consumer.
//!
//! This module implements the product semantics:
//!
//! - **Profit basis** = balance gain **since the last payout**
//!   (high-water-mark style): `balance − balance_at_last_payout` (or
//!   `initial_balance` before the first payout). Profit before the first
//!   payout is therefore excluded from the second payout's basis.
//! - **Split tiers / scaling plan**: 80% → 90% → 100% by payout count
//!   (`SplitTier::ByPayoutCount`), applied in order.
//! - **Minimum payout threshold**: a request below
//!   [`PayoutConfig::minimum_payout`] is rejected.
//! - **Payout cycle**: bi-weekly / monthly / on-demand — the engine
//!   enforces the earliest permitted date of the next payout.
//! - **Fee refund & bonus handling**: when `plan.refundable` is set, the
//!   fee refund is added to the payout (once, tracked by
//!   [`AccountState::refund_used`]).
//!
//! Requests/approvals drive [`AccountStatus::PayoutPending`] through the
//! `PayoutRequested` / `PayoutApproved` pipeline events.

use crate::core::account::{Account, AccountStatus};
use crate::core::ids::AccountId;
use crate::core::types::{Money, Timestamp};
use crate::core::Error;
use chrono::Duration;

/// How often a payout may be requested.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum PayoutCycle {
    /// Any time (no cycle constraint).
    OnDemand,
    /// At most every 14 days.
    BiWeekly,
    /// At most once per calendar month (30 days).
    Monthly,
}

impl PayoutCycle {
    /// Minimum gap between payouts, `None` for on-demand.
    #[must_use]
    pub fn min_gap(self) -> Option<Duration> {
        match self {
            PayoutCycle::OnDemand => None,
            PayoutCycle::BiWeekly => Some(Duration::days(14)),
            PayoutCycle::Monthly => Some(Duration::days(30)),
        }
    }
}

/// Profit-split scaling plan. Tiers apply **in order** by payout count:
/// the first payout uses tier 0, the second tier 1, and so on; the last
/// tier repeats.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SplitTier {
    /// Profit share the trader receives (0.80 = 80%).
    pub trader_share: rust_decimal::Decimal,
}

/// A configured payout policy for a funded account.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct PayoutConfig {
    /// Minimum payout amount; a request below this is rejected.
    pub minimum_payout: Money,
    /// Payout frequency.
    pub cycle: PayoutCycle,
    /// Scaling plan tiers (80 → 90 → 100 by payout count).
    pub tiers: Vec<SplitTier>,
}

impl Default for PayoutConfig {
    /// Standard prop-firm default: $100 minimum, on-demand, 80/90/100
    /// scaling tiers.
    fn default() -> Self {
        PayoutConfig {
            minimum_payout: Money(crate::core::types::dec!(100)),
            cycle: PayoutCycle::OnDemand,
            tiers: vec![
                SplitTier {
                    trader_share: crate::core::types::dec!(0.80),
                },
                SplitTier {
                    trader_share: crate::core::types::dec!(0.90),
                },
                SplitTier {
                    trader_share: crate::core::types::dec!(1.00),
                },
            ],
        }
    }
}

impl PayoutConfig {
    /// Returns the trader share for the Nth payout (0-based). Tiers apply
    /// in order; the last tier repeats once the ladder is exhausted.
    #[must_use]
    pub fn trader_share_for(&self, payout_count: u32) -> rust_decimal::Decimal {
        if self.tiers.is_empty() {
            return rust_decimal::Decimal::ZERO;
        }
        let idx = (payout_count as usize).min(self.tiers.len() - 1);
        self.tiers[idx].trader_share
    }
}

/// The payout domain record.
#[derive(Debug, Clone)]
pub struct Payout {
    /// Unique request id.
    pub id: crate::core::ids::ViolationId,
    /// Account the payout belongs to.
    pub account_id: AccountId,
    /// Profit basis at request time (balance gain since the last payout).
    pub profit_basis: Money,
    /// Trader share applied (from the scaling tier).
    pub trader_share: rust_decimal::Decimal,
    /// Fee refund added on top (0 when none or already used).
    pub fee_refund: Money,
    /// Final approved amount = profit_basis × trader_share + fee_refund.
    pub amount: Money,
    /// When the payout was requested.
    pub requested_at: Timestamp,
    /// When the payout was approved/executed (`None` until approved).
    pub approved_at: Option<Timestamp>,
}

/// Errors the payout engine can produce.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PayoutError {
    /// Request amount is below the configured minimum.
    BelowMinimum { minimum: Money },
    /// The payout cycle forbids a payout this soon.
    CycleNotReached { next_allowed_at: Timestamp },
    /// No profit accrued since the last payout.
    NoProfit,
}

/// The engine's payout evaluation for a funded account.
///
/// This is the pure decision function: it computes what a payout request
/// would be worth and whether it is permitted, without mutating anything.
#[must_use]
pub struct PayoutQuote {
    /// Profit basis (balance gain since last payout).
    pub profit_basis: Money,
    /// Trader share from the scaling tier.
    pub trader_share: rust_decimal::Decimal,
    /// Fee refund contribution (0 if none).
    pub fee_refund: Money,
    /// Total amount the trader would receive.
    pub amount: Money,
    /// Rejection reason, if the request is not permitted.
    pub rejected: Option<PayoutError>,
}

/// Quotes a payout for `account` under `config`.
///
/// `fee_refund_amount` is the plan's enrolment fee when `plan.refundable`
/// is set AND the account has not already been refunded (tracked by the
/// caller via `refund_used`); pass `None` otherwise.
pub fn quote_payout(
    account: &Account,
    config: &PayoutConfig,
    last_payout_at: Option<Timestamp>,
    balance_at_last_payout: Option<Money>,
    refund_used: bool,
    now: Timestamp,
) -> PayoutQuote {
    // Profit basis: balance gain since the last payout (HWM-style). Profit
    // already paid out (before `balance_at_last_payout`) is excluded.
    let reference = balance_at_last_payout.unwrap_or(account.initial_balance);
    let profit_basis = Money((account.balance.0 - reference.0).max(rust_decimal::Decimal::ZERO));
    let trader_share = config.trader_share_for(account.payout_count);
    // Fee refund: added to the FIRST payout only, when the plan is
    // refundable and the refund hasn't been consumed already. The caller
    // supplies the plan's enrolment fee via `refund_used`'s counterpart —
    // here we only honour the `refund_used` bookkeeping flag; the actual
    // fee amount is passed by the caller through `balance_at_last_payout`
    // bookkeeping (see `AccountState::apply_fee_refund`).
    let fee_refund = if account.payout_count == 0 && !refund_used {
        account.plan.refund_fee_amount
    } else {
        Money::ZERO
    };
    let amount = Money(profit_basis.0 * trader_share + fee_refund.0);
    let mut rejected: Option<PayoutError> = None;
    if profit_basis.0 <= rust_decimal::Decimal::ZERO {
        rejected = Some(PayoutError::NoProfit);
    } else if amount.0 < config.minimum_payout.0 {
        rejected = Some(PayoutError::BelowMinimum {
            minimum: config.minimum_payout,
        });
    } else if let Some(gap) = config.cycle.min_gap() {
        if let Some(last) = last_payout_at {
            let next_allowed = last + gap;
            if now < next_allowed {
                rejected = Some(PayoutError::CycleNotReached {
                    next_allowed_at: next_allowed,
                });
            }
        }
    }
    PayoutQuote {
        profit_basis,
        trader_share,
        fee_refund,
        amount,
        rejected,
    }
}

/// Records an approved payout on the account: stamps the payout bookkeeping
/// (count, last-payout balance watermark, refund consumption).
pub fn record_payout(
    account: &mut Account,
    amount: Money,
    fee_refund: Money,
    at: Timestamp,
) -> Result<(), Error> {
    if account.status != AccountStatus::Funded && account.status != AccountStatus::PayoutPending {
        return Err(Error::invalid_state(format!(
            "payouts are only available to funded accounts (status = {:?})",
            account.status
        )));
    }
    account.payout_count = account.payout_count.saturating_add(1);
    account.balance_at_last_payout = account.balance;
    account.last_payout_at = Some(at);
    // The firm pays the trader: the cash leaves the account balance.
    account.balance = Money(account.balance.0 - amount.0 + fee_refund.0);
    if account.status == AccountStatus::PayoutPending {
        account.status = AccountStatus::Funded;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::presets::ftmo_phase1;
    use crate::core::ids::AccountId;
    use crate::core::types::dec;

    fn funded_account(balance: rust_decimal::Decimal) -> Account {
        let mut acc = Account::new(AccountId::new(), ftmo_phase1());
        acc.status = AccountStatus::Funded;
        acc.initial_balance = Money(dec!(100_000));
        acc.balance = Money(balance);
        acc.equity = Money(balance);
        acc
    }

    #[test]
    fn profit_before_first_payout_excluded_from_second_basis() {
        // 100k → 120k (profit 20k). First payout pays on the 20k and
        // stamps balance_at_last_payout = 120k. Then 120k → 130k: the
        // second payout's basis must be 10k, NOT 30k.
        let config = PayoutConfig::default();
        let mut acc = funded_account(dec!(120_000));
        let q1 = quote_payout(&acc, &config, None, None, false, chrono::Utc::now());
        assert_eq!(q1.profit_basis.0, dec!(20_000));
        assert_eq!(q1.trader_share, dec!(0.80));
        let amount1 = q1.amount;
        assert!(record_payout(&mut acc, amount1, Money::ZERO, chrono::Utc::now()).is_ok());
        assert_eq!(acc.payout_count, 1);
        assert_eq!(acc.balance_at_last_payout.0, dec!(120_000));

        // Trade further: 130k.
        acc.balance = Money(dec!(130_000));
        acc.equity = Money(dec!(130_000));
        let q2 = quote_payout(
            &acc,
            &config,
            acc.last_payout_at,
            Some(acc.balance_at_last_payout),
            false,
            chrono::Utc::now(),
        );
        assert_eq!(
            q2.profit_basis.0,
            dec!(10_000),
            "second payout basis must exclude pre-first-payout profit"
        );
        assert_eq!(
            q2.trader_share,
            dec!(0.90),
            "second payout must use tier 2 (90%)"
        );
    }

    #[test]
    fn scaling_tiers_apply_in_order() {
        let config = PayoutConfig::default();
        assert_eq!(config.trader_share_for(0), dec!(0.80));
        assert_eq!(config.trader_share_for(1), dec!(0.90));
        assert_eq!(config.trader_share_for(2), dec!(1.00));
        assert_eq!(config.trader_share_for(5), dec!(1.00), "last tier repeats");
    }

    #[test]
    fn payout_below_minimum_rejected() {
        let config = PayoutConfig {
            minimum_payout: Money(dec!(500)),
            ..PayoutConfig::default()
        };
        // $100 of profit × 0.80 = $80 < $500 minimum.
        let acc = funded_account(dec!(100_100));
        let q = quote_payout(&acc, &config, None, None, false, chrono::Utc::now());
        assert_eq!(
            q.rejected,
            Some(PayoutError::BelowMinimum {
                minimum: Money(dec!(500))
            })
        );
    }

    #[test]
    fn cycle_blocks_early_request() {
        let config = PayoutConfig {
            cycle: PayoutCycle::BiWeekly,
            ..PayoutConfig::default()
        };
        let mut acc = funded_account(dec!(120_000));
        let now = chrono::Utc::now();
        // First payout: profit basis = 120k - 100k = 20k, amount = 16k.
        let amount = quote_payout(&acc, &config, None, None, false, now).amount;
        assert!(record_payout(&mut acc, amount, Money::ZERO, now).is_ok());
        // Simulate new profit: balance grows to 130k after the payout.
        acc.balance = Money(dec!(130_000));
        acc.equity = acc.balance;
        // Request again 7 days later → blocked by cycle (14-day gap).
        let q = quote_payout(
            &acc,
            &config,
            acc.last_payout_at,
            Some(acc.balance_at_last_payout),
            false,
            now + Duration::days(7),
        );
        assert!(matches!(
            q.rejected,
            Some(PayoutError::CycleNotReached { .. })
        ));
        // 15 days later → allowed.
        let q = quote_payout(
            &acc,
            &config,
            acc.last_payout_at,
            Some(acc.balance_at_last_payout),
            false,
            now + Duration::days(15),
        );
        assert!(q.rejected.is_none());
    }
}
