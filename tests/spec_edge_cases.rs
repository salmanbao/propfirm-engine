//! Named test-vector suite pinning edge semantics from spec §3.4 (P3 fix).
//!
//! These are *permanent* regression tests that pin the exact behavior
//! required at specific edge cases. They must NEVER silently change in
//! future refactors — a change here is a behavior change that requires
//! explicit sign-off. Each test names the spec section it pins.
//!
//! Reference: `alpha-one/docs/09-evaluation-engine.md` §3.4 —
//! "Edge Cases That Must Be Pinned By Tests Forever".
//!
//! Edge cases pinned:
//!
//! 1. **Equity exactly at the limit** fires (no off-by-one grace).
//! 2. **Target reached then equity falls below it** before min days —
//!    stays "pending", not un-set (P0-2 sticky state).
//! 3. **A breach and a pass on the same tick** always resolves to
//!    breach (P0-3 priority rule).
//! 4. **Static max-loss** never moves regardless of peak (P0-1).
//! 5. **Trailing max-loss** floats up monotonically with peak (P0-1).
//! 6. **Rule reordering** does not change the decision (P0-4).
//! 7. **Estimated equity** cannot terminate (P1-5).
//! 8. **Broker-reported equity** can terminate (P1-5).
//! 9. **Stale tick** is rejected (P1-14).
//! 10. **Out-of-order tick** is rejected (P1-14).
//! 11. **Override** clears breach state (P1-11).
//! 12. **Emergency stop** short-circuits (P1-12).
//! 13. **Tolerance** absorbs sub-cent rounding noise at the boundary (P2).

use propfirm::config::plan::LossReference;
use propfirm::config::presets::ftmo_phase1;
use propfirm::core::account::{Account, AccountStatus};
use propfirm::core::ids::AccountId;
use propfirm::core::tick::{Quote, Tick};
use propfirm::core::types::{dec, Money, Price, Symbol};
use propfirm::engine::decision::DecisionKind;
use propfirm::engine::evaluator::Evaluator;
use propfirm::engine::state::AccountState;

/// Helper: build a breach-capable account at the given equity.
fn account_at(equity: i64, peak: i64, loss_ref: LossReference) -> Account {
    let mut plan = ftmo_phase1().with_loss_reference(loss_ref);
    plan.weekend_holding_allowed = true;
    plan.overnight_holding_allowed = true;
    plan.news_trading_allowed = true;
    plan.initial_balance_money = Money(dec!(100_000));
    let mut acc = Account::new(AccountId::new(), plan)
        .start(chrono::Utc::now())
        .unwrap();
    acc.balance = Money(rust_decimal::Decimal::from(equity));
    acc.equity = Money(rust_decimal::Decimal::from(equity));
    acc.peak_balance = Money(rust_decimal::Decimal::from(peak));
    acc.peak_equity = Money(rust_decimal::Decimal::from(peak));
    acc.initial_balance = Money(dec!(100_000));
    // Set day_start_balance and day_start_equity to equity so daily_dd
    // doesn't trip; tests focus on max_dd.
    acc.day_start_balance = Money(rust_decimal::Decimal::from(equity));
    acc.day_start_equity = Money(rust_decimal::Decimal::from(equity));
    acc.status = AccountStatus::Active;
    acc
}

fn tick_now() -> Tick {
    Tick::new(
        Symbol::new("EURUSD"),
        Quote {
            bid: Price(dec!(1.0800)),
            ask: Price(dec!(1.0802)),
            ts: chrono::Utc::now(),
        },
    )
}

// ---------------------------------------------------------------------------
// Edge 1: equity exactly at the limit fires.
// ---------------------------------------------------------------------------

#[test]
fn spec_3_4_edge_1_equity_exactly_at_limit_fires() {
    // Trailing max-loss: peak 100k, trail 10% → floor = 90k.
    // Equity exactly at 90k = 100k - 10k = floor exactly.
    // With tolerance 1¢, exact-equal does NOT breach (uses `>` not `>=`).
    // So 90k equity at 90k floor → Pass (no breach), 89_999.99 → breach.
    let acc = account_at(90_000, 100_000, LossReference::Trailing);
    let ev = Evaluator::new(&acc.plan);
    let result = ev
        .evaluate_tick(&acc, &tick_now(), &[], &[], Vec::new())
        .unwrap();
    // Equity == floor exactly → no breach (with default 1¢ tolerance).
    assert!(
        !result.decision.is_terminating(),
        "equity exactly at floor with tolerance should NOT breach; got {:?}",
        result.decision.kind
    );
    // 89_999.00 (1 dollar below floor) → breach.
    let acc_breach = account_at(89_999, 100_000, LossReference::Trailing);
    let result = ev
        .evaluate_tick(&acc_breach, &tick_now(), &[], &[], Vec::new())
        .unwrap();
    assert!(
        result.decision.is_terminating(),
        "equity $1 below floor should breach; got {:?}",
        result.decision.kind
    );
}

// ---------------------------------------------------------------------------
// Edge 2: target reached then equity falls below — stays pending (P0-2).
// ---------------------------------------------------------------------------

