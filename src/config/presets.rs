//! Preset challenge plan factories for popular prop firm styles.
//!
//! These presets are *inspired by* common industry terms but are not
//! endorsed by any specific firm. Use them as starting points; verify
//! current rules with each provider before going live.
//!
//! **P0-1 fix**: every preset now declares `max_loss_reference` explicitly
//! so `MaxDrawdownRule` measures against the right baseline (static for the
//! classic "max total loss" rule, trailing only where the firm actually
//! advertises a trailing max loss).

use crate::config::plan::{
    ChallengePhase, ChallengePlan, ConsistencyType, DailyLossType, LossReference, PlanMeta,
};
use crate::core::ids::ChallengeId;
use crate::core::types::{dec, Money, Pct};
use chrono::Utc;

fn base_plan(firm: &str, program: &str, balance: Money) -> ChallengePlan {
    let p = ChallengePlan {
        id: ChallengeId::new(),
        meta: PlanMeta {
            firm_name: firm.into(),
            program_name: program.into(),
            version: "1.0.0".into(),
            currency: "USD".into(),
            description: format!("{firm} {program} evaluation program"),
        },
        initial_balance_money: balance,
        effective_at: Utc::now(),
        ..ChallengePlan::default()
    };
    p
}

/// FTMO-style Phase 1 plan.
///
/// FTMO's classic "Challenge" phase uses a *static* max total loss (10%
/// of initial balance — the floor is $90k on a $100k account and *never
/// moves*, even if equity grows to $150k first). The daily loss is also
/// static (5% of day-start balance, resets each trading day). The
/// *trailing* max loss is a separate, optional rule FTMO applied to funded
/// accounts — not to phase-1/phase-2 — so we leave `trailing_drawdown_*`
/// disabled here.
///
/// **P1.12 fix**: FTMO's published minimum trading days is **4** (was
/// hardcoded to 3 — drift caught by the deep assessment).
#[must_use]
pub fn ftmo_phase1() -> ChallengePlan {
    let mut p = base_plan("FTMO", "Challenge", Money(dec!(10_000)));
    p.phase = ChallengePhase::Phase1;
    p.profit_target_pct = Pct(dec!(0.10));
    p.max_daily_drawdown_pct = Pct(dec!(0.05));
    p.max_total_drawdown_pct = Pct(dec!(0.10));
    // P0#1: FTMO daily loss is `pct_initial` (fixed dollar buffer
    // derived from the initial balance, doesn't grow with the account).
    // Per the propfirm-rules-dataset: "pct_initial — FTMO, FundedNext,
    // FundingPips, The5%ers, Bitfunded, Apex EOD".
    p.daily_loss_type = DailyLossType::PctInitial;
    // P0-1: FTMO phase-1 uses STATIC max total loss. The floor is the
    // initial balance; growing to $11k and pulling back to $9.5k does NOT
    // breach (you're still above the $9k floor).
    //
    // NOTE: the propfirm-rules-dataset (corrected 2026-08-29) encodes
    // FTMO 2-Step as `eod_trail`. The existing engine's `EodTrailing`
    // uses `day_start_balance` (most recent EOD close), which doesn't
    // capture the "highest end-of-day closed balance" (all-time peak)
    // semantics the dataset describes. Until the engine tracks
    // `peak_eod_balance` separately, we stay on `Static` as the more
    // conservative choice (the dataset's own notes flag the FTMO
    // encoding as conservative). See PROPFIRM_DATASET_GAP_ANALYSIS.md.
    p.max_loss_reference = LossReference::Static;
    p.drawdown_on_balance = false;
    p.min_trading_days = 4; // P1.12: FTMO published spec is 4, not 3.
    p.time_limit_days = Some(30);
    p.news_trading_allowed = true;
    p.overnight_holding_allowed = true;
    p.weekend_holding_allowed = false;
    p.hedging_allowed = true;
    p.grid_trading_allowed = true;
    p.require_stop_loss = true;
    p.consistency_pct = Some(Pct(dec!(0.40)));
    p.leverage = 100;
    p.validate().unwrap();
    p
}

/// FTMO-style Phase 2 plan.
#[must_use]
pub fn ftmo_phase2() -> ChallengePlan {
    let mut p = ftmo_phase1();
    p.phase = ChallengePhase::Phase2;
    p.meta.program_name = "Verification".into();
    p.profit_target_pct = Pct(dec!(0.05));
    p.time_limit_days = Some(60);
    p.validate().unwrap();
    p
}

/// FTMO-style funded plan.
///
/// Funded accounts have no profit target and use a *trailing* max loss
/// (the floor floats up as the account grows — this is what FTMO's
/// published "trailing drawdown" actually refers to).
#[must_use]
pub fn ftmo_funded() -> ChallengePlan {
    let mut p = ftmo_phase2();
    p.phase = ChallengePhase::Funded;
    p.meta.program_name = "Funded".into();
    p.profit_target_pct = Pct::ZERO;
    p.time_limit_days = None;
    // P0-1: funded phase switches to TRAILING max loss.
    p.max_loss_reference = LossReference::Trailing;
    p.trailing_drawdown_enabled = true;
    p.trailing_drawdown_pct = Pct(dec!(0.10));
    p.consistency_pct = Some(Pct(dec!(0.40)));
    p.validate().unwrap();
    p
}

