//! Account domain model and snapshot.
//!
//! An [`Account`] is the aggregate root for a trader's evaluation program.
//! It tracks the underlying balance, peak equity, drawdown basis, rule
//! plan, and lifecycle status. The engine never mutates an account in place;
//! instead it produces new [`AccountSnapshot`]s after each event.

use crate::config::plan::ChallengePlan;
use crate::core::ids::{AccountId, ChallengeId};
use crate::core::types::{dec, Money, Pct, Timestamp};
use crate::core::{invalid_state, Error};

/// Type of the account – distinguishes between evaluation phases and funded
/// status.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serialization", derive(serde::Serialize, serde::Deserialize))]
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
#[cfg_attr(feature = "serialization", derive(serde::Serialize, serde::Deserialize))]
pub enum AccountStatus {
    /// Evaluation not started.
    Pending,
    /// Evaluation in progress.
    Active,
    /// Profit target has been reached but `min_trading_days` not yet met
    /// (per the binding spec's `target_hit_pending` state). The account
    /// is still tradable, but cannot be promoted to [`AccountStatus::Passed`]
    /// until the day count is satisfied. Once set, `target_reached_at` is
    /// *never cleared* even if equity subsequently dips below target.
    TargetHitPending,
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
    /// Account was terminated via emergency stop (ops/compliance action).
    /// Transitions to [`AccountStatus::Failed`] only via an explicit,
    /// audited override.
    EmergencyStopped,
}

impl AccountStatus {
    #[must_use]
    pub fn is_active(self) -> bool {
        matches!(
            self,
            AccountStatus::Active | AccountStatus::Funded | AccountStatus::TargetHitPending
        )
    }
    #[must_use]
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            AccountStatus::Passed
                | AccountStatus::Failed
                | AccountStatus::Closed
                | AccountStatus::EmergencyStopped
        )
    }
    /// Returns true if this status was reached as a result of a breach
    /// (vs. a positive outcome).
    #[must_use]
    pub fn is_breach_terminal(self) -> bool {
        matches!(
            self,
            AccountStatus::Failed | AccountStatus::EmergencyStopped
        )
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

    /// **P1-9 fix**: tenant this account belongs to. Required for the
    /// `PFaaS` platform's correctness property: no cross-tenant data
    /// leakage, even in the same database table. Filtered on every
    /// store read.
    pub tenant_id: crate::tenant::TenantId,

    /// Initial deposit (starting balance).
    pub initial_balance: Money,
    /// Current cash balance (after realized P&L, commission, swap, payouts).
    pub balance: Money,
    /// Floating equity = balance + unrealized P&L.
    pub equity: Money,
    pub estimated_equity: Money,
    pub estimated_balance: Money,
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
    /// **A.6 fix**: whether the current trading day has already been
    /// counted in `active_trading_days` (via [`AccountState::mark_active_trading_day`]).
    /// Reset to `false` at each rollover. Prevents double-counting
    /// when the first trade is counted immediately rather than at rollover.
    pub day_counted_today: bool,

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
    /// **P1.3 fix**: sum of all positive daily profits (days where
    /// `today_realized_pnl > 0` at rollover). Used as the denominator
    /// by the consistency rule — industry-standard denominator, not
    /// `total_realized_pnl` which includes losses.
    pub sum_positive_days_profit: Money,

    /// **P1.1 fix**: equity at the start of the current server day. Used
    /// by `daily_drawdown()` when `drawdown_on_balance = false` (the
    /// default per the binding spec) so daily drawdown is computed from
    /// EOD equity rather than the intraday balance.
    pub day_start_equity: Money,

    /// **Day-rollover fix**: persisted start of the account's current
    /// trading day. The pipeline uses this for day-boundary comparisons
    /// instead of `Utc::now()`, so rollover is based on the account's
    /// recorded day rather than the current wall-clock day.
    pub current_trading_day_start: Option<Timestamp>,

    /// **P0-2 fix**: Timestamp the profit target was first reached, or
    /// `None` if not yet. Once set, *never cleared* — even if equity
    /// subsequently dips below target before `min_trading_days` is met.
    /// The account stays in [`AccountStatus::TargetHitPending`] until
    /// the day count is satisfied, at which point it transitions to
    /// [`AccountStatus::Passed`].
    pub target_reached_at: Option<Timestamp>,
    /// Day index (0-based) on which the profit target was first reached.
    /// Used together with `target_reached_at` for the
    /// `target_hit_pending` → `passed` state transition.
    pub target_reached_on_day: Option<u32>,

    /// Optimistic-concurrency version (P1-8 fix). Bumped on every
    /// successful write. The store rejects `put_with_version(v)` calls
    /// where `v` does not match the persisted value, returning a
    /// [`crate::core::Error::StateConflict`].
    pub version: u64,

    /// Last-evaluated tick timestamp (P1-14 fix). Used by the
    /// out-of-order-tick guard: any tick with `ts <= last_tick_ts` is
    /// rejected before evaluation runs, preventing replay/reordering
    /// from silently producing a different verdict.
    pub last_tick_ts: Option<Timestamp>,

    /// **P1.4/P1.5 fix**: execution time of the most recent fill.
    /// Updated by the pipeline on every `TradeFilled` event; used by
    /// the inactivity-termination rule (N days without a trade).
    pub last_trade_at: Option<Timestamp>,

    /// **§D.2 fix**: number of payouts executed on this account.
    /// Drives the scaling-plan tier selection (80 → 90 → 100).
    pub payout_count: u32,
    /// **§D.2 fix**: balance watermark stamped at the last approved
    /// payout. The next payout's profit basis is `balance −` this value
    /// (or `initial_balance` before the first payout), so profit already
    /// paid out is never paid again.
    pub balance_at_last_payout: Money,
    /// **§D.2 fix**: when the last payout was approved (cycle enforcement).
    pub last_payout_at: Option<Timestamp>,
    /// **§D.2 fix**: whether the enrolment-fee refund has already been
    /// consumed (paid out once alongside the first payout when the plan
    /// is refundable).
    pub refund_used: bool,
}