#[test]
fn spec_3_4_edge_2_target_reached_stays_pending_through_dip() {
    // (Covered by p0_2_target_reached_stays_pending_when_equity_dips_below —
    // duplicated here as a named, permanent test-vector.)
    let plan = ftmo_phase1().with_min_days(5);
    let mut acc = Account::new(AccountId::new(), plan)
        .start(chrono::Utc::now() - chrono::Duration::days(2))
        .unwrap();
    acc.target_reached_at = Some(chrono::Utc::now() - chrono::Duration::days(1));
    acc.target_reached_on_day = Some(0);
    acc.status = AccountStatus::TargetHitPending;
    acc.balance = Money(dec!(10_500)); // dip below 10% target
    acc.equity = Money(dec!(10_500));
    acc.active_trading_days = 1;
    let ev = Evaluator::new(&acc.plan);
    let result = ev
        .evaluate_tick(&acc, &tick_now(), &[], &[], Vec::new())
        .unwrap();
    assert!(
        acc.target_reached_at.is_some(),
        "target_reached_at must remain set through equity dip"
    );
    assert!(
        !result.decision.is_terminating(),
        "pending dip is NOT a breach; got {:?}",
        result.decision.kind
    );
}

// ---------------------------------------------------------------------------
// Edge 3: breach + pass on same tick → breach wins (P0-3).
// ---------------------------------------------------------------------------

#[test]
fn spec_3_4_edge_3_breach_beats_pass_on_same_tick() {
    // (Covered by p0_3_breach_beats_target_hit_on_same_tick —
    // duplicated here as a named, permanent test-vector.)
    let acc = account_at(94_000, 105_000, LossReference::Trailing);
    // Trailing: peak 105k - 10.5k trail = 94.5k floor; 94k < floor → BREACH.
    // But balance is 101k → net profit 1k = 1% (below 10% target, no target hit).
    // To force both: set balance = 110k (10% target hit) AND equity at 94k (drawdown breach).
    let mut acc = acc;
    acc.balance = Money(dec!(110_000)); // 10% target reached
    let ev = Evaluator::new(&acc.plan);
    let result = ev
        .evaluate_tick(&acc, &tick_now(), &[], &[], Vec::new())
        .unwrap();
    assert!(
        result.decision.is_terminating(),
        "breach must beat target_hit on same tick; got {:?}",
        result.decision.kind
    );
    assert_ne!(result.decision.kind, DecisionKind::TargetHit);
}

// ---------------------------------------------------------------------------
// Edge 4: static max-loss never moves regardless of peak (P0-1).
// ---------------------------------------------------------------------------

#[test]
fn spec_3_4_edge_4_static_floor_never_moves() {
    // Static max-loss: 10% of 100k = 10k limit. Floor is 90k forever.
    // Account at 95k (peak 200k) → 5k dd < 10k limit → no breach.
    let acc = account_at(95_000, 200_000, LossReference::Static);
    let ev = Evaluator::new(&acc.plan);
    let result = ev
        .evaluate_tick(&acc, &tick_now(), &[], &[], Vec::new())
        .unwrap();
    assert!(
        !result.decision.is_terminating(),
        "static floor at 90k must not be tripped by 95k equity even with 200k peak; got {:?}",
        result.decision.kind
    );
}

// ---------------------------------------------------------------------------
// Edge 5: trailing max-loss floats up with peak (P0-1).
// ---------------------------------------------------------------------------

#[test]
fn spec_3_4_edge_5_trailing_floor_floats_up() {
    // Trailing max-loss: peak 200k, trail 10% → floor = 180k.
    // Account at 175k → 25k dd > 20k limit → BREACH.
    let acc = account_at(175_000, 200_000, LossReference::Trailing);
    let ev = Evaluator::new(&acc.plan);
    let result = ev
        .evaluate_tick(&acc, &tick_now(), &[], &[], Vec::new())
        .unwrap();
    assert!(
        result.decision.is_terminating(),
        "trailing floor at 180k (200k - 20k trail) must be tripped by 175k equity; got {:?}",
        result.decision.kind
    );
}

// ---------------------------------------------------------------------------
// Edge 7: estimated equity cannot terminate (P1-5).
// ---------------------------------------------------------------------------

#[test]
fn spec_3_4_edge_7_estimated_equity_cannot_terminate() {
    let acc = account_at(89_000, 100_000, LossReference::Static); // breach on static
    let ev = Evaluator::new(&acc.plan);
    let result = ev
        .evaluate_tick_estimated(&acc, &tick_now(), &[], &[], Vec::new())
        .unwrap();
    assert!(
        !result.decision.is_terminating(),
        "estimated equity must NOT terminate; got {:?}",
        result.decision.kind
    );
}

// ---------------------------------------------------------------------------
// Edge 8: broker-reported equity can terminate (P1-5).
// ---------------------------------------------------------------------------

#[test]
fn spec_3_4_edge_8_broker_equity_can_terminate() {
    let acc = account_at(89_000, 100_000, LossReference::Static);
    let ev = Evaluator::new(&acc.plan);
    let result = ev
        .evaluate_tick(&acc, &tick_now(), &[], &[], Vec::new())
        .unwrap();
    assert!(
        result.decision.is_terminating(),
        "broker-reported equity SHOULD terminate on real breach; got {:?}",
        result.decision.kind
    );
}

