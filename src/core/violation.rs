//! Violation record produced by rule evaluation.
//!
//! When a [`Rule`](crate::rules::Rule) is violated, the engine emits a
//! [`Violation`] describing what happened, why, and how severe it is. The
//! violation is also persisted in the event log for auditability.

use crate::core::ids::{RuleId, ViolationId, AccountId};
use crate::core::types::{Decimal, Money, Timestamp, dec};
use crate::core::Error;

/// Severity of a violation – determines whether it terminates the account
/// or just warns.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum ViolationSeverity {
    /// Informational only; no consequence.
    Info,
    /// Soft warning; the trader is notified but the account continues.
    Warning,
    /// Hard violation; the account is terminated.
    Hard,
    /// Critical: immediate liquidation of all open positions.
    Liquidate,
}

impl std::fmt::Display for ViolationSeverity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ViolationSeverity::Info => write!(f, "info"),
            ViolationSeverity::Warning => write!(f, "warning"),
            ViolationSeverity::Hard => write!(f, "hard"),
            ViolationSeverity::Liquidate => write!(f, "liquidate"),
        }
    }
}

/// Category of violation. Used for filtering and dashboards.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ViolationKind {
    /// Daily drawdown limit breached.
    DailyDrawdown,
    /// Maximum (total) drawdown limit breached.
    MaxDrawdown,
    /// Trailing drawdown limit breached.
    TrailingDrawdown,
    /// Profit target not yet reached.
    ProfitTargetMissed,
    /// Minimum trading days not met.
    MinTradingDays,
    /// Single-day profit too large relative to total profit (consistency rule).
    Consistency,
    /// Trading during high-impact news events.
    NewsTrading,
    /// Holding positions overnight.
    OvernightHolding,
    /// Holding positions over the weekend.
    WeekendHolding,
    /// Position size exceeded.
    MaxPositionSize,
    /// Lot size exceeded.
    MaxLotSize,
    /// Number of simultaneously open positions exceeded.
    MaxOpenPositions,
    /// Number of trades in a single day exceeded.
    MaxDailyTrades,
    /// Account time limit exceeded.
    TimeLimit,
    /// Cooldown period violated (e.g. between trades).
    Cooldown,
    /// Hedging detected (opposing positions on same symbol).
    Hedging,
    /// Grid / martingale strategy detected.
    GridTrading,
    /// Copy-trading or account-sharing detected.
    CopyTrading,
    /// Stop-loss not set on order.
    MissingStopLoss,
    /// Take-profit not set on order.
    MissingTakeProfit,
    /// Custom rule violation.
    Custom,
}

impl std::fmt::Display for ViolationKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        use ViolationKind::*;
        let s = match self {
            DailyDrawdown => "daily_drawdown",
            MaxDrawdown => "max_drawdown",
            TrailingDrawdown => "trailing_drawdown",
            ProfitTargetMissed => "profit_target_missed",
            MinTradingDays => "min_trading_days",
            Consistency => "consistency",
            NewsTrading => "news_trading",
            OvernightHolding => "overnight_holding",
            WeekendHolding => "weekend_holding",
            MaxPositionSize => "max_position_size",
            MaxLotSize => "max_lot_size",
            MaxOpenPositions => "max_open_positions",
            MaxDailyTrades => "max_daily_trades",
            TimeLimit => "time_limit",
            Cooldown => "cooldown",
            Hedging => "hedging",
            GridTrading => "grid_trading",
            CopyTrading => "copy_trading",
            MissingStopLoss => "missing_stop_loss",
            MissingTakeProfit => "missing_take_profit",
            Custom => "custom",
        };
        write!(f, "{s}")
    }
}

/// A rule violation record. Stored in the audit log and surfaced to the
/// trader via notifications.
#[derive(Debug, Clone)]
pub struct Violation {
    pub id: ViolationId,
    pub account_id: AccountId,
    /// **P1-9 fix**: tenant this violation belongs to. Required so the
    /// breach-report endpoint (TD-25) can filter by tenant.
    pub tenant_id: crate::tenant::TenantId,
    pub rule_id: RuleId,
    pub rule_name: String,
    pub kind: ViolationKind,
    pub severity: ViolationSeverity,
    pub message: String,
    pub occurred_at: Timestamp,
    /// Optional numeric breach value (e.g. drawdown amount).
    pub breach_value: Option<Money>,
    /// Optional numeric threshold value (e.g. allowed drawdown).
    pub threshold_value: Option<Money>,
    /// Optional utilization ratio (breach / threshold).
    pub utilization: Option<Decimal>,
}

impl Violation {
    /// Constructs a new violation record.
    pub fn new(
        account_id: AccountId,
        rule_id: RuleId,
        rule_name: impl Into<String>,
        kind: ViolationKind,
        severity: ViolationSeverity,
        message: impl Into<String>,
        occurred_at: Timestamp,
    ) -> Self {
        Violation {
            id: ViolationId::new(),
            account_id,
            tenant_id: crate::tenant::TenantId::new(),
            rule_id,
            rule_name: rule_name.into(),
            kind,
            severity,
            message: message.into(),
            occurred_at,
            breach_value: None,
            threshold_value: None,
            utilization: None,
        }
    }

    /// **P1-9 fix**: builder-style setter for tenant id. Called by the
    /// rule registry when constructing a violation, so the violation
    /// inherits the account's tenant.
    pub fn with_tenant(mut self, tenant_id: crate::tenant::TenantId) -> Self {
        self.tenant_id = tenant_id;
        self
    }

    /// Builder-style setter for breach value.
    pub fn with_breach(mut self, breach: Money, threshold: Money) -> Self {
        self.breach_value = Some(breach);
        self.threshold_value = Some(threshold);
        if !threshold.0.is_zero() {
            self.utilization = Some(breach.0 / threshold.0);
        }
        self
    }

    /// Returns true if this violation should terminate the account.
    pub fn is_terminating(&self) -> bool {
        matches!(self.severity, ViolationSeverity::Hard | ViolationSeverity::Liquidate)
    }

    /// Validates the violation is internally consistent.
    pub fn validate(&self) -> Result<(), Error> {
        if self.rule_name.is_empty() {
            return Err(Error::InvalidState("violation rule_name cannot be empty".into()));
        }
        if let Some(util) = self.utilization {
            if util < dec!(0) {
                return Err(Error::InvalidState("violation utilization cannot be negative".into()));
            }
        }
        Ok(())
    }
}
