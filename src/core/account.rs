//! Account domain model and snapshot.
//!
//! An [`Account`] is the aggregate root for a trader's evaluation program.
//! It tracks the underlying balance, peak equity, drawdown basis, rule
//! plan, and lifecycle status. The engine never mutates an account in place;
//! instead it produces new [`AccountSnapshot`]s after each event.

use crate::config::plan::ChallengePlan;
use crate::core::ids::{AccountId, ChallengeId};
use crate::core::types::{Money, Pct, Timestamp, dec};
use crate::core::{Error, invalid_state};

/// Type of the account – distinguishes between evaluation phases and funded
/// status.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AccountType {
    /// Phase 1 evaluation.
    Phase1,
    /// Phase 2 evaluation (verification).
    Phase2,
    /// Funded live account.
    Funded,
    /// Demo / paper-trading.
    Demo,
}

impl std::fmt::Display for AccountType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AccountType::Phase1 => write!(f, "phase1"),
            AccountType::Phase2 => write!(f, "phase2"),
            AccountType::Funded => write!(f, "funded"),
            AccountType::Demo => write!(f, "demo"),
        }
    }
}

/// Lifecycle status of the account.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AccountStatus {
    /// Evaluation not started.
    Pending,
    /// Evaluation in progress.
    Active,
    /// Evaluation passed – awaiting upgrade.
    Passed,
    /// Evaluation failed.
    Failed,
    /// Funded and live.
    Funded,
    /// Payout pending.
    PayoutPending,
    /// Closed by firm.
    Closed,
}

impl AccountStatus {
    pub fn is_active(self) -> bool {
        matches!(self, AccountStatus::Active | AccountStatus::Funded)
    }
    pub fn is_terminal(self) -> bool {
        matches!(self, AccountStatus::Passed | AccountStatus::Failed | AccountStatus::Closed)
    }
}

/// The account aggregate root.
#[derive(Debug, Clone)]
pub struct Account {
    pub id: AccountId,
    pub account_type: AccountType,
    pub status: AccountStatus,
    pub challenge_id: ChallengeId,
    pub plan: ChallengePlan,

    /// Initial deposit (starting balance).
    pub initial_balance: Money,
    /// Current cash balance (after realized P&L, commission, swap, payouts).
    pub balance: Money,
    /// Floating equity = balance + unrealized P&L.
    pub equity: Money,
    /// Highest peak of equity seen so far (for trailing drawdown).
    pub peak_equity: Money,
    /// Highest peak of balance seen so far (for max drawdown).
    pub peak_balance: Money,
    /// Start of evaluation window.
    pub started_at: Option<Timestamp>,
    /// End of evaluation window (deadline).
    pub deadline: Option<Timestamp>,

    /// Daily starting balance (reset each server day).
    pub day_start_balance: Money,
    /// Day index (0-based) since evaluation start.
    pub trading_day_index: u32,
    /// Number of distinct trading days (days with at least one trade).
    pub active_trading_days: u32,

    /// Running tally of daily P&L for the current day.
    pub today_realized_pnl: Money,
    /// Running tally of all-time realized P&L.
    pub total_realized_pnl: Money,
    /// Total commissions paid.
    pub total_commissions: Money,
    /// Total swaps paid.
    pub total_swaps: Money,

    /// Largest single-day profit observed so far (for consistency rule).
    pub largest_day_profit: Money,
    /// Smallest single-day loss observed so far.
    pub largest_day_loss: Money,
}

impl Account {
    /// Creates a new account with a starting balance and a challenge plan.
    pub fn new(id: AccountId, plan: ChallengePlan) -> Self {
        let initial = plan.initial_balance();
        Account {
            id,
            account_type: match plan.phase {
                crate::config::plan::ChallengePhase::Phase1 => AccountType::Phase1,
                crate::config::plan::ChallengePhase::Phase2 => AccountType::Phase2,
                crate::config::plan::ChallengePhase::Funded => AccountType::Funded,
            },
            status: AccountStatus::Pending,
            challenge_id: plan.id,
            plan,
            initial_balance: initial,
            balance: initial,
            equity: initial,
            peak_equity: initial,
            peak_balance: initial,
            started_at: None,
            deadline: None,
            day_start_balance: initial,
            trading_day_index: 0,
            active_trading_days: 0,
            today_realized_pnl: Money::ZERO,
            total_realized_pnl: Money::ZERO,
            total_commissions: Money::ZERO,
            total_swaps: Money::ZERO,
            largest_day_profit: Money::ZERO,
            largest_day_loss: Money::ZERO,
        }
    }