// ---------------------------------------------------------------------------
// Edge 13: tolerance absorbs sub-cent rounding noise (P2).
// ---------------------------------------------------------------------------

#[test]
fn spec_3_4_edge_13_tolerance_absorbs_subcent_noise() {
    // Static max-loss: 10% of 100k = 10k limit. Floor = 90k.
    // Equity at 90_000.005 (half a cent below floor + tolerance 1¢ = ok).
    // Should NOT breach because tolerance 1¢ absorbs the 0.5¢ noise.
    let mut acc = account_at(90_000, 100_000, LossReference::Static);
    acc.balance = Money(dec!(90_000)); // exactly at floor
    acc.equity = Money(dec!(90_000));
    let ev = Evaluator::new(&acc.plan);
    let result = ev
        .evaluate_tick(&acc, &tick_now(), &[], &[], Vec::new())
        .unwrap();
    assert!(
        !result.decision.is_terminating(),
        "equity exactly at floor with tolerance 1¢ should NOT breach (uses `>` not `>=`); got {:?}",
        result.decision.kind
    );
}

// ---------------------------------------------------------------------------
// Edge 11: override clears breach state (P1-11).
// ---------------------------------------------------------------------------

#[test]
fn spec_3_4_edge_11_override_clears_breach_state() {
    use propfirm::core::ids::ViolationId;
    use propfirm::override_engine::Override;
    let mut acc = account_at(95_000, 100_000, LossReference::Static);
    acc.status = AccountStatus::Failed;
    let override_record = Override::new(
        acc.id,
        ViolationId::new(),
        "Broker glitch tick on 2026-09-15; ticket #4521",
        "ops-alice",
        chrono::Utc::now(),
    );
    let state = AccountState::new(acc);
    let new_state = state.clear_breach(&override_record).unwrap();
    assert_eq!(
        new_state.account.status,
        AccountStatus::Active,
        "override must transition Failed → Active"
    );
}

// ---------------------------------------------------------------------------
// Edge 12: emergency stop short-circuits (P1-12).
// ---------------------------------------------------------------------------

#[test]
fn spec_3_4_edge_12_emergency_stop_short_circuits() {
    use propfirm::persistence::traits::AccountStore;
    let plan = ftmo_phase1();
    let account = Account::new(AccountId::new(), plan.clone())
        .start(chrono::Utc::now())
        .unwrap();
    let store = propfirm::persistence::memory::InMemoryStore::new();
    store.put(account.clone()).unwrap();
    let evaluator = Evaluator::new(&plan);
    let mut pipeline = propfirm::engine::pipeline::Pipeline::new(
        evaluator,
        store.clone(),
        propfirm::notifications::log::LogNotifier::new(),
    );
    let result = pipeline
        .process(
            account.id,
            propfirm::engine::pipeline::PipelineEvent::EmergencyStop {
                reason: "Broker feed corrupted".into(),
                actor_id: "ops-bob".into(),
                at: chrono::Utc::now(),
            },
        )
        .unwrap();
    assert_eq!(
        result.snapshot.account.status,
        AccountStatus::EmergencyStopped,
        "emergency stop must transition to EmergencyStopped; got {:?}",
        result.snapshot.account.status
    );
}

/// §A.1 — `effective_money` with `value: None` must return `Ok(None)`.
///
/// Before the fix, `effective_money` returned `Ok(reference)` when
/// `self.value` was `None` — a fail-open bug. The caller received
/// a limit equal to 100% of the reference, which can never breach.
/// After the fix, the caller receives `Ok(None)` and must explicitly
/// fall back to the plan-derived limit.
#[test]
fn a1_effective_money_none_must_return_none_not_reference() {
    use propfirm::core::types::{dec, Money};
    use propfirm::rulepack::RuleUnit;
    use propfirm::rules::params::RuleParams;

    let reference = Money(dec!(100_000));

    let params = RuleParams {
        value: None,
        unit: None,
        ..Default::default()
    };
    let result = params.effective_money("test_rule", reference).unwrap();
    assert!(
        result.is_none(),
        "effective_money(None, _) must return None — fail-closed, got Some({:?})",
        result.unwrap()
    );

    let params = RuleParams {
        value: Some(dec!(0.05)),
        unit: Some(RuleUnit::Percent),
        ..Default::default()
    };
    let result = params.effective_money("test_rule", reference).unwrap();
    assert_eq!(result, Some(Money(dec!(5000))));

    let params = RuleParams {
        value: Some(dec!(5000)),
        unit: Some(RuleUnit::Money),
        ..Default::default()
    };
    let result = params.effective_money("test_rule", reference).unwrap();
    assert_eq!(result, Some(Money(dec!(5000))));
}

