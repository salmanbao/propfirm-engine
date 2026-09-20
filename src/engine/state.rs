//! State mutation helpers.
//!
//! [`AccountState`] is a small mutable overlay over [`Account`] used by the
//! pipeline to apply transitions (P&L updates, day rollover, plan upgrade)
//! in a testable, side-effect-free way.

use crate::core::account::Account;
use crate::core::types::{Money, Timestamp, dec};
use crate::core::Error;

/// A working copy of an account with mutation methods that return new
/// immutable instances.
#[derive(Debug, Clone)]
pub struct AccountState {
    pub account: Account,
}

impl AccountState {
    pub fn new(account: Account) -> Self { AccountState { account } }

    /// Applies a realized P&L change (positive or negative) and updates
    /// balance, peak balance, today_realized_pnl, total_realized_pnl,
    /// largest_day_profit/loss.
    pub fn apply_realized_pnl(mut self, pnl: Money, commission: Money, swap: Money, at: Timestamp) -> Self {
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
        if net.0 > self.account.largest_day_profit.0 {
            self.account.largest_day_profit = net;
        }
        if net.0 < self.account.largest_day_loss.0 {
            self.account.largest_day_loss = net;
        }
        let _ = at;
        self
    }

    /// Updates the equity (from unrealized P&L) and tracks peak equity.
    pub fn update_equity(mut self, equity: Money) -> Self {
        self.account.equity = equity;
        if equity.0 > self.account.peak_equity.0 {
            self.account.peak_equity = equity;
        }
        self
    }

    /// Rolls over a new trading day. Resets today_realized_pnl, updates
    /// day_start_balance to the current balance, bumps day index, and
    /// increments active_trading_days if the previous day had any trades.
    pub fn rollover_day(mut self, had_trades_today: bool) -> Self {
        if had_trades_today {
            self.account.active_trading_days += 1;
        }
        self.account.trading_day_index += 1;
        self.account.day_start_balance = self.account.balance;
        self.account.today_realized_pnl = Money::ZERO;
        self
    }

    /// Marks an active trading day (called on the first trade of a day).
    pub fn mark_active_trading_day(mut self) -> Self {
        // active_trading_days is incremented at rollover if had_trades_today;
        // here we just ensure today counts. The increment happens at rollover.
        let _ = &mut self;
        self
    }

    /// Upgrades the account to a new phase (e.g. Phase1 → Phase2).
    pub fn upgrade_phase(mut self, to: crate::config::plan::ChallengePhase) -> Result<Self, Error> {
        use crate::config::plan::ChallengePhase::*;
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
                "invalid phase transition: {:?} -> {:?}",
                from, to
            ))),
        }
    }

    /// Terminates the account with the given status.
    pub fn terminate(mut self, status: crate::core::account::AccountStatus) -> Self {
        self.account.status = status;
        self
    }

    /// Delta description (used by the pipeline to emit events).
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
pub fn equity_after_tick(balance: Money, positions: &[crate::core::position::Position], quote: &crate::core::tick::Quote) -> Money {
    let _ = dec!(0);
    let unreal: Money = positions.iter().filter(|p| p.is_open()).map(|p| p.unrealized_pnl(quote)).fold(Money::ZERO, |acc, x| Money(acc.0 + x.0));
    Money(balance.0 + unreal.0)
}