/// MyForexFunds-style Phase 1 (aggressive).
#[must_use]
pub fn myforexfunds_phase1() -> ChallengePlan {
    let mut p = base_plan("MyForexFunds", "Challenge", Money(dec!(50_000)));
    p.phase = ChallengePhase::Phase1;
    p.profit_target_pct = Pct(dec!(0.08));
    p.max_daily_drawdown_pct = Pct(dec!(0.05));
    p.max_total_drawdown_pct = Pct(dec!(0.10));
    // P0-1: MFF classic phase-1 uses STATIC max loss.
    p.max_loss_reference = LossReference::Static;
    p.min_trading_days = 0;
    p.time_limit_days = None;
    p.news_trading_allowed = true;
    p.overnight_holding_allowed = true;
    p.weekend_holding_allowed = true;
    p.hedging_allowed = true;
    p.require_stop_loss = false;
    p.consistency_pct = None;
    p.leverage = 500;
    p.validate().unwrap();
    p
}

/// The Funded Trader-style Phase 1.
#[must_use]
pub fn thefundedtrader_phase1() -> ChallengePlan {
    let mut p = base_plan("The Funded Trader", "Challenge", Money(dec!(100_000)));
    p.phase = ChallengePhase::Phase1;
    p.profit_target_pct = Pct(dec!(0.10));
    p.max_daily_drawdown_pct = Pct(dec!(0.05));
    p.max_total_drawdown_pct = Pct(dec!(0.10));
    // P0-1: TFT classic phase-1 uses STATIC max loss.
    p.max_loss_reference = LossReference::Static;
    p.min_trading_days = 0;
    p.time_limit_days = None;
    p.news_trading_allowed = false;
    p.overnight_holding_allowed = false;
    p.weekend_holding_allowed = false;
    p.hedging_allowed = false;
    p.require_stop_loss = true;
    p.consistency_pct = None;
    p.leverage = 100;
    p.validate().unwrap();
    p
}

/// SurgeTrader-style plan (single phase, profit target only).
#[must_use]
pub fn surgetrader_plan() -> ChallengePlan {
    let mut p = base_plan("SurgeTrader", "One-Step", Money(dec!(25_000)));
    p.phase = ChallengePhase::Phase1;
    p.profit_target_pct = Pct(dec!(0.10));
    p.max_daily_drawdown_pct = Pct(dec!(0.03));
    p.max_total_drawdown_pct = Pct(dec!(0.05));
    // P0-1: SurgeTrader uses STATIC max loss.
    p.max_loss_reference = LossReference::Static;
    p.min_trading_days = 0;
    p.time_limit_days = None;
    p.news_trading_allowed = true;
    p.overnight_holding_allowed = true;
    p.weekend_holding_allowed = true;
    p.hedging_allowed = true;
    p.require_stop_loss = false;
    p.consistency_pct = None;
    p.leverage = 30;
    p.validate().unwrap();
    p
}

/// Custom plan builder.
#[must_use]
pub fn custom(name: &str, balance: Money) -> ChallengePlan {
    base_plan("Custom", name, balance)
}

/// **P1.12 fix**: FTMO-style 1-Step plan (single-phase evaluation).
///
/// As of 2026, FTMO and most major prop firms offer a "1-Step" product:
/// a single evaluation phase with no separate Phase 2 verification. The
/// profit target is typically higher (10%) to compensate for skipping
/// Phase 2, drawdown rules are tighter (often trailing-EOD), and there's
/// no min-trading-days requirement (1-Step is meant to be fast).
///
/// Note: real 1-Step plans use EOD-reset trailing max loss — the engine
/// supports this via `LossReference::EodTrailing`.
#[must_use]
pub fn ftmo_1step() -> ChallengePlan {
    let mut p = base_plan("FTMO", "1-Step Challenge", Money(dec!(10_000)));
    p.phase = ChallengePhase::Phase1;
    p.profit_target_pct = Pct(dec!(0.10));
    // Per the dataset: FTMO 1-Step daily_loss = 3% (pct_initial).
    p.max_daily_drawdown_pct = Pct(dec!(0.03));
    p.max_total_drawdown_pct = Pct(dec!(0.10));
    p.daily_loss_type = DailyLossType::PctInitial;
    // P1.2: FTMO 1-Step uses EOD-reset trailing max loss.
    p.max_loss_reference = LossReference::EodTrailing;
    p.drawdown_on_balance = false;
    p.min_trading_days = 0; // 1-Step has no min days.
    p.time_limit_days = None; // unlimited + inactivity termination.
    p.inactivity_days = Some(30); // P1.5: FTMO 1-Step terminates after 30 days of no trading.
    p.news_trading_allowed = true;
    p.overnight_holding_allowed = true;
    p.weekend_holding_allowed = false;
    p.hedging_allowed = true;
    p.grid_trading_allowed = true;
    p.require_stop_loss = true;
    // Per the dataset: FTMO 1-Step consistency = 50% best_day_pct_of_positive_days.
    p.consistency_pct = Some(Pct(dec!(0.50)));
    p.consistency_type = ConsistencyType::BestDayPctOfPositiveDays;
    p.leverage = 100;
    p.validate().unwrap();
    p
}

