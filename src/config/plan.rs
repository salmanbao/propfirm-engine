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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
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
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, Default, serde::Serialize, serde::Deserialize,
)]
pub enum LossReference {
    /// Drawdown is measured from the initial deposit; the floor never
    /// moves. This is the mode most V1 phase-1/phase-2 challenges use for
    /// the *max total loss* rule (as opposed to a trailing max loss).
    Static,
    /// Drawdown is measured from the high-water mark (peak) of
    /// balance/equity. The floor floats up as the account grows. This is
    /// the default for backward compatibility with code written before
    /// the distinction was introduced.
    ///
    /// **Note**: this variant uses `peak_balance` (closed-balance peak).
    /// For the dataset's `intraday_trail` mechanism (which uses
    /// unrealised-equity peaks), use [`Self::IntradayTrail`].
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
    /// **P1#4 fix**: Intraday-trailing max drawdown. Floor follows the
    /// highest **unrealised equity** peak (`Account::peak_equity`), not
    /// the closed-balance peak. Used by Apex's Intraday Trail variant,
    /// FundingPips Zero, and Breakout 2-Step — the dataset's harshest
    /// max-drawdown mechanism. An open position that runs into profit
    /// and back out can breach you without a single losing closed trade.
    IntradayTrail,
}

impl std::fmt::Display for LossReference {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LossReference::Static => write!(f, "static"),
            LossReference::Trailing => write!(f, "trailing"),
            LossReference::EodTrailing => write!(f, "eod_trailing"),
            LossReference::IntradayTrail => write!(f, "intraday_trail"),
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
            "intraday_trail" | "intradaytrail" | "intraday-trail" => {
                Ok(LossReference::IntradayTrail)
            }
            other => Err(crate::core::Error::invalid_config(format!(
                "unknown loss reference '{other}' (expected 'static', 'trailing', 'eod_trailing', or 'intraday_trail')"
            ))),
        }
    }
}

/// **P0#1 fix**: Type of daily loss limit. Maps 1:1 to the
/// `daily_loss.type` field in the propfirm-rules-dataset schema.
///
/// The dataset proves four real variants are in use by verified firms:
///
/// - [`Self::None`] — TopStep, Apex Intraday, FundedNext Stellar Instant.
/// - [`Self::PctInitial`] — FTMO, FundedNext, FundingPips, The5%ers,
///   Bitfunded, Apex EOD. Fixed dollar amount derived from the
///   **initial** balance; the dollar room does not grow with the
///   account, even if the account is up.
/// - [`Self::PctPriorDay`] — FundingPips (5 plans), The5%ers (both),
///   Breakout (4 plans), HyroTrader Swing. Percentage of the balance
///   at the daily reset; the room moves with the account.
/// - [`Self::TrailingIntradayHigh`] — HyroTrader Standard. The limit
///   itself trails the intraday peak equity. Harshest daily form;
///   requires tracking `Account::intraday_peak_equity`.
///
/// Before this enum existed, the engine silently used `PctPriorDay`
/// for every plan (because `DailyDrawdownRule` computed
/// `plan_pct × day_start_balance`). That was wrong for 6 of 9 verified
/// firms. Preserved as the default for backward compatibility — every
/// preset that needs a different variant must now set it explicitly.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, Default, serde::Serialize, serde::Deserialize,
)]
#[cfg_attr(feature = "serialization", serde(rename_all = "snake_case"))]
pub enum DailyLossType {
    /// No daily loss limit. The `max_daily_drawdown_pct` field is
    /// ignored when this is set.
    None,
    /// Fixed dollar amount derived from the **initial** balance.
    /// `limit = plan_pct × initial_balance`. The floor (day_start −
    /// limit) moves with the day_start, but the room itself does not
    /// grow with the account.
    PctInitial,
    /// Percentage of the balance/equity at the daily reset.
    /// `limit = plan_pct × day_start`. The room moves with the account.
    /// This is the engine's historical behavior and the default for
    /// backward compatibility.
    #[default]
    PctPriorDay,
    /// The limit itself trails the intraday peak equity.
    /// `limit = plan_pct × intraday_peak_equity`. As the intraday peak
    /// goes up, the floor goes up. An open position that runs into
    /// profit and back out can breach you with no closed losing trade —
    /// the harshest daily-loss form, used by HyroTrader Standard.
    /// Requires `Account::intraday_peak_equity` (reset at day rollover).
    TrailingIntradayHigh,
}

impl std::fmt::Display for DailyLossType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DailyLossType::None => write!(f, "none"),
            DailyLossType::PctInitial => write!(f, "pct_initial"),
            DailyLossType::PctPriorDay => write!(f, "pct_prior_day"),
            DailyLossType::TrailingIntradayHigh => write!(f, "trailing_intraday_high"),
        }
    }
}

