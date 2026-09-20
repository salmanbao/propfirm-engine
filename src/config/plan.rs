//! Challenge plan definitions.
//!
//! A [`ChallengePlan`] is the immutable specification of a prop firm's
//! evaluation program: profit targets, drawdown limits, time limits, and
//! rule toggles. Plans are versioned (via [`ChallengeId`]) so historical
//! evaluations can be reproduced even after the firm changes its rule set.

use crate::core::ids::{ChallengeId, RuleId};
use crate::core::types::{dec, Money, Pct, Timestamp};
use crate::core::Error;
use chrono_tz::Tz;

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

/// Reference point used to compute the maximum *total* (lifetime) drawdown
/// of an account. This maps directly to the binding spec's
/// `mode: static | trailing | eod_trailing` field in the rule-pack schema.
///
/// - [`LossReference::Static`]: drawdown = `initial_balance - current`
///   (the floor never moves; if your $100k account drops below $90k at
///   any time, you've breached a 10% static limit, even if you grew to
///   $105k first and pulled back to $95k).
/// - [`LossReference::Trailing`]: drawdown = `peak_balance - current`
///   (the floor floats up as the account grows; this is what
///   [`TrailingDrawdownRule`](crate::rules::evaluators::trailing_drawdown::TrailingDrawdownRule)
///   already implements).
/// - [`LossReference::EodTrailing`]: **P1.6 fix** — floor = prior day's
///   closing balance − pct, reset once per day at the trading-session
///   rollover. This is the mode used by FTMO 1-Step and several 2026
///   programs — distinct from continuous trailing (which floats
///   intraday) and from static (which never moves).
///
/// **Important**: [`MaxDrawdownRule`](crate::rules::evaluators::max_drawdown::MaxDrawdownRule)
/// reads this field to decide which reference to use. Plans that do not
/// set it explicitly default to `Trailing` for backward compatibility —
/// but every preset in [`crate::config::presets`] sets it deliberately.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum LossReference {
    /// Drawdown is measured from the initial deposit; the floor never
    /// moves. This is the mode most V1 phase-1/phase-2 challenges use for
    /// the *max total loss* rule (as opposed to a trailing max loss).
    Static,
    /// Drawdown is measured from the high-water mark (peak) of
    /// balance/equity. The floor floats up as the account grows. This is
    /// the default for backward compatibility with code written before
    /// the distinction was introduced.
    #[default]
    Trailing,
    /// **P1.6 fix**: End-of-day-reset trailing. Floor = prior trading
    /// day's closing balance − pct, recomputed once per day at the
    /// trading-session rollover (in the plan's configured timezone —
    /// see P1.5). Distinct from continuous `Trailing` because the floor
    /// does not float intraday; intraday drawdown against the
    /// session-opening floor is allowed up to the daily DD limit.
    ///
    /// Used by FTMO 1-Step, `FundedNext`, and several 2026 programs.
    EodTrailing,
}

impl std::fmt::Display for LossReference {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LossReference::Static => write!(f, "static"),
            LossReference::Trailing => write!(f, "trailing"),
            LossReference::EodTrailing => write!(f, "eod_trailing"),
        }
    }
}