/// **P1.12 fix**: Instant Funding plan (no evaluation).
///
/// A growing 2026 product: pay a higher fee upfront and skip the
/// evaluation entirely — start on a funded account with reduced profit
/// split (often 50% → scaling plan → 90%) until a profit target is hit.
/// This preset captures the funded-with-restrictions shape.
#[must_use]
pub fn ftmo_instant_funding() -> ChallengePlan {
    let mut p = base_plan("FTMO", "Instant Funding", Money(dec!(10_000)));
    p.phase = ChallengePhase::Funded;
    p.profit_target_pct = Pct(dec!(0.05)); // lower target — just verify profitability
    p.max_daily_drawdown_pct = Pct(dec!(0.05));
    p.max_total_drawdown_pct = Pct(dec!(0.10));
    p.max_loss_reference = LossReference::Trailing;
    p.trailing_drawdown_enabled = true;
    p.trailing_drawdown_pct = Pct(dec!(0.10));
    p.drawdown_on_balance = false;
    p.min_trading_days = 0;
    p.time_limit_days = None;
    p.news_trading_allowed = true;
    p.overnight_holding_allowed = true;
    p.weekend_holding_allowed = false;
    p.hedging_allowed = true;
    p.grid_trading_allowed = false;
    p.require_stop_loss = true;
    p.consistency_pct = Some(Pct(dec!(0.40)));
    p.leverage = 100;
    p.validate().unwrap();
    p
}

/// **P1.12 fix**: FundedNext-style 1-Step plan (50k account, 8% target).
#[must_use]
pub fn fundednext_1step() -> ChallengePlan {
    let mut p = base_plan("FundedNext", "1-Step", Money(dec!(50_000)));
    p.phase = ChallengePhase::Phase1;
    p.profit_target_pct = Pct(dec!(0.08));
    p.max_daily_drawdown_pct = Pct(dec!(0.05));
    p.max_total_drawdown_pct = Pct(dec!(0.10));
    // P1.2: FundedNext 1-Step uses EOD-reset trailing max loss.
    p.max_loss_reference = LossReference::EodTrailing;
    p.min_trading_days = 0;
    p.time_limit_days = None;
    p.inactivity_days = Some(30); // P1.5: FundedNext terminates after 30 days of no trading.
    p.news_trading_allowed = true;
    p.overnight_holding_allowed = true;
    p.weekend_holding_allowed = true;
    p.hedging_allowed = true;
    p.require_stop_loss = false;
    p.consistency_pct = None;
    p.leverage = 100;
    p.validate().unwrap();
    p
}

/// **P1.12 fix**: Topstep-style futures plan (per-trade max loss, no time limit).
/// an **End-of-Day trailing** account max loss ("trailing threshold") with
/// a **per-trade loss limit** ("Max Loss Per Trade") and have **no daily
/// loss limit** — the daily-DD preset here was wrong and has been removed
/// (`max_daily_drawdown_pct = 0` disables the daily rule).
#[must_use]
pub fn topstep_futures() -> ChallengePlan {
    let mut p = base_plan("Topstep", "Trading Combine", Money(dec!(50_000)));
    p.phase = ChallengePhase::Phase1;
    p.profit_target_pct = Pct(dec!(0.06));
    // P1.5: Topstep has NO daily loss limit — only the EOD-trailing
    // threshold and the per-trade limit. Zero disables the daily rule.
    p.max_daily_drawdown_pct = Pct::ZERO;
    // P0#1: explicitly mark `daily_loss_type = None` for clarity
    // (matches the dataset's `daily_loss.type = "none"` for TopStep).
    p.daily_loss_type = DailyLossType::None;
    // P1.5: the account-level threshold trails the best end-of-day balance.
    p.max_total_drawdown_pct = Pct(dec!(0.10));
    p.max_loss_reference = LossReference::EodTrailing;
    // P1#3: TopStep's drawdown locks at the starting balance once the
    // floor would otherwise trail past it. Per the dataset: "Trails
    // end-of-day, then freezes once the floor reaches the starting
    // balance. TopStep's Combine trails the highest end-of-day balance
    // until the floor would exceed the starting balance, at which point
    // it stops and becomes fixed there."
    p.eod_trail_locks_at_start = true;
    // P0.1: Topstep publishes an explicit per-trade loss limit.
    p.per_trade_max_loss_pct = Some(Pct(dec!(0.02)));
    p.min_trading_days = 5;
    p.time_limit_days = None;
    p.inactivity_days = Some(30);
    p.news_trading_allowed = true;
    p.overnight_holding_allowed = true;
    p.weekend_holding_allowed = true;
    p.hedging_allowed = false;
    p.require_stop_loss = true;
    p.consistency_pct = None;
    p.leverage = 1; // futures: no leverage flag; 1 contract per position
    p.validate().unwrap();
    p
}

// =============================================================================
// P2 preset sweep — encodes the 6 firms missing from the original preset
// list (FundingPips, The5%ers, Breakout, HyroTrader, Bitfunded, Apex) +
// the TopStep 100K / 150K Combine size variants. Rule values are read
// directly from `powerFC/propfirm-rules-dataset` (schema v2.0, last
// exported 2026-09-16) — every plan in this section is a verified-firm
// preset, not an "inspired-by" approximation.
// =============================================================================