impl Account {
    /// Creates a new account with a starting balance and a challenge plan.
    #[must_use]
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
            tenant_id: crate::tenant::TenantId::new(),
            initial_balance: initial,
            balance: initial,
            equity: initial,
            estimated_equity: initial,
            estimated_balance: initial,
            peak_equity: initial,
            peak_balance: initial,
            started_at: None,
            deadline: None,
            day_start_balance: initial,
            trading_day_index: 0,
            active_trading_days: 0,
            day_counted_today: false,
            today_realized_pnl: Money::ZERO,
            total_realized_pnl: Money::ZERO,
            total_commissions: Money::ZERO,
            total_swaps: Money::ZERO,
            largest_day_profit: Money::ZERO,
            largest_day_loss: Money::ZERO,
            sum_positive_days_profit: Money::ZERO,
            day_start_equity: initial,
            current_trading_day_start: None,
            target_reached_at: None,
            target_reached_on_day: None,
            version: 0,
            last_tick_ts: None,
            last_trade_at: None,
            payout_count: 0,
            balance_at_last_payout: initial,
            last_payout_at: None,
            refund_used: false,
        }
    }

    /// **P1-9 fix**: sets the tenant id on the account. Call this
    /// immediately after `Account::new()` — every account MUST have
    /// a tenant id before being persisted.
    #[must_use]
    pub fn with_tenant(mut self, tenant_id: crate::tenant::TenantId) -> Self {
        self.tenant_id = tenant_id;
        self
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
        self.current_trading_day_start = Some(self.plan.trading_day_start(at));
        if let Some(days) = self.plan.time_limit_days {
            self.deadline = Some(at + chrono::Duration::days(i64::from(days)));
        }
        Ok(self)
    }

    /// Returns the maximum daily drawdown as a money amount, derived from
    /// the day-start equity (or balance, when `drawdown_on_balance` is set).
    /// Daily drawdown is *always* a day-anchored measure (resets at rollover);
    /// the static-vs-trailing distinction only applies to the *total*
    /// drawdown rule, not the daily one.
    #[must_use]
    pub fn daily_dd_limit(&self) -> Money {
        let plan_dd = self.plan.max_daily_drawdown_pct;
        let day_start = if self.plan.drawdown_on_balance {
            self.day_start_balance
        } else {
            self.day_start_equity
        };
        Money(plan_dd.0 * day_start.0)
    }

    /// **P0-1 fix**: Returns the *static* maximum total drawdown limit,
    /// i.e. `max_total_drawdown_pct * initial_balance`. The floor never
    /// moves regardless of how high the account grows.
    ///
    /// For an account with $100k initial and a 10% static max loss, this
    /// returns $10k *forever* — the trader can grow to $200k and pull back
    /// to $190k without ever tripping this rule, because $190k is still
    /// $10k above the static $90k floor.
    #[must_use]
    pub fn max_dd_limit_static(&self) -> Money {
        let plan_dd = self.plan.max_total_drawdown_pct;
        Money(plan_dd.0 * self.initial_balance.0)
    }

    /// **P0-1 fix**: Returns the *trailing* maximum total drawdown limit,
    /// i.e. `max_total_drawdown_pct * peak_balance`. The floor floats up
    /// as the account grows.
    ///
    /// This is what `TrailingDrawdownRule` already computes internally;
    /// `MaxDrawdownRule` should call this when `plan.max_loss_reference ==
    /// Trailing`, and call [`max_dd_limit_static`](Self::max_dd_limit_static)
    /// when `== Static`.
    #[must_use]
    pub fn max_dd_limit_trailing(&self) -> Money {
        let plan_dd = self.plan.max_total_drawdown_pct;
        Money(plan_dd.0 * self.peak_balance.0)
    }

    /// **P1.6 fix**: Returns the *EOD-reset-trailing* maximum total drawdown
    /// limit, i.e. `max_total_drawdown_pct * eod_reference_balance`. The
    /// floor floats up *once per day* at the trading-session rollover —
    /// distinct from continuous `Trailing` (which floats intraday).
    ///
    /// The EOD reference is `day_start_balance` (the closing balance from
    /// the prior trading day, captured at rollover). The timezone-aware
    /// day model (P1.5) provides the rollover boundaries in the plan's
    /// configured timezone.
    #[must_use]
    pub fn max_dd_limit_eod_trailing(&self) -> Money {
        let plan_dd = self.plan.max_total_drawdown_pct;
        Money(plan_dd.0 * self.day_start_balance.0)
    }

    /// **P0-1 fix**: Returns the maximum total drawdown limit appropriate
    /// for this account's `max_loss_reference` setting (static, trailing,
    /// or `eod_trailing`). Dispatches to the specific limit function.
    #[must_use]
    pub fn max_dd_limit(&self) -> Money {
        match self.plan.max_loss_reference {
            crate::config::plan::LossReference::Static => self.max_dd_limit_static(),
            crate::config::plan::LossReference::Trailing => self.max_dd_limit_trailing(),
            crate::config::plan::LossReference::EodTrailing => self.max_dd_limit_eod_trailing(),
        }
    }

    /// **P0-1 fix**: Returns the current total drawdown measured against
    /// the *same reference* the limit is computed from, so the two are
    /// always consistent. When the plan uses `Static`, the drawdown is
    /// measured from `initial_balance`; when `Trailing`, from `peak_balance`;
    /// when `EodTrailing`, from `day_start_balance` (prior day close).
    ///
    /// This is the function `MaxDrawdownRule` should call to compute
    /// `dd` *and* `limit` — they will always use the same reference point,
    /// which is the whole point of the P0-1 fix.
    #[must_use]
    pub fn total_drawdown(&self) -> Money {
        let reference = match self.plan.max_loss_reference {
            crate::config::plan::LossReference::Static => self.initial_balance,
            crate::config::plan::LossReference::Trailing => self.peak_balance,
            crate::config::plan::LossReference::EodTrailing => self.day_start_balance,
        };
        let current = if self.plan.drawdown_on_balance {
            self.balance
        } else {
            self.equity
        };
        Money((reference.0 - current.0).max(dec!(0)))
    }

    /// **Backward-compat helper**: same as `total_drawdown()` when in
    /// trailing mode, i.e. the old behavior pre-P0-1. Prefer
    /// [`total_drawdown`](Self::total_drawdown) in new code.
    #[must_use]
    pub fn balance_drawdown(&self) -> Money {
        Money((self.peak_balance.0 - self.balance.0).max(dec!(0)))
    }

    /// Returns the profit target as a money amount.
    #[must_use]
    pub fn profit_target(&self) -> Money {
        let pct = self.plan.profit_target_pct;
        Money(pct.0 * self.initial_balance.0)
    }

    /// Current drawdown from peak equity.
    #[must_use]
    pub fn equity_drawdown(&self) -> Money {
        Money((self.peak_equity.0 - self.equity.0).max(dec!(0)))
    }

    /// Current daily drawdown (from day start). The basis (balance or
    /// equity) is controlled by `plan.drawdown_on_balance`; defaults to
    /// equity-based (the binding spec's `daily_pnl_cents = equity_now -
    /// day_start_equity_cents`).
    ///
    /// **P1.1 fix**: uses `day_start_equity` (not `day_start_balance`)
    /// as the day-start reference so the drawdown is measured from the
    /// correct equity snapshot, not the balance snapshot. The limit
    /// (`daily_dd_limit()`) is also anchored on `day_start_equity` for
    /// consistency.
    #[must_use]
    pub fn daily_drawdown(&self) -> Money {
        let (day_start, current) = if self.plan.drawdown_on_balance {
            (self.day_start_balance, self.balance)
        } else {
            (self.day_start_equity, self.equity)
        };
        Money((day_start.0 - current.0).max(dec!(0)))
    }

    /// Net profit (current balance - initial balance).
    #[must_use]
    pub fn net_profit(&self) -> Money {
        Money(self.balance.0 - self.initial_balance.0)
    }

    /// Returns true if the profit target has been reached (using balance).
    #[must_use]
    pub fn reached_profit_target(&self) -> bool {
        self.net_profit().0 >= self.profit_target().0
    }

    /// Returns the daily drawdown utilization as a percentage (0..1).
    #[must_use]
    pub fn daily_dd_utilization(&self) -> Pct {
        let limit = self.daily_dd_limit();
        if limit.0.is_zero() {
            return Pct::ZERO;
        }
        Pct(self.daily_drawdown().0 / limit.0)
    }

    /// Returns the total drawdown utilization as a percentage (0..1),
    /// measured against the *same* reference (static or trailing) used
    /// to compute the limit. (P0-1 fix.)
    #[must_use]
    pub fn max_dd_utilization(&self) -> Pct {
        let limit = self.max_dd_limit();
        if limit.0.is_zero() {
            return Pct::ZERO;
        }
        Pct(self.total_drawdown().0 / limit.0)
    }

    /// **P0-2 helper**: returns true if the profit target has been hit
    /// and is now in pending state (recorded via `target_reached_at`).
    /// This is *sticky* — once true, it stays true even if equity dips
    /// back below target before `min_trading_days` is satisfied.
    #[must_use]
    pub fn target_pending(&self) -> bool {
        self.target_reached_at.is_some()
    }

    /// **P0-2 helper**: returns true if the profit target has been hit
    /// AND the minimum trading days requirement has been met — i.e. the
    /// account is eligible to be promoted to `Passed`.
    #[must_use]
    pub fn target_fully_satisfied(&self) -> bool {
        self.target_reached_at.is_some() && self.active_trading_days >= self.plan.min_trading_days
    }
}