    /// Marks the account as active and stamps the start time.
    pub fn start(mut self, at: Timestamp) -> Result<Self, Error> {
        if self.status != AccountStatus::Pending {
            return Err(invalid_state(format!(
                "account {} is not pending (status = {:?})",
                self.id, self.status
            )));
        }
        self.status = AccountStatus::Active;
        self.started_at = Some(at);
        if let Some(days) = self.plan.time_limit_days {
            self.deadline = Some(at + chrono::Duration::days(days as i64));
        }
        Ok(self)
    }

    /// Returns the maximum daily drawdown as a money amount, derived from
    /// the day-start balance.
    pub fn daily_dd_limit(&self) -> Money {
        let plan_dd = self.plan.max_daily_drawdown_pct;
        Money(plan_dd.0 * self.day_start_balance.0)
    }

    /// Returns the maximum total drawdown as a money amount.
    pub fn max_dd_limit(&self) -> Money {
        let plan_dd = self.plan.max_total_drawdown_pct;
        Money(plan_dd.0 * self.initial_balance.0)
    }

    /// Returns the profit target as a money amount.
    pub fn profit_target(&self) -> Money {
        let pct = self.plan.profit_target_pct;
        Money(pct.0 * self.initial_balance.0)
    }

    /// Current drawdown from peak equity.
    pub fn equity_drawdown(&self) -> Money {
        Money((self.peak_equity.0 - self.equity.0).max(dec!(0)))
    }

    /// Current drawdown from peak balance.
    pub fn balance_drawdown(&self) -> Money {
        Money((self.peak_balance.0 - self.balance.0).max(dec!(0)))
    }

    /// Current daily drawdown (from day start).
    pub fn daily_drawdown(&self) -> Money {
        Money((self.day_start_balance.0 - self.equity.0).max(dec!(0)))
    }

    /// Net profit (current balance - initial balance).
    pub fn net_profit(&self) -> Money {
        Money(self.balance.0 - self.initial_balance.0)
    }

    /// Returns true if the profit target has been reached (using balance).
    pub fn reached_profit_target(&self) -> bool {
        self.net_profit().0 >= self.profit_target().0
    }

    /// Returns the daily drawdown utilization as a percentage (0..1).
    pub fn daily_dd_utilization(&self) -> Pct {
        let limit = self.daily_dd_limit();
        if limit.0.is_zero() {
            return Pct::ZERO;
        }
        Pct(self.daily_drawdown().0 / limit.0)
    }

    /// Returns the total drawdown utilization as a percentage (0..1).
    pub fn max_dd_utilization(&self) -> Pct {
        let limit = self.max_dd_limit();
        if limit.0.is_zero() {
            return Pct::ZERO;
        }
        Pct(self.balance_drawdown().0 / limit.0)
    }
}

/// Point-in-time snapshot of an account, suitable for serialization and
/// transmission to the trader UI.
#[derive(Debug, Clone)]
pub struct AccountSnapshot {
    pub id: AccountId,
    pub account_type: AccountType,
    pub status: AccountStatus,
    pub initial_balance: Money,
    pub balance: Money,
    pub equity: Money,
    pub peak_equity: Money,
    pub peak_balance: Money,
    pub day_start_balance: Money,
    pub net_profit: Money,
    pub daily_drawdown: Money,
    pub total_drawdown: Money,
    pub daily_dd_utilization: Pct,
    pub max_dd_utilization: Pct,
    pub profit_target: Money,
    pub profit_target_utilization: Pct,
    pub active_trading_days: u32,
    pub trading_day_index: u32,
    pub started_at: Option<Timestamp>,
    pub deadline: Option<Timestamp>,
    pub challenge_id: ChallengeId,
}

impl From<&Account> for AccountSnapshot {
    fn from(a: &Account) -> Self {
        AccountSnapshot {
            id: a.id,
            account_type: a.account_type,
            status: a.status,
            initial_balance: a.initial_balance,
            balance: a.balance,
            equity: a.equity,
            peak_equity: a.peak_equity,
            peak_balance: a.peak_balance,
            day_start_balance: a.day_start_balance,
            net_profit: a.net_profit(),
            daily_drawdown: a.daily_drawdown(),
            total_drawdown: a.balance_drawdown(),
            daily_dd_utilization: a.daily_dd_utilization(),
            max_dd_utilization: a.max_dd_utilization(),
            profit_target: a.profit_target(),
            profit_target_utilization: {
                let t = a.profit_target();
                if t.0.is_zero() { Pct::ZERO } else { Pct(a.net_profit().0.max(dec!(0)) / t.0) }
            },
            active_trading_days: a.active_trading_days,
            trading_day_index: a.trading_day_index,
            started_at: a.started_at,
            deadline: a.deadline,
            challenge_id: a.challenge_id,
        }
    }
}