impl std::str::FromStr for LossReference {
    type Err = crate::core::Error;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_ascii_lowercase().as_str() {
            "static" => Ok(LossReference::Static),
            "trailing" => Ok(LossReference::Trailing),
            "eod_trailing" | "eodtrailing" | "eod-trailing" => Ok(LossReference::EodTrailing),
            other => Err(crate::core::Error::invalid_config(format!(
                "unknown loss reference '{other}' (expected 'static', 'trailing', or 'eod_trailing')"
            ))),
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
    /// **Reference point** used by [`MaxDrawdownRule`](crate::rules::evaluators::max_drawdown::MaxDrawdownRule)
    /// to compute the maximum total drawdown: `Static` measures from
    /// `initial_balance` (the floor never moves), `Trailing` measures
    /// from `peak_balance` (the floor floats up). Maps to the binding
    /// spec's `mode: static | trailing` rule-pack field.
    pub max_loss_reference: LossReference,
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
    /// Allowed trading hours (server time) as (`start_hour`, `end_hour`), 24h format.
    pub trading_hours: Option<(u8, u8)>,
    /// **P1.1 fix**: server-time timezone for day-reset / EOD trailing
    /// calculations. The offset is applied to `chrono::Utc::now()` to
    /// determine the plan's "server day" boundary (midnight in this
    /// timezone). When `None`, UTC is assumed (existing behaviour).
    pub timezone: Option<Tz>,

    /// **P1.1 fix**: hour-of-day (0..23) when the server day resets.
    /// Midnight in the plan's timezone, i.e. the boundary after which
    /// `trading_day_index`, `active_trading_days`, `day_start_balance`,
    /// `largest_day_profit`, and `largest_day_loss` all reset.
    /// Default 0 (= midnight UTC) when `timezone` is `None`.
    pub day_reset_time: u8,
    /// Effective timestamp of the plan.
    pub effective_at: Timestamp,
    /// (e.g. 0.02 = "no single closed trade may lose more than 2% of
    /// balance" — Topstep-style). `None` = rule disabled. Previously this
    /// rule was enabled by default for every account via the default
    /// registry, liquidating accounts on programs that have no such rule.
    pub per_trade_max_loss_pct: Option<Pct>,
    /// **P0.1 fix**: absolute per-trade max-loss limit in money
    /// (alternative to `per_trade_max_loss_pct`; the tighter of the two
    /// applies when both are set). `None` = not configured.
    pub per_trade_max_loss_money: Option<Money>,
    /// **P0.1 fix**: whether the HFT / scalping ban is enabled for this
    /// plan (round-trip time / closes-per-minute detection). `false` =
    /// rule disabled. Only firms that publish an HFT/scalping ban set
    /// this to `true` in their preset.
    pub hft_ban_enabled: bool,
    /// **P0.1 fix**: minimum round-trip time in seconds for the HFT ban
    /// (open + close faster than this is scalping). Used only when
    /// `hft_ban_enabled` is `true`.
    pub hft_min_round_trip_seconds: u64,
    /// **P1.5 fix**: whether the plan is "unlimited time + inactivity
    /// termination" (several 2026 programs). When `Some(n)`, the account
    /// is terminated after `n` consecutive days without a trade.
    pub inactivity_days: Option<u32>,
}

impl ChallengePlan {
    /// Returns the initial balance as a [`Money`] value.
    #[must_use]
    pub fn initial_balance(&self) -> Money {
        self.initial_balance_money
    }

    /// Returns the profit target as a percentage.
    #[must_use]
    pub fn profit_target(&self) -> Pct {
        self.profit_target_pct
    }

    /// Validates the plan for internal consistency. Returns an error if any
    /// constraint is violated.
    pub fn validate(&self) -> Result<(), Error> {
        if self.initial_balance_money.0 <= dec!(0) {
            return Err(Error::InvalidConfig(
                "initial_balance must be positive".into(),
            ));
        }
        if self.profit_target_pct.0 < dec!(0) {
            return Err(Error::InvalidConfig(
                "profit_target_pct must be non-negative".into(),
            ));
        }
        if self.max_daily_drawdown_pct.0 < dec!(0) || self.max_daily_drawdown_pct.0 > dec!(1) {
            return Err(Error::InvalidConfig(
                "max_daily_drawdown_pct must be in [0, 1]".into(),
            ));
        }
        if self.max_total_drawdown_pct.0 < dec!(0) || self.max_total_drawdown_pct.0 > dec!(1) {
            return Err(Error::InvalidConfig(
                "max_total_drawdown_pct must be in [0, 1]".into(),
            ));
        }
        if let Some(c) = self.consistency_pct {
            if c.0 < dec!(0) || c.0 > dec!(1) {
                return Err(Error::InvalidConfig(
                    "consistency_pct must be in [0, 1]".into(),
                ));
            }
        }
        if let Some((s, e)) = self.trading_hours {
            if s > 24 || e > 24 {
                return Err(Error::InvalidConfig(
                    "trading_hours must be in [0, 24]".into(),
                ));
            }
        }
        Ok(())
    }