impl std::str::FromStr for DailyLossType {
    type Err = crate::core::Error;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_ascii_lowercase().as_str() {
            "none" => Ok(DailyLossType::None),
            "pct_initial" | "pctinitial" => Ok(DailyLossType::PctInitial),
            "pct_prior_day" | "pctpriorday" | "pct_prior" => Ok(DailyLossType::PctPriorDay),
            "trailing_intraday_high" | "trailingintradayhigh" => {
                Ok(DailyLossType::TrailingIntradayHigh)
            }
            other => Err(crate::core::Error::invalid_config(format!(
                "unknown daily loss type '{other}' (expected 'none', 'pct_initial', 'pct_prior_day', or 'trailing_intraday_high')"
            ))),
        }
    }
}

/// **P0#2 fix**: Type of consistency rule. Maps 1:1 to the
/// `consistency.type` field in the propfirm-rules-dataset schema.
///
/// The dataset proves two real denominator variants are in use by
/// verified firms — they produce materially different verdicts:
///
/// - [`Self::BestDayPctOfTotal`] — denominator = `total_realized_pnl`
///   (all days, including losing days). Used by HyroTrader (all 4
///   plans, after the 2026-08-29 correction in the dataset).
/// - [`Self::BestDayPctOfPositiveDays`] — denominator =
///   `sum_positive_days_profit` (only winning days). Stricter than it
///   first looks — losing days don't dilute the denominator, so the
///   cap is smaller for the same headline %. Used by FTMO 1-Step,
///   FundingPips Zero, The5%ers 1-Step.
///
/// Before this enum existed, the engine always used
/// `BestDayPctOfPositiveDays`. That was wrong for HyroTrader.
/// Preserved as the default for backward compatibility — every
/// preset that needs a different variant must now set it explicitly.
///
/// **Note**: the dataset explicitly warns there is **no per-trade
/// consistency type**. A per-trade rule is frequently attributed to
/// HyroTrader and is incorrect; it's per-day. Do not add a per-trade
/// variant.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, Default, serde::Serialize, serde::Deserialize,
)]
#[cfg_attr(feature = "serialization", serde(rename_all = "snake_case"))]
pub enum ConsistencyType {
    /// No consistency rule. The `consistency_pct` field is ignored.
    None,
    /// No day above X% of **total** realized P&L (denominator =
    /// `total_realized_pnl`, including losing days). HyroTrader.
    BestDayPctOfTotal,
    /// No day above X% of profit from **winning** days only (denominator
    /// = `sum_positive_days_profit`). Stricter than
    /// [`Self::BestDayPctOfTotal`]. FTMO 1-Step, FundingPips Zero,
    /// The5%ers 1-Step.
    #[default]
    BestDayPctOfPositiveDays,
}

impl std::fmt::Display for ConsistencyType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ConsistencyType::None => write!(f, "none"),
            ConsistencyType::BestDayPctOfTotal => write!(f, "best_day_pct_of_total"),
            ConsistencyType::BestDayPctOfPositiveDays => {
                write!(f, "best_day_pct_of_positive_days")
            }
        }
    }
}

impl std::str::FromStr for ConsistencyType {
    type Err = crate::core::Error;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_ascii_lowercase().as_str() {
            "none" => Ok(ConsistencyType::None),
            "best_day_pct_of_total" => Ok(ConsistencyType::BestDayPctOfTotal),
            "best_day_pct_of_positive_days" => Ok(ConsistencyType::BestDayPctOfPositiveDays),
            other => Err(crate::core::Error::invalid_config(format!(
                "unknown consistency type '{other}' (expected 'none', 'best_day_pct_of_total', or 'best_day_pct_of_positive_days')"
            ))),
        }
    }
}