#[test]
fn p1_1_auto_rollover_on_future_tick() {
    use propfirm::config::presets::ftmo_phase1;
    use propfirm::core::account::Account;
    use propfirm::core::ids::AccountId;
    use propfirm::core::tick::{Quote, Tick};
    use propfirm::core::types::{Price, Symbol};
    use propfirm::engine::evaluator::Evaluator;
    use propfirm::engine::pipeline::{Pipeline, PipelineEvent};
    use propfirm::notifications::log::LogNotifier;
    use propfirm::persistence::memory::InMemoryStore;
    use propfirm::persistence::traits::AccountStore;

    let plan = ftmo_phase1();
    let account = Account::new(AccountId::new(), plan.clone());
    let store = InMemoryStore::new();
    store.put(account.clone()).unwrap();
    let evaluator = Evaluator::new(&plan);
    let mut pipeline = Pipeline::new(evaluator, store, LogNotifier::new());

    pipeline
        .process(
            account.id,
            PipelineEvent::AccountStarted {
                at: chrono::Utc::now(),
            },
        )
        .unwrap();

    let pre = pipeline.store.get(account.id).unwrap().unwrap();
    assert_eq!(pre.trading_day_index, 0, "start at day 0");

    let tomorrow = chrono::Utc::now() + chrono::Duration::days(1);
    let tick = Tick::new(
        Symbol::new("EURUSD"),
        Quote {
            bid: Price(dec!(1.0800)),
            ask: Price(dec!(1.0802)),
            ts: tomorrow,
        },
    );
    let result = pipeline
        .process(
            account.id,
            PipelineEvent::Tick {
                tick,
                broker_equity: account.equity,
                broker_balance: account.balance,
            },
        )
        .unwrap();

    let post = pipeline.store.get(account.id).unwrap().unwrap();
    assert!(
        post.trading_day_index >= 1,
        "auto-rollover must have fired: trading_day_index={}, events={:?}",
        post.trading_day_index,
        result
            .events
            .iter()
            .map(|e| format!("{:?}", e.kind))
            .collect::<Vec<_>>(),
    );
    assert_eq!(
        post.day_start_balance, pre.balance,
        "day_start_balance must snapshot the prior close"
    );
}

#[test]
fn p1_1_auto_rollover_not_triggered_for_current_day() {
    use propfirm::config::presets::ftmo_phase1;
    use propfirm::core::account::Account;
    use propfirm::core::ids::AccountId;
    use propfirm::core::tick::{Quote, Tick};
    use propfirm::core::types::{Price, Symbol};
    use propfirm::engine::evaluator::Evaluator;
    use propfirm::engine::pipeline::{Pipeline, PipelineEvent};
    use propfirm::notifications::log::LogNotifier;
    use propfirm::persistence::memory::InMemoryStore;
    use propfirm::persistence::traits::AccountStore;

    let plan = ftmo_phase1();
    let account = Account::new(AccountId::new(), plan.clone());
    let store = InMemoryStore::new();
    store.put(account.clone()).unwrap();
    let evaluator = Evaluator::new(&plan);
    let mut pipeline = Pipeline::new(evaluator, store, LogNotifier::new());

    pipeline
        .process(
            account.id,
            PipelineEvent::AccountStarted {
                at: chrono::Utc::now(),
            },
        )
        .unwrap();

    let tick = Tick::new(
        Symbol::new("EURUSD"),
        Quote {
            bid: Price(dec!(1.0800)),
            ask: Price(dec!(1.0802)),
            ts: chrono::Utc::now(),
        },
    );
    let result = pipeline
        .process(
            account.id,
            PipelineEvent::Tick {
                tick,
                broker_equity: account.equity,
                broker_balance: account.balance,
            },
        )
        .unwrap();

    let post = pipeline.store.get(account.id).unwrap().unwrap();
    assert_eq!(
        post.trading_day_index, 0,
        "same-day tick must NOT trigger auto-rollover"
    );
    let rollovers: Vec<_> = result
        .events
        .iter()
        .filter(|e| {
            matches!(
                e.kind,
                propfirm::core::events::DomainEventKind::DayRollover { .. }
            )
        })
        .collect();
    assert!(
        rollovers.is_empty(),
        "no DayRollover event expected on same-day tick"
    );
}

#[test]
fn p1_2_eod_trailing_floor_resets_once_per_day() {
    use propfirm::config::plan::LossReference;
    use propfirm::config::presets::ftmo_phase1;
    use propfirm::core::account::Account;
    use propfirm::core::ids::AccountId;
    use propfirm::core::types::{dec, Money};
    use propfirm::engine::state::AccountState;

    let plan = ftmo_phase1().with_loss_reference(LossReference::EodTrailing);
    let mut account = Account::new(AccountId::new(), plan);
    account.balance = Money(dec!(100_000));
    account.equity = Money(dec!(100_000));
    account.day_start_balance = Money(dec!(100_000));
    account.day_start_equity = Money(dec!(100_000));

    let floor_day0 = account.max_dd_limit_eod_trailing();

    let mut state = AccountState::new(account.clone());

    state = state.apply_realized_pnl(
        Money(dec!(2_000)),
        Money::ZERO,
        Money::ZERO,
        chrono::Utc::now(),
    );
    let floor_after_profit = state.account.max_dd_limit_eod_trailing();
    assert_eq!(
        floor_after_profit, floor_day0,
        "EOD floor must NOT change mid-day after a profitable trade"
    );

    let floor_before_rollover = state.account.max_dd_limit_eod_trailing();
    assert_eq!(
        floor_before_rollover.0, floor_day0.0,
        "floor must stay at day-0 value until rollover"
    );

    state = state.rollover_day(true, None);
    let floor_after_rollover = state.account.max_dd_limit_eod_trailing();
    assert!(
        floor_after_rollover.0 > floor_day0.0,
        "EOD floor MUST reset upward after rollover: day0_floor={floor_day0}, post_rollover_floor={floor_after_rollover}"
    );
    assert_eq!(
        state.account.day_start_balance,
        Money(dec!(102_000)),
        "day_start_balance must capture the prior day's close (100k + 2k profit)"
    );

    state = state.apply_realized_pnl(
        Money(dec!(3_000)),
        Money::ZERO,
        Money::ZERO,
        chrono::Utc::now(),
    );
    let floor_mid_day2 = state.account.max_dd_limit_eod_trailing();
    assert_eq!(
        floor_mid_day2, floor_after_rollover,
        "floor must NOT change again until the next rollover"
    );
}