/// TopStep 100K Trading Combine — same rules as the 50K Combine, just a
/// different starting balance. Per the dataset: 6% target, no daily loss,
/// 3% EOD-trail max drawdown with `locks_at: start_balance`, 2%
/// per-trade max loss, 5 min trading days, no time limit, 30-day
/// inactivity termination.
#[must_use]
pub fn topstep_100k_combine() -> ChallengePlan {
    let mut p = topstep_futures();
    p.meta.program_name = "100K Trading Combine".into();
    p.initial_balance_money = Money(dec!(100_000));
    // Per the dataset: 100K Combine uses 3% max drawdown (vs 4% on 50K).
    p.max_total_drawdown_pct = Pct(dec!(0.03));
    p.validate().unwrap();
    p
}

/// TopStep 150K Trading Combine — same rules as 100K.
#[must_use]
pub fn topstep_150k_combine() -> ChallengePlan {
    let mut p = topstep_100k_combine();
    p.meta.program_name = "150K Trading Combine".into();
    p.initial_balance_money = Money(dec!(150_000));
    p.validate().unwrap();
    p
}

// -----------------------------------------------------------------------------
// FundingPips (5 plans): 1 Step, 2 Step Standard, 2 Step Flex, 2 Step Pro,
// Zero. Per the dataset: all 5 use `pct_prior_day` daily loss and `static`
// max drawdown. Zero is the most complex plan in the dataset — it
// exercises `intraday_trail` max drawdown + `locks_at: start_balance` +
// `min_profitable_days = 7` + `best_day_pct_of_total` consistency (15%).
// -----------------------------------------------------------------------------

/// FundingPips 1 Step — 10% target, 3% daily loss (pct_prior_day), 6%
/// static max drawdown, 3 min trading days, no consistency rule.
#[must_use]
pub fn fundingpips_1step() -> ChallengePlan {
    let mut p = base_plan("FundingPips", "1 Step", Money(dec!(50_000)));
    p.phase = ChallengePhase::Phase1;
    p.profit_target_pct = Pct(dec!(0.10));
    p.max_daily_drawdown_pct = Pct(dec!(0.03));
    p.max_total_drawdown_pct = Pct(dec!(0.06));
    p.daily_loss_type = DailyLossType::PctPriorDay;
    p.max_loss_reference = LossReference::Static;
    p.min_trading_days = 3;
    p.time_limit_days = None;
    p.consistency_pct = None;
    p.consistency_type = ConsistencyType::None;
    p.leverage = 100;
    p.validate().unwrap();
    p
}

/// FundingPips 2 Step Standard — Phase 1: 8% target; Phase 2: 5% target.
/// Both phases: 5% daily loss (pct_prior_day), 10% static max drawdown,
/// 3 min trading days.
#[must_use]
pub fn fundingpips_2step_standard_phase1() -> ChallengePlan {
    let mut p = base_plan("FundingPips", "2 Step Standard - Phase 1", Money(dec!(50_000)));
    p.phase = ChallengePhase::Phase1;
    p.profit_target_pct = Pct(dec!(0.08));
    p.max_daily_drawdown_pct = Pct(dec!(0.05));
    p.max_total_drawdown_pct = Pct(dec!(0.10));
    p.daily_loss_type = DailyLossType::PctPriorDay;
    p.max_loss_reference = LossReference::Static;
    p.min_trading_days = 3;
    p.consistency_pct = None;
    p.consistency_type = ConsistencyType::None;
    p.leverage = 100;
    p.validate().unwrap();
    p
}

#[must_use]
pub fn fundingpips_2step_standard_phase2() -> ChallengePlan {
    let mut p = fundingpips_2step_standard_phase1();
    p.phase = ChallengePhase::Phase2;
    p.meta.program_name = "2 Step Standard - Phase 2".into();
    p.profit_target_pct = Pct(dec!(0.05));
    p.validate().unwrap();
    p
}

/// FundingPips 2 Step Flex — Phase 1: 10% target; Phase 2: 6% target.
/// Both: 4% daily loss (pct_prior_day), 12% static max drawdown, 0 min days.
#[must_use]
pub fn fundingpips_2step_flex_phase1() -> ChallengePlan {
    let mut p = base_plan("FundingPips", "2 Step Flex - Phase 1", Money(dec!(50_000)));
    p.phase = ChallengePhase::Phase1;
    p.profit_target_pct = Pct(dec!(0.10));
    p.max_daily_drawdown_pct = Pct(dec!(0.04));
    p.max_total_drawdown_pct = Pct(dec!(0.12));
    p.daily_loss_type = DailyLossType::PctPriorDay;
    p.max_loss_reference = LossReference::Static;
    p.min_trading_days = 0;
    p.consistency_pct = None;
    p.consistency_type = ConsistencyType::None;
    p.leverage = 100;
    p.validate().unwrap();
    p
}

#[must_use]
pub fn fundingpips_2step_flex_phase2() -> ChallengePlan {
    let mut p = fundingpips_2step_flex_phase1();
    p.phase = ChallengePhase::Phase2;
    p.meta.program_name = "2 Step Flex - Phase 2".into();
    p.profit_target_pct = Pct(dec!(0.06));
    p.validate().unwrap();
    p
}

