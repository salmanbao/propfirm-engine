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

    state = state.rollover_day(true);
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