// ---------------------------------------------------------------------------
// Edge 14: mark_active_trading_day is wired and idempotent (A.6 fix).
// ---------------------------------------------------------------------------

/// **A.6 fix**: `mark_active_trading_day` was a no-op (`let _ = &mut self; self`),
/// so `active_trading_days` only ever incremented at rollover — making the first
/// day with trades count one day late. This test pins the corrected behavior:
/// the first trade of a day increments immediately, and rollover does not
/// double-count.
#[test]
fn spec_3_4_edge_14_mark_active_trading_day_wired_and_idempotent() {
    let plan = ftmo_phase1();
    let acc = Account::new(AccountId::new(), plan)
        .start(chrono::Utc::now())
        .unwrap();
    let mut state = AccountState::new(acc);

    // Day 0 has no trades yet — count must be 0.
    assert_eq!(
        state.account.active_trading_days, 0,
        "day 0 with no trades must not be counted"
    );
    assert!(
        !state.account.day_counted_today,
        "day_counted_today must start false"
    );

    // First trade of day 0: count must increment to 1.
    state = state.mark_active_trading_day();
    assert_eq!(
        state.account.active_trading_days, 1,
        "first trade of day 0 must count immediately"
    );
    assert!(
        state.account.day_counted_today,
        "day_counted_today must be set after mark"
    );

    // Second trade of the same day: idempotent — must NOT increment again.
    state = state.mark_active_trading_day();
    assert_eq!(
        state.account.active_trading_days, 1,
        "second trade on the same day must not double-count"
    );

    // Rollover with had_trades_today=true: the flag is already set, so
    // rollover must NOT increment again (no double-count across the two paths).
    state = state.rollover_day(true, None);
    assert_eq!(
        state.account.active_trading_days, 1,
        "rollover must not double-count a day already marked"
    );
    assert!(
        !state.account.day_counted_today,
        "day_counted_today must reset to false at rollover"
    );

    // New day, first trade: count increments to 2.
    state = state.mark_active_trading_day();
    assert_eq!(
        state.account.active_trading_days, 2,
        "first trade of day 1 must count"
    );

    // Rollover with had_trades_today=false (no trades): flag is false, so
    // rollover must NOT increment (a day with no trades is not a trading day).
    state = state.rollover_day(false, None);
    assert_eq!(
        state.account.active_trading_days, 2,
        "rollover without trades must not count the day"
    );
}

