//! Challenge plan definitions.
//!
//! A [`ChallengePlan`] is the immutable specification of a prop firm's
//! evaluation program: profit targets, drawdown limits, time limits, and
//! rule toggles. Plans are versioned (via [`ChallengeId`]) so historical
//! evaluations can be reproduced even after the firm changes its rule set.

use crate::core::ids::{ChallengeId, RuleId};
use crate::core::types::{Money, Pct, Timestamp, dec};
use crate::core::Error;

/// Phase of the challenge lifecycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ChallengePhase {
    Phase1,
    Phase2,
    Funded,
}

impl std::fmt::Display for ChallengePhase {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ChallengePhase::Phase1 => write!(f, "phase1"),
            ChallengePhase::Phase2 => write!(f, "phase2"),
            ChallengePhase::Funded => write!(f, "funded"),
        }
    }
}

/// Metadata about the plan (firm name, version, currency).
#[derive(Debug, Clone)]
pub struct PlanMeta {
    pub firm_name: String,
    pub program_name: String,
    pub version: String,
    pub currency: String,
    pub description: String,
}

impl Default for PlanMeta {
    fn default() -> Self {
        PlanMeta {
            firm_name: "Generic Prop Firm".into(),
            program_name: "Standard Evaluation".into(),
            version: "1.0.0".into(),
            currency: "USD".into(),
            description: String::new(),
        }
    }
}

/// The full challenge plan.
#[derive(Debug, Clone)]
pub struct ChallengePlan {
    pub id: ChallengeId,
    pub phase: ChallengePhase,
    pub meta: PlanMeta,

    /// Initial (deposit) balance.
    pub initial_balance_money: Money,
    /// Profit target as a percentage of initial balance (e.g. 0.08 = 8%).
    pub profit_target_pct: Pct,
    /// Maximum daily drawdown as a percentage (e.g. 0.05 = 5%).
    pub max_daily_drawdown_pct: Pct,
    /// Maximum total drawdown as a percentage (e.g. 0.10 = 10%).
    pub max_total_drawdown_pct: Pct,
    /// Whether drawdown is computed on balance (true) or equity (false).
    pub drawdown_on_balance: bool,
    /// Whether trailing drawdown is enabled.
    pub trailing_drawdown_enabled: bool,
    /// Trailing drawdown as a percentage of peak equity.
    pub trailing_drawdown_pct: Pct,
    /// Minimum number of distinct trading days required.
    pub min_trading_days: u32,
    /// Optional time limit (in days) to complete the phase.
    pub time_limit_days: Option<u32>,
    /// Max position size per order (in lots). None = no limit.
    pub max_position_lots: Option<rust_decimal::Decimal>,
    /// Max total lot exposure (sum of all open lots). None = no limit.
    pub max_total_lots: Option<rust_decimal::Decimal>,
    /// Max simultaneously open positions. None = no limit.
    pub max_open_positions: Option<u32>,
    /// Max trades per day. None = no limit.
    pub max_daily_trades: Option<u32>,
    /// Whether news trading is allowed.
    pub news_trading_allowed: bool,
    /// Whether holding positions overnight is allowed.
    pub overnight_holding_allowed: bool,
    /// Whether holding positions over the weekend is allowed.
    pub weekend_holding_allowed: bool,
    /// Whether hedging is allowed.
    pub hedging_allowed: bool,
    /// Whether grid / martingale strategies are allowed.
    pub grid_trading_allowed: bool,
    /// Whether SL is required on every order.
    pub require_stop_loss: bool,
    /// Whether TP is required on every order.
    pub require_take_profit: bool,
    /// Consistency rule: largest single-day profit cannot exceed this % of total profit.
    pub consistency_pct: Option<Pct>,
    /// Cooldown between trades (in seconds). 0 = no cooldown.
    pub cooldown_seconds: u64,
    /// Whether copy trading is allowed.
    pub copy_trading_allowed: bool,
    /// Whether the plan is refundable.
    pub refundable: bool,
    /// Maximum account leverage (e.g. 1:100).
    pub leverage: u32,
    /// Allowed trading hours (server time) as (start_hour, end_hour), 24h format.
    pub trading_hours: Option<(u8, u8)>,
    /// Effective timestamp of the plan.
    pub effective_at: Timestamp,
}

impl ChallengePlan {
    /// Returns the initial balance as a [`Money`] value.
    pub fn initial_balance(&self) -> Money {
        self.initial_balance_money
    }