/// FundingPips 2 Step Pro — Phase 1 + Phase 2: 6% target, 3% daily loss
/// (pct_prior_day), 6% static max drawdown, 1 min trading day.
#[must_use]
pub fn fundingpips_2step_pro_phase1() -> ChallengePlan {
    let mut p = base_plan("FundingPips", "2 Step Pro - Phase 1", Money(dec!(50_000)));
    p.phase = ChallengePhase::Phase1;
    p.profit_target_pct = Pct(dec!(0.06));
    p.max_daily_drawdown_pct = Pct(dec!(0.03));
    p.max_total_drawdown_pct = Pct(dec!(0.06));
    p.daily_loss_type = DailyLossType::PctPriorDay;
    p.max_loss_reference = LossReference::Static;
    p.min_trading_days = 1;
    p.consistency_pct = None;
    p.consistency_type = ConsistencyType::None;
    p.leverage = 100;
    p.validate().unwrap();
    p
}

#[must_use]
pub fn fundingpips_2step_pro_phase2() -> ChallengePlan {
    let mut p = fundingpips_2step_pro_phase1();
    p.phase = ChallengePhase::Phase2;
    p.meta.program_name = "2 Step Pro - Phase 2".into();
    p.validate().unwrap();
    p
}

/// FundingPips Zero — the most complex plan in the dataset. Exercises
/// every new mechanism variant we shipped: `intraday_trail` max drawdown
/// + `locks_at: start_balance` + `min_profitable_days = 7` +
/// `best_day_pct_of_total` consistency (15%). No profit target (funded
/// stage from day 1). 3% daily loss (pct_prior_day).
#[must_use]
pub fn fundingpips_zero() -> ChallengePlan {
    let mut p = base_plan("FundingPips", "Zero", Money(dec!(50_000)));
    p.phase = ChallengePhase::Funded;
    p.profit_target_pct = Pct::ZERO; // no target - instant-funding product
    p.max_daily_drawdown_pct = Pct(dec!(0.03));
    p.max_total_drawdown_pct = Pct(dec!(0.05));
    p.daily_loss_type = DailyLossType::PctPriorDay;
    // P1#4 + P1#3: intraday_trail + locks_at: start_balance - the
    // harshest max-drawdown mechanism in the dataset.
    p.max_loss_reference = LossReference::IntradayTrail;
    p.eod_trail_locks_at_start = true;
    // P1#6: requires 7 profitable trading days.
    p.min_profitable_days = Some(7);
    // Per the dataset: 15% best_day_pct_of_total (denominator =
    // total_realized_pnl, includes losing days).
    p.consistency_pct = Some(Pct(dec!(0.15)));
    p.consistency_type = ConsistencyType::BestDayPctOfTotal;
    p.min_trading_days = 0;
    p.time_limit_days = None;
    p.leverage = 100;
    p.validate().unwrap();
    p
}

// -----------------------------------------------------------------------------
// The5%ers (2 plans): 1-Step, 2-Step (8/5). Per the dataset: both use
// `pct_prior_day` daily loss and `static` max drawdown. The 1-Step has
// a 50% `best_day_pct_of_total` consistency rule that KEEPS APPLYING on
// the funded stage (rare - most firms drop consistency after eval).
// -----------------------------------------------------------------------------

/// The5%ers 1-Step - 10% target, 3% daily loss (pct_prior_day), 6% static
/// max drawdown, 50% best_day_pct_of_total consistency.
/// Note: the dataset says this rule keeps applying on the funded stage -
/// encoded via `consistency_applies_on_funded_stage = true` (P4 fix).
#[must_use]
pub fn the5ers_1step() -> ChallengePlan {
    let mut p = base_plan("The5%ers", "1-Step", Money(dec!(100_000)));
    p.phase = ChallengePhase::Phase1;
    p.profit_target_pct = Pct(dec!(0.10));
    p.max_daily_drawdown_pct = Pct(dec!(0.03));
    p.max_total_drawdown_pct = Pct(dec!(0.06));
    p.daily_loss_type = DailyLossType::PctPriorDay;
    p.max_loss_reference = LossReference::Static;
    p.min_trading_days = 0;
    p.consistency_pct = Some(Pct(dec!(0.50)));
    p.consistency_type = ConsistencyType::BestDayPctOfTotal;
    // P4: The5%ers' consistency rule keeps applying on the funded stage.
    p.consistency_applies_on_funded_stage = true;
    p.leverage = 100;
    p.validate().unwrap();
    p
}

/// The5%ers 2-Step (8/5) - Phase 1: 8% target; Phase 2: 5% target. Both:
/// 3% daily loss (pct_prior_day), 10% static max drawdown, 1 min trading
/// day, no consistency rule.
#[must_use]
pub fn the5ers_2step_phase1() -> ChallengePlan {
    let mut p = base_plan("The5%ers", "2-Step (8/5) - Phase 1", Money(dec!(100_000)));
    p.phase = ChallengePhase::Phase1;
    p.profit_target_pct = Pct(dec!(0.08));
    p.max_daily_drawdown_pct = Pct(dec!(0.03));
    p.max_total_drawdown_pct = Pct(dec!(0.10));
    p.daily_loss_type = DailyLossType::PctPriorDay;
    p.max_loss_reference = LossReference::Static;
    p.min_trading_days = 1;
    p.consistency_pct = None;
    p.consistency_type = ConsistencyType::None;
    p.leverage = 100;
    p.validate().unwrap();
    p
}

#[must_use]
pub fn the5ers_2step_phase2() -> ChallengePlan {
    let mut p = the5ers_2step_phase1();
    p.phase = ChallengePhase::Phase2;
    p.meta.program_name = "2-Step (8/5) - Phase 2".into();
    p.profit_target_pct = Pct(dec!(0.05));
    p.validate().unwrap();
    p
}