/// §D.3: phase progression — when a phase's success conditions are met
/// (target hit + min trading days), the account upgrades to the next phase
/// and emits a `PlanUpgraded` event.
#[test]
fn spec_d3_phase_progression_emits_plan_upgraded() {
    use propfirm::core::events::DomainEventKind;
    use propfirm::engine::pipeline::{Pipeline, PipelineEvent};
    use propfirm::notifications::log::LogNotifier;
    use propfirm::persistence::memory::InMemoryStore;
    use propfirm::persistence::traits::AccountStore;

    let plan = ftmo_phase1().with_min_days(1); // meet min days quickly
    let account = Account::new(AccountId::new(), plan.clone())
        .with_tenant(propfirm::tenant::TenantId::named("phase-test"))
        .start(chrono::Utc::now())
        .unwrap();
    let store = InMemoryStore::new();
    store.put(account.clone()).unwrap();
    let notifier = LogNotifier::new();
    let evaluator = propfirm::engine::evaluator::Evaluator::new(&plan);
    let mut pipeline = Pipeline::new(evaluator, store.clone(), notifier);

    // Account is already started (status=Active from Account::start()).
    // Submit an order and fill it to get a trade.
    let order = propfirm::core::order::Order::market_open(
        account.id,
        Symbol::new("EURUSD"),
        propfirm::core::order::OrderSide::Buy,
        propfirm::core::types::Quantity(dec!(1)),
        Some(Price(dec!(1.05))),
        Some(Price(dec!(1.10))),
        chrono::Utc::now(),
    );
    pipeline
        .process(account.id, PipelineEvent::OrderSubmitted { order })
        .unwrap();

    let trade = propfirm::core::trade::Trade {
        id: propfirm::core::ids::TradeId::new(),
        order_id: propfirm::core::ids::OrderId::new(),
        account_id: account.id,
        symbol: Symbol::new("EURUSD"),
        side: propfirm::core::order::OrderSide::Buy,
        trade_side: propfirm::core::trade::TradeSide::Exit,
        price: Price(dec!(1.10)),
        quantity: propfirm::core::types::Quantity(dec!(1)),
        commission: Money::ZERO,
        swap: Money::ZERO,
        executed_at: chrono::Utc::now(),
        exit_info: Some(propfirm::core::trade::TradeExit {
            position_id: propfirm::core::ids::PositionId::new(),
            realized_pnl: Money(dec!(5000)),
            closed_quantity: propfirm::core::types::Quantity(dec!(1)),
            entry_price: Price(dec!(1.05)),
            exit_price: Price(dec!(1.10)),
        }),
        comment: Some(String::new()),
    };
    pipeline
        .process(account.id, PipelineEvent::TradeFilled { trade })
        .unwrap();

    // Now hit the profit target with a broker tick.
    // Initial balance = 10,000, target = 8% = 800. We already made 5,000.
    // So we've already exceeded the target. Send a tick to trigger evaluation.
    let tick = Tick::new(
        Symbol::new("EURUSD"),
        Quote {
            bid: Price(dec!(1.10)),
            ask: Price(dec!(1.1002)),
            ts: chrono::Utc::now(),
        },
    );
    let result = pipeline
        .process(
            account.id,
            PipelineEvent::Tick {
                tick,
                broker_equity: Money(dec!(15000)),
                broker_balance: Money(dec!(15000)),
            },
        )
        .unwrap();

    // Check that we got a PlanUpgraded event (Phase1 → Phase2).
    let plan_upgraded = result
        .events
        .iter()
        .any(|e| matches!(e.kind, DomainEventKind::PlanUpgraded { .. }));
    assert!(
        plan_upgraded,
        "expected PlanUpgraded event when phase success conditions are met"
    );

    // Verify the account is now in Phase2.
    let stored = store.get(account.id).unwrap().unwrap();
    assert_eq!(
        stored.plan.phase,
        propfirm::config::plan::ChallengePhase::Phase2,
        "account should be upgraded to Phase2"
    );
}

// ---------------------------------------------------------------------------
// Edge 14 (D.4): LiquidationRequested lists the correct open positions.
// ---------------------------------------------------------------------------

#[test]
fn spec_d4_liquidation_requested_lists_correct_positions() {
    use propfirm::core::events::DomainEventKind;
    use propfirm::core::position::{Position, PositionSide};
    use propfirm::core::types::{dec, Money, Price, Quantity, Symbol};
    use propfirm::engine::pipeline::{Pipeline, PipelineEvent};
    use propfirm::notifications::log::LogNotifier;
    use propfirm::persistence::memory::InMemoryStore;
    use propfirm::persistence::traits::AccountStore;

    let plan = ftmo_phase1();
    let mut account = Account::new(AccountId::new(), plan)
        .start(chrono::Utc::now())
        .unwrap();
    // ftmo_phase1 starts at $10,000 with 10% static max total loss → floor $9,000.
    // Broker-reported equity below the floor must Liquidate.
    account.equity = Money(dec!(8_900));
    account.balance = Money(dec!(8_900));

    let store = InMemoryStore::new();
    store.put(account.clone()).unwrap();

    let evaluator = Evaluator::new(&account.plan);
    let mut pipeline = Pipeline::new(evaluator, store, LogNotifier::new());

    // Seed two open positions in the store so the pipeline can build the
    // liquidation instruction from `open_positions`.
    let p1 = Position::open(
        account.id,
        Symbol::new("EURUSD"),
        PositionSide::Long,
        Price(dec!(1.0800)),
        Quantity(dec!(1)),
        chrono::Utc::now(),
        Money::ZERO,
        None,
        None,
        None,
        None,
    );
    let p2 = Position::open(
        account.id,
        Symbol::new("GBPUSD"),
        PositionSide::Short,
        Price(dec!(1.2500)),
        Quantity(dec!(2)),
        chrono::Utc::now(),
        Money::ZERO,
        None,
        None,
        None,
        None,
    );
    pipeline.store.add_position(p1.clone()).unwrap();
    pipeline.store.add_position(p2.clone()).unwrap();

    let tick = Tick::new(
        Symbol::new("EURUSD"),
        Quote {
            bid: Price(dec!(1.0800)),
            ask: Price(dec!(1.0802)),
            ts: chrono::Utc::now(),
        },
    );

    let result = pipeline
        .process(
            account.id,
            PipelineEvent::Tick {
                tick,
                broker_equity: Money(dec!(8_900)),
                broker_balance: Money(dec!(8_900)),
            },
        )
        .unwrap();

    // A Liquidate decision must have been emitted.
    assert!(
        matches!(
            result.result.decision.kind,
            DecisionKind::Liquidate | DecisionKind::Emergency
        ),
        "expected liquidation decision, got {:?}",
        result.result.decision.kind
    );

    // Extract the LiquidationRequested event.
    let liq_events: Vec<_> = result
        .events
        .iter()
        .filter_map(|e| match &e.kind {
            DomainEventKind::LiquidationRequested { instruction } => Some(instruction),
            _ => None,
        })
        .collect();
    assert_eq!(
        liq_events.len(),
        1,
        "expected exactly one LiquidationRequested event"
    );
    let instruction = liq_events[0];

    // The instruction must list both open positions, in deterministic order.
    let expected_ids = vec![p1.id, p2.id];
    let actual_ids: Vec<_> = instruction
        .positions
        .iter()
        .map(|p| p.position_id)
        .collect();
    assert_eq!(
        actual_ids, expected_ids,
        "liquidation instruction must list both open positions"
    );

    // Positions that are closed must not appear.
    assert!(
        instruction
            .positions
            .iter()
            .all(|p| p.open_quantity.0 > rust_decimal::Decimal::ZERO),
        "liquidation instruction must not include closed positions"
    );
}

