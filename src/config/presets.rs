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

use crate::config::plan::{ChallengePhase, ChallengePlan, LossReference, PlanMeta};
use crate::core::ids::ChallengeId;
use crate::core::types::{Money, Pct, dec};
use chrono::Utc;

fn base_plan(firm: &str, program: &str, balance: Money) -> ChallengePlan {
    let mut p = ChallengePlan::default();
    p.id = ChallengeId::new();
    p.meta = PlanMeta {
        firm_name: firm.into(),
        program_name: program.into(),
        version: "1.0.0".into(),
        currency: "USD".into(),
        description: format!("{firm} {program} evaluation program"),
    };
    p.initial_balance_money = balance;
    p.effective_at = Utc::now();
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
pub fn ftmo_phase1() -> ChallengePlan {
    let mut p = base_plan("FTMO", "Challenge", Money(dec!(10_000)));
    p.phase = ChallengePhase::Phase1;
    p.profit_target_pct = Pct(dec!(0.10));
    p.max_daily_drawdown_pct = Pct(dec!(0.05));
    p.max_total_drawdown_pct = Pct(dec!(0.10));
    // P0-1: FTMO phase-1 uses STATIC max total loss. The floor is the
    // initial balance; growing to $11k and pulling back to $9.5k does NOT
    // breach (you're still above the $9k floor).
    p.max_loss_reference = LossReference::Static;
    p.drawdown_on_balance = false;
    p.min_trading_days = 4;  // P1.12: FTMO published spec is 4, not 3.
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
/// Note: real 1-Step plans use EOD-reset trailing max loss; the engine
/// supports this via `LossReference::EodTrailing` once P1.6 lands.
pub fn ftmo_1step() -> ChallengePlan {
    let mut p = base_plan("FTMO", "1-Step Challenge", Money(dec!(10_000)));
    p.phase = ChallengePhase::Phase1;
    p.profit_target_pct = Pct(dec!(0.10));
    p.max_daily_drawdown_pct = Pct(dec!(0.05));
    p.max_total_drawdown_pct = Pct(dec!(0.10));
    // P1.6: when EOD-trailing lands, switch this to EodTrailing.
    p.max_loss_reference = LossReference::Static;
    p.drawdown_on_balance = false;
    p.min_trading_days = 0;  // 1-Step has no min days.
    p.time_limit_days = None;  // unlimited + inactivity termination (P1 future).
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

/// **P1.12 fix**: Instant Funding plan (no evaluation).
///
/// A growing 2026 product: pay a higher fee upfront and skip the
/// evaluation entirely — start on a funded account with reduced profit
/// split (often 50% → scaling plan → 90%) until a profit target is hit.
/// This preset captures the funded-with-restrictions shape.
pub fn ftmo_instant_funding() -> ChallengePlan {
    let mut p = base_plan("FTMO", "Instant Funding", Money(dec!(10_000)));
    p.phase = ChallengePhase::Funded;
    p.profit_target_pct = Pct(dec!(0.05));  // lower target — just verify profitability
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
pub fn fundednext_1step() -> ChallengePlan {
    let mut p = base_plan("FundedNext", "1-Step", Money(dec!(50_000)));
    p.phase = ChallengePhase::Phase1;
    p.profit_target_pct = Pct(dec!(0.08));
    p.max_daily_drawdown_pct = Pct(dec!(0.05));
    p.max_total_drawdown_pct = Pct(dec!(0.10));
    p.max_loss_reference = LossReference::Static;
    p.min_trading_days = 0;
    p.time_limit_days = None;
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
pub fn topstep_futures() -> ChallengePlan {
    let mut p = base_plan("Topstep", "Trading Combine", Money(dec!(50_000)));
    p.phase = ChallengePhase::Phase1;
    p.profit_target_pct = Pct(dec!(0.06));
    p.max_daily_drawdown_pct = Pct(dec!(0.05));
    p.max_total_drawdown_pct = Pct(dec!(0.10));
    p.max_loss_reference = LossReference::Static;
    p.min_trading_days = 5;
    p.time_limit_days = None;
    p.news_trading_allowed = true;
    p.overnight_holding_allowed = true;
    p.weekend_holding_allowed = true;
    p.hedging_allowed = false;
    p.require_stop_loss = true;
    p.consistency_pct = None;
    p.leverage = 1;  // futures: no leverage flag; 1 contract per position
    p.validate().unwrap();
    p
}