/// Point-in-time snapshot of an account, suitable for serialization and
/// transmission to the trader UI.
#[derive(Debug, Clone)]
#[cfg_attr(feature = "serialization", derive(serde::Serialize, serde::Deserialize))]
pub struct AccountSnapshot {
    pub id: AccountId,
    pub account_type: AccountType,
    pub status: AccountStatus,
    pub initial_balance: Money,
    pub balance: Money,
    pub equity: Money,
    pub estimated_equity: Money,
    pub estimated_balance: Money,
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
    /// **P0-2 fix**: timestamp the profit target was first reached
    /// (sticky; never cleared once set). Null until target is hit.
    pub target_reached_at: Option<Timestamp>,
    /// **P0-1 fix**: the loss reference mode in effect (static or trailing).
    pub max_loss_reference: crate::config::plan::LossReference,
    /// **P1-8 fix**: optimistic-concurrency version.
    pub version: u64,
    /// **P1-9 fix**: tenant id this account belongs to.
    pub tenant_id: crate::tenant::TenantId,
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
            estimated_equity: a.estimated_equity,
            estimated_balance: a.estimated_balance,
            peak_equity: a.peak_equity,
            peak_balance: a.peak_balance,
            day_start_balance: a.day_start_balance,
            net_profit: a.net_profit(),
            daily_drawdown: a.daily_drawdown(),
            total_drawdown: a.total_drawdown(),
            daily_dd_utilization: a.daily_dd_utilization(),
            max_dd_utilization: a.max_dd_utilization(),
            profit_target: a.profit_target(),
            profit_target_utilization: {
                let t = a.profit_target();
                if t.0.is_zero() {
                    Pct::ZERO
                } else {
                    Pct(a.net_profit().0.max(dec!(0)) / t.0)
                }
            },
            active_trading_days: a.active_trading_days,
            trading_day_index: a.trading_day_index,
            started_at: a.started_at,
            deadline: a.deadline,
            challenge_id: a.challenge_id,
            target_reached_at: a.target_reached_at,
            max_loss_reference: a.plan.max_loss_reference,
            version: a.version,
            tenant_id: a.tenant_id,
        }
    }
}