    /// Builder-style setter for initial balance.
    #[must_use]
    pub fn with_balance(mut self, balance: Money) -> Self {
        self.initial_balance_money = balance;
        self
    }

    /// Builder-style setter for phase.
    #[must_use]
    pub fn with_phase(mut self, phase: ChallengePhase) -> Self {
        self.phase = phase;
        self
    }

    /// Builder-style setter for profit target.
    #[must_use]
    pub fn with_profit_target(mut self, pct: Pct) -> Self {
        self.profit_target_pct = pct;
        self
    }

    /// Builder-style setter for daily drawdown.
    #[must_use]
    pub fn with_daily_dd(mut self, pct: Pct) -> Self {
        self.max_daily_drawdown_pct = pct;
        self
    }

    /// Builder-style setter for total drawdown.
    #[must_use]
    pub fn with_total_dd(mut self, pct: Pct) -> Self {
        self.max_total_drawdown_pct = pct;
        self
    }

    /// Builder-style setter for the maximum-loss reference mode
    /// (static vs trailing). See [`LossReference`] for semantics.
    #[must_use]
    pub fn with_loss_reference(mut self, mode: LossReference) -> Self {
        self.max_loss_reference = mode;
        self
    }

    /// Builder-style setter for min trading days.
    #[must_use]
    pub fn with_min_days(mut self, days: u32) -> Self {
        self.min_trading_days = days;
        self
    }

    /// Builder-style setter for time limit.
    #[must_use]
    pub fn with_time_limit_days(mut self, days: u32) -> Self {
        self.time_limit_days = Some(days);
        self
    }

    /// Builder-style setter for trailing drawdown.
    #[must_use]
    pub fn with_trailing_dd(mut self, pct: Pct) -> Self {
        self.trailing_drawdown_enabled = true;
        self.trailing_drawdown_pct = pct;
        self
    }

    /// Builder-style setter for consistency.
    #[must_use]
    pub fn with_consistency(mut self, pct: Pct) -> Self {
        self.consistency_pct = Some(pct);
        self
    }

    /// Builder-style setter for the per-trade max-loss rule (P0.1).
    #[must_use]
    pub fn with_per_trade_max_loss_pct(mut self, pct: Pct) -> Self {
        self.per_trade_max_loss_pct = Some(pct);
        self
    }

    /// Builder-style setter for the HFT/scalping ban (P0.1).
    #[must_use]
    pub fn with_hft_ban(mut self, min_round_trip_seconds: u64) -> Self {
        self.hft_ban_enabled = true;
        self.hft_min_round_trip_seconds = min_round_trip_seconds;
        self
    }

    /// Builder-style setter for inactivity termination (P1.5).
    #[must_use]
    pub fn with_inactivity_days(mut self, days: u32) -> Self {
        self.inactivity_days = Some(days);
        self
    }

    /// Returns the rule id for a named rule (stable across runs).
    #[must_use]
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
            max_loss_reference: LossReference::Trailing,
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
            // P1.1: timezone defaults to UTC (None), day reset at midnight UTC
            timezone: None,
            day_reset_time: 0,
            effective_at: chrono::Utc::now(),
            // P0.1: opt-in rules default to OFF. A plan must explicitly
            // enable per-trade max loss / the HFT ban (or bind a pack
            // entry that does), otherwise the rule must not run.
            per_trade_max_loss_pct: None,
            per_trade_max_loss_money: None,
            hft_ban_enabled: false,
            hft_min_round_trip_seconds: 60,
            inactivity_days: None,
        }
    }
}
