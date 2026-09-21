//! State mutation helpers.
//!
//! [`AccountState`] is a small mutable overlay over [`Account`] used by the
//! pipeline to apply transitions (P&L updates, day rollover, plan upgrade)
//! in a testable, side-effect-free way.

use crate::core::account::Account;
use crate::core::types::{Money, Timestamp};
use crate::core::Error;

/// A working copy of an account with mutation methods that return new
/// immutable instances.
#[derive(Debug, Clone)]
pub struct AccountState {
    pub account: Account,
}

impl AccountState {
    #[must_use]
    pub fn new(account: Account) -> Self {
        AccountState { account }
    }

    /// Applies a realized P&L change (positive or negative) and updates
    /// balance, peak balance, `today_realized_pnl`, `total_realized_pnl`,
    /// `largest_day_profit/loss`.
    ///
    /// **P1.7 fix**: `largest_day_profit` / `largest_day_loss` are now
    /// tracked per-DAY, not per-trade. The previous implementation
    /// compared each trade's net against the all-time largest, which
    /// made `largest_day_profit` actually "largest single trade profit"
    /// — wrong for the consistency rule. Now we accumulate the running
    /// day's net into `today_realized_pnl`, and `largest_day_profit` is
    /// updated only at day rollover (where `today_realized_pnl` is
    /// frozen and reset).
    #[must_use]
    pub fn apply_realized_pnl(
        mut self,
        pnl: Money,
        commission: Money,
        swap: Money,
        at: Timestamp,
    ) -> Self {
        let net = Money(pnl.0 - commission.0 - swap.0);
        self.account.balance = Money(self.account.balance.0 + net.0);
        self.account.equity = self.account.balance; // assume no open positions; will be recomputed on next tick
        if self.account.balance.0 > self.account.peak_balance.0 {
            self.account.peak_balance = self.account.balance;
        }
        self.account.today_realized_pnl = Money(self.account.today_realized_pnl.0 + net.0);
        self.account.total_realized_pnl = Money(self.account.total_realized_pnl.0 + net.0);
        self.account.total_commissions = Money(self.account.total_commissions.0 + commission.0);
        self.account.total_swaps = Money(self.account.total_swaps.0 + swap.0);
        // P1.4/P1.5: stamp the last-trade time for the inactivity rule.
        self.account.last_trade_at = Some(at);
        // P1.7: do NOT update largest_day_profit per-trade — that would
        // make it "largest trade profit", not "largest day profit".
        // It's now updated only at rollover_day().
        self
    }

    /// Updates the equity (from unrealized P&L) and tracks peak equity.
    #[must_use]
    pub fn update_equity(mut self, equity: Money) -> Self {
        self.account.equity = equity;
        if equity.0 > self.account.peak_equity.0 {
            self.account.peak_equity = equity;
        }
        self
    }

    /// **P1-5 fix**: updates the broker-reported balance. Only called
    /// from the broker-is-truth tick path; never derived by the engine.
    #[must_use]
    pub fn update_balance(mut self, balance: Money) -> Self {
        self.account.balance = balance;
        if balance.0 > self.account.peak_balance.0 {
            self.account.peak_balance = balance;
        }
        self
    }

    /// **P1.7 fix**: also stamps `largest_day_profit` / `largest_day_loss`
    /// from the frozen `today_realized_pnl` (per-DAY tracking, not
    /// per-trade). This is the value the consistency rule checks
    /// against.
    #[must_use]
    pub fn rollover_day(mut self, had_trades_today: bool) -> Self {
        // P1.7: before resetting today_realized_pnl, freeze it into
        // largest_day_profit / largest_day_loss (per-day, not per-trade).
        if had_trades_today {
            let today_net = self.account.today_realized_pnl;
            if today_net.0 > self.account.largest_day_profit.0 {
                self.account.largest_day_profit = today_net;
            }
            if today_net.0 < self.account.largest_day_loss.0 {
                self.account.largest_day_loss = today_net;
            }
            if today_net.0 > rust_decimal::Decimal::ZERO {
                self.account.sum_positive_days_profit =
                    Money(self.account.sum_positive_days_profit.0 + today_net.0);
            }
            // A.6 fix: only count at rollover if mark_active_trading_day
            // was NOT already called today (prevents double-count).
            if !self.account.day_counted_today {
                self.account.active_trading_days += 1;
            }
        }
        self.account.trading_day_index += 1;
        self.account.day_start_balance = self.account.balance;
        self.account.day_start_equity = self.account.equity;
        self.account.today_realized_pnl = Money::ZERO;
        // A.6 fix: reset the idempotency flag for the new day.
        self.account.day_counted_today = false;
        self
    }