/// Metadata about the plan (firm name, version, currency).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
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
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
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
    #[serde(
        serialize_with = "serialize_timezone",
        deserialize_with = "deserialize_timezone"
    )]
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
    /// **§D.2 fix**: enrolment-fee refund amount for refundable plans,
    /// added to the trader's first payout on top of the profit split.
    /// `Money::ZERO` when the plan is not refundable. This is the
    /// consumer of the previously-dead `refundable` field.
    pub refund_fee_amount: Money,
    /// **P0#1 fix**: Type of daily loss limit. Defaults to
    /// [`DailyLossType::PctPriorDay`] (the engine's historical
    /// behavior) for backward compatibility — every preset that
    /// needs a different variant (`PctInitial`, `TrailingIntradayHigh`,
    /// `None`) must set it explicitly. See [`DailyLossType`] for the
    /// full taxonomy and which verified firms use which variant.
    pub daily_loss_type: DailyLossType,
    /// **P1#5 fix**: When `true`, a daily-loss breach is a soft
    /// warning (severity = `Warning`) rather than a hard breach
    /// (severity = `Liquidate`). Used by Apex EOD Trail (the only
    /// firm in the dataset whose daily loss is `soft: true`).
    /// Default `false` — every other firm treats daily loss as hard.
    pub daily_loss_soft: bool,
    /// **P0#2 fix**: Type of consistency rule. Defaults to
    /// [`ConsistencyType::BestDayPctOfPositiveDays`] (the engine's
    /// historical behavior) for backward compatibility — every preset
    /// that needs a different variant (`BestDayPctOfTotal`, `None`)
    /// must set it explicitly. See [`ConsistencyType`] for the full
    /// taxonomy and which verified firms use which variant.
    pub consistency_type: ConsistencyType,
    /// **P1#3 fix**: When `true` and `max_loss_reference` is
    /// [`LossReference::EodTrailing`], the trailing floor freezes at
    /// the starting balance once it would otherwise trail past it.
    /// After the lock engages, the worst case is breakeven rather
    /// than breach. Used by TopStep (all 3 Combines), Breakout 2-Step,
    /// FundedNext Stellar Instant, FundingPips Zero.
    pub eod_trail_locks_at_start: bool,
    /// **P1#6 fix**: Minimum number of **profitable** trading days
    /// required to pass the phase. Distinct from `min_trading_days`
    /// (which counts any day with at least one trade, regardless of
    /// P&L). `None` = rule disabled. Used by FundingPips Zero (7).
    pub min_profitable_days: Option<u32>,
    /// **§D.2 fix**: payout policy — minimum payout, cycle, scaling
    /// tiers. `None` means the tenant has not configured payouts (the
    /// payout endpoints are inert for this plan).
    pub payout_config: Option<crate::payout::PayoutConfig>,
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

    /// Returns the start of the current trading day based on the plan's
    /// timezone and day_reset_time. When timezone is None, UTC is assumed.
    #[must_use]
    pub fn trading_day_start(&self, now: Timestamp) -> Timestamp {
        let reset_hour = self.day_reset_time as u32;
        match self.timezone {
            Some(tz) => {
                let local_now = now.with_timezone(&tz);
                let reset_today = local_now
                    .date_naive()
                    .and_hms_opt(reset_hour, 0, 0)
                    .expect("valid hour")
                    .and_local_timezone(tz)
                    .unwrap();
                if local_now >= reset_today {
                    reset_today.with_timezone(&chrono::Utc)
                } else {
                    (reset_today - chrono::Duration::days(1)).with_timezone(&chrono::Utc)
                }
            }
            None => {
                let reset_today = now
                    .date_naive()
                    .and_hms_opt(reset_hour, 0, 0)
                    .expect("valid hour")
                    .and_utc();
                if now >= reset_today {
                    reset_today
                } else {
                    reset_today - chrono::Duration::days(1)
                }
            }
        }
    }

    /// Returns the start of the *next* trading day after `current_start`,
    /// using the plan's timezone and day_reset_time. This is the calendar-
    /// aware replacement for adding a fixed 24-hour duration, so it stays
    /// correct across DST transitions.
    #[must_use]
    pub fn next_trading_day_start(&self, current_start: Timestamp) -> Timestamp {
        let reset_hour = self.day_reset_time as u32;
        match self.timezone {
            Some(tz) => {
                let local_current = current_start.with_timezone(&tz);
                let next_day = local_current
                    .date_naive()
                    .succ_opt()
                    .expect("valid next day")
                    .and_hms_opt(reset_hour, 0, 0)
                    .expect("valid hour")
                    .and_local_timezone(tz)
                    .unwrap();
                next_day.with_timezone(&chrono::Utc)
            }
            None => current_start
                .date_naive()
                .succ_opt()
                .expect("valid next day")
                .and_hms_opt(reset_hour, 0, 0)
                .expect("valid hour")
                .and_utc(),
        }
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
            refund_fee_amount: Money::ZERO,
            // P0#1: default PctPriorDay preserves the engine's historical
            // behavior — every preset that needs a different variant sets
            // it explicitly. See DailyLossType for the full taxonomy.
            daily_loss_type: DailyLossType::PctPriorDay,
            // P1#5: default false — every verified firm except Apex EOD
            // treats daily loss as a hard breach.
            daily_loss_soft: false,
            // P0#2: default BestDayPctOfPositiveDays preserves the engine's
            // historical behavior — every preset that needs a different
            // variant sets it explicitly.
            consistency_type: ConsistencyType::BestDayPctOfPositiveDays,
            // P1#3: default false — locks_at_start is an opt-in for the
            // 4 firms that use it (TopStep, Breakout 2-Step, FundedNext
            // Stellar Instant, FundingPips Zero).
            eod_trail_locks_at_start: false,
            // P1#6: default None — min_profitable_days is an opt-in for
            // FundingPips Zero (7).
            min_profitable_days: None,
            payout_config: Some(crate::payout::PayoutConfig::default()),
        }
    }
}

fn serialize_timezone<S>(tz: &Option<Tz>, serializer: S) -> Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    match tz {
        Some(tz) => serializer.serialize_str(tz.name()),
        None => serializer.serialize_none(),
    }
}

fn deserialize_timezone<'de, D>(deserializer: D) -> Result<Option<Tz>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value: Option<String> = serde::Deserialize::deserialize(deserializer)?;
    match value {
        Some(name) => {
            let tz = name.parse::<Tz>().map_err(serde::de::Error::custom)?;
            Ok(Some(tz))
        }
        None => Ok(None),
    }
}