// -----------------------------------------------------------------------------
// Breakout (4 plans): 1-Step Classic, 1-Step Pro, 1-Step Turbo, 2-Step.
// Per the dataset: all 4 use `pct_prior_day` daily loss. The 1-Step plans
// use `static` max drawdown; the 2-Step uses `intraday_trail` with
// `locks_at: start_balance`. Turbo is the dataset's tightest target-to-
// buffer ratio (9% target, 3% static drawdown).
// -----------------------------------------------------------------------------

/// Breakout 1-Step Classic - 10% target, 3% daily loss (pct_prior_day),
/// 6% static max drawdown.
#[must_use]
pub fn breakout_1step_classic() -> ChallengePlan {
    let mut p = base_plan("Breakout", "1-Step Classic", Money(dec!(50_000)));
    p.phase = ChallengePhase::Phase1;
    p.profit_target_pct = Pct(dec!(0.10));
    p.max_daily_drawdown_pct = Pct(dec!(0.03));
    p.max_total_drawdown_pct = Pct(dec!(0.06));
    p.daily_loss_type = DailyLossType::PctPriorDay;
    p.max_loss_reference = LossReference::Static;
    p.consistency_pct = None;
    p.consistency_type = ConsistencyType::None;
    p.leverage = 100;
    p.validate().unwrap();
    p
}

/// Breakout 1-Step Pro - 12% target, 3% daily loss, 5% static max drawdown.
#[must_use]
pub fn breakout_1step_pro() -> ChallengePlan {
    let mut p = base_plan("Breakout", "1-Step Pro", Money(dec!(50_000)));
    p.phase = ChallengePhase::Phase1;
    p.profit_target_pct = Pct(dec!(0.12));
    p.max_daily_drawdown_pct = Pct(dec!(0.03));
    p.max_total_drawdown_pct = Pct(dec!(0.05));
    p.daily_loss_type = DailyLossType::PctPriorDay;
    p.max_loss_reference = LossReference::Static;
    p.consistency_pct = None;
    p.consistency_type = ConsistencyType::None;
    p.leverage = 100;
    p.validate().unwrap();
    p
}

/// Breakout 1-Step Turbo - the dataset's tightest target-to-buffer ratio:
/// 9% target against 3% static drawdown.
#[must_use]
pub fn breakout_1step_turbo() -> ChallengePlan {
    let mut p = base_plan("Breakout", "1-Step Turbo", Money(dec!(50_000)));
    p.phase = ChallengePhase::Phase1;
    p.profit_target_pct = Pct(dec!(0.09));
    p.max_daily_drawdown_pct = Pct(dec!(0.03));
    p.max_total_drawdown_pct = Pct(dec!(0.03));
    p.daily_loss_type = DailyLossType::PctPriorDay;
    p.max_loss_reference = LossReference::Static;
    p.consistency_pct = None;
    p.consistency_type = ConsistencyType::None;
    p.leverage = 100;
    p.validate().unwrap();
    p
}

/// Breakout 2-Step - Phase 1: 5% target; Phase 2: 10% target. Both:
/// 5% daily loss (pct_prior_day), 8% intraday_trail max drawdown with
/// `locks_at: start_balance`. Exercises P1#3 + P1#4 together.
#[must_use]
pub fn breakout_2step_phase1() -> ChallengePlan {
    let mut p = base_plan("Breakout", "2-Step - Phase 1", Money(dec!(50_000)));
    p.phase = ChallengePhase::Phase1;
    p.profit_target_pct = Pct(dec!(0.05));
    p.max_daily_drawdown_pct = Pct(dec!(0.05));
    p.max_total_drawdown_pct = Pct(dec!(0.08));
    p.daily_loss_type = DailyLossType::PctPriorDay;
    p.max_loss_reference = LossReference::IntradayTrail;
    p.eod_trail_locks_at_start = true;
    p.consistency_pct = None;
    p.consistency_type = ConsistencyType::None;
    p.leverage = 100;
    p.validate().unwrap();
    p
}

#[must_use]
pub fn breakout_2step_phase2() -> ChallengePlan {
    let mut p = breakout_2step_phase1();
    p.phase = ChallengePhase::Phase2;
    p.meta.program_name = "2-Step - Phase 2".into();
    p.profit_target_pct = Pct(dec!(0.10));
    p.validate().unwrap();
    p
}

// -----------------------------------------------------------------------------
// HyroTrader (4 plans): 1-Step Standard, 1-Step Swing, 2-Step Standard,
// 2-Step Swing. Per the dataset (post-2026-08-29 correction): all 4 use
// `best_day_pct_of_positive_days` consistency (40%) + `static` max
// drawdown + 4 restriction tags (max_loss_per_trade_3pct, no_martingale,
// no_cross_account_hedging, lowcap_exposure_5pct). Standard plans use
// `trailing_intraday_high` daily loss (harshest daily form); Swing plans
// use `pct_prior_day`. Same plan family, materially different risk.
// -----------------------------------------------------------------------------