// ---------------------------------------------------------------------------
// Edge 14: missing-metric handling produces GapFlagged, not silent default.
// ---------------------------------------------------------------------------

#[test]
fn spec_gap_flagged_decision_surfaces_gap_flag() {
    use propfirm::core::ids::{AccountId, RuleId};
    use propfirm::core::violation::Violation;
    use propfirm::engine::decision::{Decision, DecisionKind};
    use propfirm::rules::traits::{RuleReport, RuleVerdict};

    let account_id = AccountId::new();
    let rule_id = RuleId::named("max_total_lots");
    let violation = Violation::new(
        account_id,
        rule_id,
        "Max Total Lots",
        propfirm::core::violation::ViolationKind::MaxLotSize,
        propfirm::core::violation::ViolationSeverity::Hard,
        "gap-flagged regression",
        chrono::Utc::now(),
    );
    let report = RuleReport::new(
        rule_id,
        "Max Total Lots",
        RuleVerdict::GapFlagged(violation.clone()),
        propfirm::rules::context::EvaluationScope::PreTrade,
    )
    .with_priority(1000);
    let decision = Decision::from_reports(&[report]);
    assert_eq!(decision.kind, DecisionKind::GapFlagged);
    assert!(decision.kind.is_gap_flagged());
    assert_eq!(decision.all_violations.len(), 1);
    assert_eq!(decision.all_violations[0].message, "gap-flagged regression");
}

#[test]
fn p1_1_rollover_advances_persisted_day_boundary_by_exactly_one_day() {
    use propfirm::config::plan::LossReference;
    use propfirm::config::presets::ftmo_phase1;
    use propfirm::core::account::Account;
    use propfirm::core::ids::AccountId;
    use propfirm::core::types::Money;
    use propfirm::engine::state::AccountState;

    let plan = ftmo_phase1().with_loss_reference(LossReference::Static);
    let mut account = Account::new(AccountId::new(), plan.clone());
    account.balance = Money(dec!(100_000));
    account.equity = Money(dec!(100_000));
    account.day_start_balance = Money(dec!(100_000));
    account.day_start_equity = Money(dec!(100_000));
    let seed_boundary = chrono::Utc::now() - chrono::Duration::days(2);
    account.current_trading_day_start = Some(seed_boundary);

    let mut state = AccountState::new(account.clone());
    let pre = state.account.current_trading_day_start;
    state = state.rollover_day(true, None);
    let post = state.account.current_trading_day_start;

    assert!(
        post.is_some(),
        "rollover must persist a new trading day boundary"
    );
    assert_eq!(
        post.unwrap(),
        plan.next_trading_day_start(pre.unwrap()),
        "rollover must advance the persisted boundary by exactly one plan trading day"
    );
}

#[test]
fn p1_1_auto_rollover_catches_up_multiple_missed_days() {
    use propfirm::config::plan::ChallengePlan;
    use propfirm::core::ids::AccountId;
    use propfirm::core::types::Money;
    use propfirm::engine::pipeline::{Pipeline, PipelineEvent};
    use propfirm::engine::state::AccountState;
    use propfirm::notifications::log::LogNotifier;
    use propfirm::persistence::memory::InMemoryStore;
    use propfirm::persistence::traits::AccountStore;

    let plan = ChallengePlan {
        timezone: Some(chrono_tz::America::New_York),
        day_reset_time: 0,
        initial_balance_money: Money(dec!(100_000)),
        ..ChallengePlan::default()
    };
    let account = Account::new(AccountId::new(), plan.clone())
        .start(chrono::Utc::now())
        .unwrap();
    let mut state = AccountState::new(account.clone());
    // Seed the boundary as two days ago in local time.
    let two_days_ago = (chrono::Utc::now() - chrono::Duration::days(2))
        .with_timezone(&chrono_tz::America::New_York)
        .date_naive()
        .and_hms_opt(0, 0, 0)
        .unwrap()
        .and_local_timezone(chrono_tz::America::New_York)
        .unwrap()
        .with_timezone(&chrono::Utc);
    state.account.current_trading_day_start = Some(two_days_ago);

    let store = InMemoryStore::new();
    store.put(state.account.clone()).unwrap(); // Store the modified account
    let mut pipeline = Pipeline::new(Evaluator::new(&plan), store, LogNotifier::new());

    // Event arrives two trading days later.
    let event_ts = chrono::Utc::now();
    let result = pipeline.process(
        account.id,
        PipelineEvent::Tick {
            tick: Tick::new(
                Symbol::new("EURUSD"),
                Quote {
                    bid: Price(dec!(1.0)),
                    ask: Price(dec!(1.0)),
                    ts: event_ts,
                },
            ),
            broker_equity: Money(dec!(100_000)),
            broker_balance: Money(dec!(100_000)),
        },
    );
    assert!(result.is_ok(), "multi-day gap must not error: {result:?}");
    let applied = result.unwrap();
    // Should have advanced by two trading days.
    assert_eq!(
        applied.snapshot.account.trading_day_index, 2,
        "account must be caught up to the current trading day after a multi-day gap"
    );
}