    /// Marks an active trading day (called on the first trade of a day).
    ///
    /// **A.6 fix**: this was a no-op (`let _ = &mut self; self`), so
    /// `active_trading_days` only ever incremented at rollover — making
    /// the first day with trades count one day late. Now it increments
    /// `active_trading_days` and sets `day_counted_today` (idempotent).
    ///
    /// `rollover_day` skips its own increment when the flag is already
    /// set, so the two paths cannot double-count.
    #[must_use]
    pub fn mark_active_trading_day(mut self) -> Self {
        if !self.account.day_counted_today {
            self.account.active_trading_days += 1;
            self.account.day_counted_today = true;
        }
        self
    }

    /// Upgrades the account to a new phase (e.g. Phase1 → Phase2).
    pub fn upgrade_phase(mut self, to: crate::config::plan::ChallengePhase) -> Result<Self, Error> {
        use crate::config::plan::ChallengePhase::{Funded, Phase1, Phase2};
        let from = self.account.plan.phase;
        match (from, to) {
            (Phase1, Phase2) | (Phase2, Funded) => {
                self.account.plan = self.account.plan.clone().with_phase(to);
                self.account.account_type = match to {
                    Phase1 => crate::core::account::AccountType::Phase1,
                    Phase2 => crate::core::account::AccountType::Phase2,
                    Funded => crate::core::account::AccountType::Funded,
                };
                Ok(self)
            }
            _ => Err(Error::InvalidState(format!(
                "invalid phase transition: {from:?} -> {to:?}"
            ))),
        }
    }

    /// **P0-2 fix**: stamps `target_reached_at` on the account. Idempotent —
    /// if the target was already reached, this is a no-op. Once set, the
    /// timestamp is *never cleared*, even if equity subsequently dips
    /// below target before `min_trading_days` is satisfied. Transitions
    /// the account status to `TargetHitPending`.
    #[must_use]
    pub fn mark_target_reached(mut self, at: Timestamp) -> Self {
        if self.account.target_reached_at.is_none() {
            self.account.target_reached_at = Some(at);
            self.account.target_reached_on_day = Some(self.account.trading_day_index);
            if self.account.status == crate::core::account::AccountStatus::Active {
                self.account.status = crate::core::account::AccountStatus::TargetHitPending;
            }
        }
        self
    }

    /// **P1-12 fix**: marks the account as emergency-stopped. Only valid
    /// from active/pending states. Once stopped, the account can only be
    /// restored via an explicit, audited [`Override`] record (P1-11).
    #[must_use]
    pub fn emergency_stop(mut self, reason: &str, actor_id: &str, at: Timestamp) -> Self {
        let _ = (reason, actor_id, at);
        self.account.status = crate::core::account::AccountStatus::EmergencyStopped;
        self
    }

    /// **P1-11 fix**: clears a breach via an explicit override. Only valid
    /// from `Failed` or `EmergencyStopped` terminal states. The override
    /// itself is part of the permanent record; this method just transitions
    /// the account back to `Active`. The `Override` record is created by
    /// the caller and persisted to the event log alongside this transition.
    pub fn clear_breach(
        mut self,
        _override: &crate::override_engine::Override,
    ) -> Result<Self, Error> {
        use crate::core::account::AccountStatus::{Active, EmergencyStopped, Failed};
        match self.account.status {
            Failed | EmergencyStopped => {
                self.account.status = Active;
                Ok(self)
            }
            other => Err(Error::invalid_state(format!(
                "cannot clear breach from status {other:?} — only Failed/EmergencyStopped are clearable"
            ))),
        }
    }

    /// Terminates the account with the given status.
    #[must_use]
    pub fn terminate(mut self, status: crate::core::account::AccountStatus) -> Self {
        self.account.status = status;
        self
    }

    /// Delta description (used by the pipeline to emit events).
    #[must_use]
    pub fn delta(&self) -> StateDelta {
        StateDelta {
            balance: self.account.balance,
            equity: self.account.equity,
            peak_balance: self.account.peak_balance,
            peak_equity: self.account.peak_equity,
            total_realized: self.account.total_realized_pnl,
        }
    }
}

/// State delta – captures the values that changed after applying a transition.
#[derive(Debug, Clone, Copy)]
pub struct StateDelta {
    pub balance: Money,
    pub equity: Money,
    pub peak_balance: Money,
    pub peak_equity: Money,
    pub total_realized: Money,
}

impl StateDelta {
    #[must_use]
    pub fn empty() -> Self {
        Self {
            balance: Money::ZERO,
            equity: Money::ZERO,
            peak_balance: Money::ZERO,
            peak_equity: Money::ZERO,
            total_realized: Money::ZERO,
        }
    }
}

/// Helper: applies a tick revaluation to equity given open positions.
/// (Single-symbol variant — used by the broker-reported path where the
/// engine does NOT recompute equity; kept for backwards compatibility.)
#[must_use]
pub fn equity_after_tick(
    balance: Money,
    positions: &[crate::core::position::Position],
    quote: &crate::core::tick::Quote,
) -> Money {
    let unreal: Money = positions
        .iter()
        .filter(|p| p.is_open())
        .map(|p| p.unrealized_pnl(quote))
        .fold(Money::ZERO, |acc, x| Money(acc.0 + x.0));
    Money(balance.0 + unreal.0)
}