/// HyroTrader 1-Step Standard - 10% target, 4% daily loss
/// (trailing_intraday_high), 6% static max drawdown, 40% consistency
/// (best_day_pct_of_positive_days), 5 min trading days. Restrictions:
/// max_loss_per_trade_3pct, no_martingale, no_cross_account_hedging,
/// lowcap_exposure_5pct.
#[must_use]
pub fn hyrotrader_1step_standard() -> ChallengePlan {
    let mut p = base_plan("HyroTrader", "1-Step Standard", Money(dec!(50_000)));
    p.phase = ChallengePhase::Phase1;
    p.profit_target_pct = Pct(dec!(0.10));
    p.max_daily_drawdown_pct = Pct(dec!(0.04));
    p.max_total_drawdown_pct = Pct(dec!(0.06));
    // P0#1: trailing_intraday_high - the harshest daily-loss form.
    p.daily_loss_type = DailyLossType::TrailingIntradayHigh;
    p.max_loss_reference = LossReference::Static;
    p.min_trading_days = 5;
    p.consistency_pct = Some(Pct(dec!(0.40)));
    p.consistency_type = ConsistencyType::BestDayPctOfPositiveDays;
    // Restrictions (P3 - see CrossAccountHedgingRule + LowcapExposureRule).
    p.per_trade_max_loss_pct = Some(Pct(dec!(0.03))); // max_loss_per_trade_3pct
    p.grid_trading_allowed = false; // no_martingale
    p.hedging_allowed = false; // no_cross_account_hedging (single-account)
    p.cross_account_hedging_prohibited = true; // P3: cross-account hedge ban
    p.lowcap_exposure_limit_pct = Some(Pct(dec!(0.05))); // lowcap_exposure_5pct
    p.leverage = 100;
    p.validate().unwrap();
    p
}

/// HyroTrader 1-Step Swing - same as Standard but with `pct_prior_day`
/// daily loss (the only difference; Swing is a paid checkout upgrade
/// that buys the softer daily-loss mechanism).
#[must_use]
pub fn hyrotrader_1step_swing() -> ChallengePlan {
    let mut p = hyrotrader_1step_standard();
    p.meta.program_name = "1-Step Swing".into();
    p.daily_loss_type = DailyLossType::PctPriorDay;
    p.validate().unwrap();
    p
}

/// HyroTrader 2-Step Standard - Phase 1: 10% target; Phase 2: 5% target.
/// Both: 5% daily loss (trailing_intraday_high), 10% static max drawdown,
/// 40% consistency, 5 min trading days, all 4 restrictions.
#[must_use]
pub fn hyrotrader_2step_standard_phase1() -> ChallengePlan {
    let mut p = base_plan("HyroTrader", "2-Step Standard - Phase 1", Money(dec!(50_000)));
    p.phase = ChallengePhase::Phase1;
    p.profit_target_pct = Pct(dec!(0.10));
    p.max_daily_drawdown_pct = Pct(dec!(0.05));
    p.max_total_drawdown_pct = Pct(dec!(0.10));
    p.daily_loss_type = DailyLossType::TrailingIntradayHigh;
    p.max_loss_reference = LossReference::Static;
    p.min_trading_days = 5;
    p.consistency_pct = Some(Pct(dec!(0.40)));
    p.consistency_type = ConsistencyType::BestDayPctOfPositiveDays;
    p.per_trade_max_loss_pct = Some(Pct(dec!(0.03)));
    p.grid_trading_allowed = false;
    p.hedging_allowed = false;
    p.cross_account_hedging_prohibited = true;
    p.lowcap_exposure_limit_pct = Some(Pct(dec!(0.05)));
    p.leverage = 100;
    p.validate().unwrap();
    p
}

#[must_use]
pub fn hyrotrader_2step_standard_phase2() -> ChallengePlan {
    let mut p = hyrotrader_2step_standard_phase1();
    p.phase = ChallengePhase::Phase2;
    p.meta.program_name = "2-Step Standard - Phase 2".into();
    p.profit_target_pct = Pct(dec!(0.05));
    p.validate().unwrap();
    p
}

/// HyroTrader 2-Step Swing - Phase 1: 10% target; Phase 2: 5% target.
/// Both: 5% daily loss (pct_prior_day), 10% static max drawdown, 40%
/// consistency, 5 min trading days, all 4 restrictions.
#[must_use]
pub fn hyrotrader_2step_swing_phase1() -> ChallengePlan {
    let mut p = hyrotrader_2step_standard_phase1();
    p.meta.program_name = "2-Step Swing - Phase 1".into();
    p.daily_loss_type = DailyLossType::PctPriorDay;
    p.validate().unwrap();
    p
}

#[must_use]
pub fn hyrotrader_2step_swing_phase2() -> ChallengePlan {
    let mut p = hyrotrader_2step_swing_phase1();
    p.phase = ChallengePhase::Phase2;
    p.meta.program_name = "2-Step Swing - Phase 2".into();
    p.profit_target_pct = Pct(dec!(0.05));
    p.validate().unwrap();
    p
}

// -----------------------------------------------------------------------------
// Bitfunded (3 plans): 2-Step, 1-Step, Instant. Per the dataset: all 3
// use `pct_initial` daily loss + `static` max drawdown, no consistency.
// -----------------------------------------------------------------------------