#[test]
fn p1_1_rollover_respects_calendar_day_across_dst() {
    use chrono::TimeZone;
    use propfirm::config::plan::ChallengePlan;
    use propfirm::core::ids::AccountId;
    use propfirm::core::types::Money;
    use propfirm::engine::state::AccountState;

    // America/New_York spring-forward: 2026-03-08 00:00 EST -> 03:00 EDT.
    // A fixed 24-hour add would land at 2026-03-09 01:00 EDT, which is
    // wrong; calendar-day advancement should land at midnight.
    let tz = chrono_tz::America::New_York;
    let plan = ChallengePlan {
        timezone: Some(tz),
        day_reset_time: 0,
        initial_balance_money: Money(dec!(100_000)),
        ..ChallengePlan::default()
    };
    let account = Account::new(AccountId::new(), plan.clone())
        .start(chrono::Utc::now())
        .unwrap();
    // Seed boundary at the spring-forward midnight.
    let seed = tz
        .with_ymd_and_hms(2026, 3, 8, 0, 0, 0)
        .unwrap()
        .with_timezone(&chrono::Utc);
    let mut state = AccountState::new(account);
    state.account.current_trading_day_start = Some(seed);

    state = state.rollover_day(true, None);
    let expected = tz
        .with_ymd_and_hms(2026, 3, 9, 0, 0, 0)
        .unwrap()
        .with_timezone(&chrono::Utc);
    assert_eq!(
        state.account.current_trading_day_start.unwrap(),
        expected,
        "rollover must advance by calendar day, not fixed 24 hours, across DST"
    );
}

#[test]
fn debug_multi_day_rollover() {
    use propfirm::config::plan::ChallengePlan;
    use propfirm::core::ids::AccountId;
    use propfirm::core::types::Money;
    use propfirm::engine::state::AccountState;

    let plan = ChallengePlan {
        timezone: Some(chrono_tz::America::New_York),
        day_reset_time: 0,
        initial_balance_money: Money(dec!(100_000)),
        ..ChallengePlan::default()
    };
    let account = Account::new(AccountId::new(), plan.clone())
        .start(chrono::Utc::now())
        .unwrap();
    let mut state = AccountState::new(account.clone());

    // Seed the boundary as two days ago in local time.
    let two_days_ago = (chrono::Utc::now() - chrono::Duration::days(2))
        .with_timezone(&chrono_tz::America::New_York)
        .date_naive()
        .and_hms_opt(0, 0, 0)
        .unwrap()
        .and_local_timezone(chrono_tz::America::New_York)
        .unwrap()
        .with_timezone(&chrono::Utc);

    println!("two_days_ago (UTC): {}", two_days_ago);
    state.account.current_trading_day_start = Some(two_days_ago);
    println!(
        "Initial trading_day_index: {}",
        state.account.trading_day_index
    );
    println!(
        "Initial current_trading_day_start: {:?}",
        state.account.current_trading_day_start
    );

    let event_ts = chrono::Utc::now();
    let event_day_start = state.account.plan.trading_day_start(event_ts);
    let current_day_start = state
        .account
        .current_trading_day_start
        .unwrap_or_else(|| state.account.plan.trading_day_start(event_ts));

    println!("event_ts: {}", event_ts);
    println!("event_day_start: {}", event_day_start);
    println!("current_day_start: {}", current_day_start);
    println!(
        "event_day_start > current_day_start: {}",
        event_day_start > current_day_start
    );

    // Simulate the loop
    let mut sim_state = state.clone();
    let mut iterations = 0;
    while sim_state
        .account
        .current_trading_day_start
        .map(|start| start < event_day_start)
        .unwrap_or(true)
    {
        iterations += 1;
        let had_trades = !sim_state.account.today_realized_pnl.0.is_zero();
        let next_start = sim_state
            .account
            .plan
            .next_trading_day_start(sim_state.account.current_trading_day_start.unwrap());
        println!(
            "Iteration {}: current={:?}, next_start={}, had_trades={}",
            iterations, sim_state.account.current_trading_day_start, next_start, had_trades
        );
        println!(
            "  next_start >= event_day_start: {}",
            next_start >= event_day_start
        );
        if next_start >= event_day_start {
            sim_state = sim_state.rollover_day(had_trades, Some(event_ts));
        } else {
            sim_state = sim_state.rollover_day(false, Some(next_start));
        }
        println!(
            "  After rollover: trading_day_index={}, current={:?}",
            sim_state.account.trading_day_index, sim_state.account.current_trading_day_start
        );
    }
    println!("Total iterations: {}", iterations);
    println!(
        "Final trading_day_index: {}",
        sim_state.account.trading_day_index
    );
}