    /// Returns the profit target as a percentage.
    pub fn profit_target(&self) -> Pct {
        self.profit_target_pct
    }

    /// Validates the plan for internal consistency. Returns an error if any
    /// constraint is violated.
    pub fn validate(&self) -> Result<(), Error> {
        if self.initial_balance_money.0 <= dec!(0) {
            return Err(Error::InvalidConfig("initial_balance must be positive".into()));
        }
        if self.profit_target_pct.0 < dec!(0) {
            return Err(Error::InvalidConfig("profit_target_pct must be non-negative".into()));
        }
        if self.max_daily_drawdown_pct.0 < dec!(0) || self.max_daily_drawdown_pct.0 > dec!(1) {
            return Err(Error::InvalidConfig("max_daily_drawdown_pct must be in [0, 1]".into()));
        }
        if self.max_total_drawdown_pct.0 < dec!(0) || self.max_total_drawdown_pct.0 > dec!(1) {
            return Err(Error::InvalidConfig("max_total_drawdown_pct must be in [0, 1]".into()));
        }
        if let Some(c) = self.consistency_pct {
            if c.0 < dec!(0) || c.0 > dec!(1) {
                return Err(Error::InvalidConfig("consistency_pct must be in [0, 1]".into()));
            }
        }
        if let Some((s, e)) = self.trading_hours {
            if s > 24 || e > 24 {
                return Err(Error::InvalidConfig("trading_hours must be in [0, 24]".into()));
            }
        }
        Ok(())
    }

    /// Builder-style setter for initial balance.
    pub fn with_balance(mut self, balance: Money) -> Self {
        self.initial_balance_money = balance;
        self
    }

    /// Builder-style setter for phase.
    pub fn with_phase(mut self, phase: ChallengePhase) -> Self {
        self.phase = phase;
        self
    }

    /// Builder-style setter for profit target.
    pub fn with_profit_target(mut self, pct: Pct) -> Self {
        self.profit_target_pct = pct;
        self
    }

    /// Builder-style setter for daily drawdown.
    pub fn with_daily_dd(mut self, pct: Pct) -> Self {
        self.max_daily_drawdown_pct = pct;
        self
    }

    /// Builder-style setter for total drawdown.
    pub fn with_total_dd(mut self, pct: Pct) -> Self {
        self.max_total_drawdown_pct = pct;
        self
    }

    /// Builder-style setter for min trading days.
    pub fn with_min_days(mut self, days: u32) -> Self {
        self.min_trading_days = days;
        self
    }

    /// Builder-style setter for time limit.
    pub fn with_time_limit_days(mut self, days: u32) -> Self {
        self.time_limit_days = Some(days);
        self
    }

    /// Builder-style setter for trailing drawdown.
    pub fn with_trailing_dd(mut self, pct: Pct) -> Self {
        self.trailing_drawdown_enabled = true;
        self.trailing_drawdown_pct = pct;
        self
    }

    /// Builder-style setter for consistency.
    pub fn with_consistency(mut self, pct: Pct) -> Self {
        self.consistency_pct = Some(pct);
        self
    }

    /// Returns the rule id for a named rule (stable across runs).
    pub fn rule_id(name: &str) -> RuleId {
        RuleId::named(name)
    }
}

impl Default for ChallengePlan {
    fn default() -> Self {
        ChallengePlan {
            id: ChallengeId::new(),
            phase: ChallengePhase::Phase1,
            meta: PlanMeta::default(),
            initial_balance_money: Money(dec!(10_000)),
            profit_target_pct: Pct(dec!(0.08)),
            max_daily_drawdown_pct: Pct(dec!(0.05)),
            max_total_drawdown_pct: Pct(dec!(0.10)),
            drawdown_on_balance: false,
            trailing_drawdown_enabled: false,
            trailing_drawdown_pct: Pct::ZERO,
            min_trading_days: 3,
            time_limit_days: Some(30),
            max_position_lots: Some(dec!(5)),
            max_total_lots: Some(dec!(30)),
            max_open_positions: Some(20),
            max_daily_trades: Some(50),
            news_trading_allowed: false,
            overnight_holding_allowed: true,
            weekend_holding_allowed: false,
            hedging_allowed: true,
            grid_trading_allowed: true,
            require_stop_loss: false,
            require_take_profit: false,
            consistency_pct: Some(Pct(dec!(0.50))),
            cooldown_seconds: 0,
            copy_trading_allowed: false,
            refundable: true,
            leverage: 100,
            trading_hours: None,
            effective_at: chrono::Utc::now(),
        }
    }
}