/// Bitfunded 2-Step - Phase 1: 8% target; Phase 2: 5% target. Phase 1:
/// 10% static max drawdown; Phase 2: 8% static. Both: 5% daily loss
/// (pct_initial), 0 min trading days.
#[must_use]
pub fn bitfunded_2step_phase1() -> ChallengePlan {
    let mut p = base_plan("Bitfunded", "2-Step - Phase 1", Money(dec!(50_000)));
    p.phase = ChallengePhase::Phase1;
    p.profit_target_pct = Pct(dec!(0.08));
    p.max_daily_drawdown_pct = Pct(dec!(0.05));
    p.max_total_drawdown_pct = Pct(dec!(0.10));
    p.daily_loss_type = DailyLossType::PctInitial;
    p.max_loss_reference = LossReference::Static;
    p.consistency_pct = None;
    p.consistency_type = ConsistencyType::None;
    p.leverage = 100;
    p.validate().unwrap();
    p
}

#[must_use]
pub fn bitfunded_2step_phase2() -> ChallengePlan {
    let mut p = bitfunded_2step_phase1();
    p.phase = ChallengePhase::Phase2;
    p.meta.program_name = "2-Step - Phase 2".into();
    p.profit_target_pct = Pct(dec!(0.05));
    p.max_total_drawdown_pct = Pct(dec!(0.08));
    p.validate().unwrap();
    p
}

/// Bitfunded 1-Step - 10% target, 4% daily loss (pct_initial), 6% static
/// max drawdown.
#[must_use]
pub fn bitfunded_1step() -> ChallengePlan {
    let mut p = base_plan("Bitfunded", "1-Step", Money(dec!(50_000)));
    p.phase = ChallengePhase::Phase1;
    p.profit_target_pct = Pct(dec!(0.10));
    p.max_daily_drawdown_pct = Pct(dec!(0.04));
    p.max_total_drawdown_pct = Pct(dec!(0.06));
    p.daily_loss_type = DailyLossType::PctInitial;
    p.max_loss_reference = LossReference::Static;
    p.consistency_pct = None;
    p.consistency_type = ConsistencyType::None;
    p.leverage = 100;
    p.validate().unwrap();
    p
}

/// Bitfunded Instant - instant-funding product (no evaluation). 0% target,
/// 3% daily loss (pct_initial), 6% static max drawdown.
#[must_use]
pub fn bitfunded_instant() -> ChallengePlan {
    let mut p = base_plan("Bitfunded", "Instant", Money(dec!(5_000)));
    p.phase = ChallengePhase::Funded;
    p.profit_target_pct = Pct::ZERO;
    p.max_daily_drawdown_pct = Pct(dec!(0.03));
    p.max_total_drawdown_pct = Pct(dec!(0.06));
    p.daily_loss_type = DailyLossType::PctInitial;
    p.max_loss_reference = LossReference::Static;
    p.consistency_pct = None;
    p.consistency_type = ConsistencyType::None;
    p.leverage = 100;
    p.validate().unwrap();
    p
}

// -----------------------------------------------------------------------------
// Apex Trader Funding (2 plans): EOD Trail, Intraday Trail. Per the
// dataset: both use `phases_by_account_size` because the dollar values
// don't reduce to one clean percentage across sizes. We expose them as
// size-parameterized constructors (option (b) from the gap analysis) -
// the caller picks the size and gets the right phase values baked in.
// -----------------------------------------------------------------------------

/// Apex EOD Trail Evaluation - 6% target, 2% daily loss (pct_initial,
/// `soft: true`), 4% EOD-trail max drawdown, 30-day time limit. The only
/// verified firm whose daily loss is `soft: true` (warning, not breach).
/// Dollar values are size-specific; pass the account size to get the
/// correct `value_dollars` baked in.
#[must_use]
pub fn apex_eod_trail(account_size: Money) -> ChallengePlan {
    let mut p = base_plan("Apex Trader Funding", "EOD Trail Evaluation", account_size);
    p.phase = ChallengePhase::Phase1;
    p.profit_target_pct = Pct(dec!(0.06));
    // 2% daily loss, soft (warning not breach). Dollar value is
    // 0.02 x account_size, but the dataset publishes it explicitly as
    // $500 for the $25K size. We compute it from the percentage.
    p.max_daily_drawdown_pct = Pct(dec!(0.02));
    p.daily_loss_type = DailyLossType::PctInitial;
    p.daily_loss_soft = true;
    p.max_total_drawdown_pct = Pct(dec!(0.04));
    p.max_loss_reference = LossReference::EodTrailing;
    p.min_trading_days = 0;
    p.time_limit_days = Some(30);
    p.consistency_pct = None;
    p.consistency_type = ConsistencyType::None;
    p.leverage = 1; // futures
    p.validate().unwrap();
    p
}

/// Apex Intraday Trail Evaluation - 6% target, NO daily loss, 4%
/// intraday_trail max drawdown (the harshest max-DD mechanism in the
/// dataset - floor follows unrealised equity peak), 30-day time limit.
#[must_use]
pub fn apex_intraday_trail(account_size: Money) -> ChallengePlan {
    let mut p = base_plan("Apex Trader Funding", "Intraday Trail Evaluation", account_size);
    p.phase = ChallengePhase::Phase1;
    p.profit_target_pct = Pct(dec!(0.06));
    p.max_daily_drawdown_pct = Pct::ZERO;
    p.daily_loss_type = DailyLossType::None;
    p.max_total_drawdown_pct = Pct(dec!(0.04));
    p.max_loss_reference = LossReference::IntradayTrail;
    p.min_trading_days = 0;
    p.time_limit_days = Some(30);
    p.consistency_pct = None;
    p.consistency_type = ConsistencyType::None;
    p.leverage = 1; // futures
    p.validate().unwrap();
    p
}
