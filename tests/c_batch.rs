//! §C.1 / §C.2 — instrument specs, units-vs-lots conversion, and the
//! three plan-cap rules (`max_total_lots`, `trading_hours`, margin).

use chrono::TimeZone;
use propfirm::config::presets::ftmo_phase1;
use propfirm::core::account::Account;
use propfirm::core::ids::AccountId;
use propfirm::core::instrument::{InstrumentRegistry, InstrumentSpec};
use propfirm::core::order::{Order, OrderKind, OrderSide, OrderStatus, OrderType, TimeInForce};
use propfirm::core::position::{Position, PositionSide};
use propfirm::core::types::{dec, Money, Price, Quantity, Symbol};
use propfirm::rules::context::RuleContext;
use propfirm::rules::evaluators::max_position_size::MaxPositionSizeRule;
use propfirm::rules::evaluators::plan_caps::{MarginRule, MaxTotalLotsRule, TradingHoursRule};
use propfirm::rules::traits::Rule;

fn active_account() -> Account {
    let plan = ftmo_phase1();
    let mut acc = Account::new(AccountId::new(), plan)
        .start(chrono::Utc::now())
        .unwrap();
    acc.balance = Money(dec!(100_000));
    acc.equity = Money(dec!(100_000));
    acc.peak_balance = Money(dec!(100_000));
    acc.peak_equity = Money(dec!(100_000));
    acc.day_start_balance = Money(dec!(100_000));
    acc.day_start_equity = Money(dec!(100_000));
    acc
}

fn order(acc: &Account, symbol: &str, units: Decimal) -> Order {
    Order {
        id: propfirm::core::ids::OrderId::new(),
        account_id: acc.id,
        symbol: Symbol::new(symbol),
        side: OrderSide::Buy,
        kind: OrderKind::Open,
        order_type: OrderType::Market,
        quantity: Quantity(units),
        tif: TimeInForce::Gtc,
        stop_loss: None,
        take_profit: None,
        comment: None,
        submitted_at: chrono::Utc::now(),
        status: OrderStatus::Pending,
        filled_quantity: Quantity::ZERO,
        avg_fill_price: None,
    }
}

fn fx_registry() -> InstrumentRegistry {
    let reg = InstrumentRegistry::new();
    reg.register(InstrumentSpec::fx("EURUSD"));
    reg
}

type Decimal = rust_decimal::Decimal;

// ---------------------------------------------------------------------------
// §C.1 — units vs lots in MaxPositionSizeRule
// ---------------------------------------------------------------------------

#[test]
fn c1_100k_contract_one_lot_at_limit_passes_and_one_point_one_fails() {
    // max_lots = 1.0 on an FX pair with a 100,000-unit contract.
    let mut acc = active_account();
    acc.plan.max_position_lots = Some(dec!(1.0));
    let rule = MaxPositionSizeRule::default();
    let mut ctx = RuleContext::new(acc.clone());
    ctx.instruments = fx_registry();

    // Exactly 1.0 lot = 100,000 units → at the limit: not a hard fail,
    // but inside the 80% warning zone → Warn.
    ctx.pending_order = Some(order(&acc, "EURUSD", dec!(100_000)));
    let v = rule.evaluate(&ctx).unwrap();
    assert!(
        matches!(v, propfirm::rules::traits::RuleVerdict::Warn(_)),
        "exactly at the 1.0-lot limit must Warn (≥80% zone), not Fail; got {v:?}"
    );

    // 0.5 lot = 50,000 units → below the warning zone → Pass.
    ctx.pending_order = Some(order(&acc, "EURUSD", dec!(50_000)));
    assert!(matches!(
        rule.evaluate(&ctx).unwrap(),
        propfirm::rules::traits::RuleVerdict::Pass
    ));

    // 1.1 lots = 110,000 units → over the limit, fails.
    ctx.pending_order = Some(order(&acc, "EURUSD", dec!(110_000)));
    let v = rule.evaluate(&ctx).unwrap();
    assert!(
        v.is_fail(),
        "110,000 units (1.1 lots) must fail a 1.0-lot limit; got {v:?}"
    );
}

#[test]
fn c1_10k_contract_point_one_lot_boundary() {
    // max_lots = 0.1 on a 10,000-unit contract (mini contract).
    let mut acc = active_account();
    acc.plan.max_position_lots = Some(dec!(0.1));
    let reg = InstrumentRegistry::new();
    reg.register(InstrumentSpec {
        symbol: Symbol::new("EURUSD"),
        contract_size: dec!(10_000),
        digits: 5,
        pip_size: dec!(0.0001),
    });
    let rule = MaxPositionSizeRule::default();
    let mut ctx = RuleContext::new(acc);
    ctx.instruments = reg;

    // 1,000 units = 0.1 lot → at the limit (warn zone), not a fail.
    let o = order(&ctx.account, "EURUSD", dec!(1_000));
    ctx.pending_order = Some(o);
    let v = rule.evaluate(&ctx).unwrap();
    assert!(!v.is_fail(), "at-limit order must not hard-fail; got {v:?}");

    // 1,100 units = 0.11 lots → fails.
    let o = order(&ctx.account, "EURUSD", dec!(1_100));
    ctx.pending_order = Some(o);
    assert!(rule.evaluate(&ctx).unwrap().is_fail());
}

#[test]
fn c1_old_units_compared_as_lots_would_have_passed_wrongly() {
    // Regression guard for the §C.1 bug: the OLD code compared raw units
    // against max_lots, so an order for 2 units (0.00002 lots on a
    // 100k contract) against a 1.0-lot limit passed even though 2 units
    // is fine — but an order for 500,000 units (5 lots!) ALSO passed the
    // old check against a 50-lot plan default... this test pins that the
    // NEW code fails a 5-lot order on a 1-lot limit regardless of units.
    let mut acc = active_account();
    acc.plan.max_position_lots = Some(dec!(1.0));
    let rule = MaxPositionSizeRule::default();
    let mut ctx = RuleContext::new(acc);
    ctx.instruments = fx_registry();
    // 500,000 units = 5 lots on a 100k contract → must fail.
    ctx.pending_order = Some(order(&ctx.account, "EURUSD", dec!(500_000)));
    assert!(rule.evaluate(&ctx).unwrap().is_fail());
}

// ---------------------------------------------------------------------------
// §C.2 — max_total_lots
// ---------------------------------------------------------------------------

fn open_position(acc: &Account, symbol: &str, units: Decimal) -> Position {
    Position::open(
        acc.id,
        Symbol::new(symbol),
        PositionSide::Long,
        Price(dec!(1.08)),
        Quantity(units),
        chrono::Utc::now(),
        Money::ZERO,
        None,
        None,
        None,
        None,
    )
}

#[test]
fn c2_max_total_lots_aggregates_positions_and_order() {
    let mut acc = active_account();
    acc.plan.max_total_lots = Some(dec!(10.0));
    let rule = MaxTotalLotsRule::default();
    let mut ctx = RuleContext::new(acc.clone());
    ctx.instruments = fx_registry();

    // 9 lots open + 2-lot order (200,000 units) = 11 lots > 10 → fail.
    ctx.open_positions = vec![open_position(&acc, "EURUSD", dec!(900_000))];
    ctx.pending_order = Some(order(&acc, "EURUSD", dec!(200_000)));
    let v = rule.evaluate(&ctx).unwrap();
    assert!(
        v.is_fail(),
        "9 open lots + 2-lot order must fail a 10-lot account cap; got {v:?}"
    );

    // 9 lots open + 0.5-lot order = 9.5 lots ≤ 10 → pass.
    ctx.pending_order = Some(order(&acc, "EURUSD", dec!(50_000)));
    assert!(matches!(
        rule.evaluate(&ctx).unwrap(),
        propfirm::rules::traits::RuleVerdict::Pass
    ));
}

#[test]
fn c2_max_total_lots_pack_override_changes_verdict() {
    let mut acc = active_account();
    acc.plan.max_total_lots = Some(dec!(50.0)); // plan is permissive
    let entry = propfirm::rulepack::RuleEntry {
        id: "max_total_lots".into(),
        kind: "max_total_lots".into(),
        basis: propfirm::rulepack::RuleBasis::Static,
        unit: propfirm::rulepack::RuleUnit::Money, // count semantics
        value: dec!(0.5),                          // pack is strict: 0.5 lots
        tolerance_cents: None,
        early_warning_pct: None,
        priority: 100,
        enabled: true,
        params_json: "{}".into(),
        severity: None,
        failure_policy: None,
    };
    let rule = MaxTotalLotsRule::from_entry(&entry);
    let mut ctx = RuleContext::new(acc);
    ctx.instruments = fx_registry();
    ctx.pending_order = Some(order(&ctx.account, "EURUSD", dec!(100_000))); // 1 lot
    assert!(
        rule.evaluate(&ctx).unwrap().is_fail(),
        "pack value 0.5 lots must fail a 1-lot order despite the permissive plan"
    );
}

// ---------------------------------------------------------------------------
// §C.2 — trading_hours
// ---------------------------------------------------------------------------

#[test]
fn c2_trading_hours_rejects_outside_window() {
    let mut acc = active_account();
    acc.plan.trading_hours = Some((9, 17)); // 09:00–17:00
    let rule = TradingHoursRule;
    let mut ctx = RuleContext::new(acc.clone());
    // 20:00 UTC → outside → fail.
    let at_20 = chrono::Utc.with_ymd_and_hms(2026, 9, 15, 20, 0, 0).unwrap();
    let mut o = order(&acc, "EURUSD", dec!(10_000));
    o.submitted_at = at_20;
    ctx.pending_order = Some(o);
    assert!(rule.evaluate(&ctx).unwrap().is_fail());

    // 10:00 UTC → inside → pass.
    let at_10 = chrono::Utc.with_ymd_and_hms(2026, 9, 15, 10, 0, 0).unwrap();
    let mut o = order(&acc, "EURUSD", dec!(10_000));
    o.submitted_at = at_10;
    ctx.pending_order = Some(o);
    assert!(matches!(
        rule.evaluate(&ctx).unwrap(),
        propfirm::rules::traits::RuleVerdict::Pass
    ));
}

#[test]
fn c2_trading_hours_evaluates_in_plan_timezone() {
    use chrono_tz::Tz;
    let mut acc = active_account();
    acc.plan.trading_hours = Some((9, 17));
    // New York is UTC-4 in September: 20:00 UTC = 16:00 NY (inside).
    acc.plan.timezone = Some("America/New_York".parse::<Tz>().unwrap());
    let rule = TradingHoursRule;
    let mut ctx = RuleContext::new(acc.clone());
    let at_20_utc = chrono::Utc.with_ymd_and_hms(2026, 9, 15, 20, 0, 0).unwrap();
    let mut o = order(&acc, "EURUSD", dec!(10_000));
    o.submitted_at = at_20_utc;
    ctx.pending_order = Some(o);
    assert!(
        matches!(
            rule.evaluate(&ctx).unwrap(),
            propfirm::rules::traits::RuleVerdict::Pass
        ),
        "20:00 UTC = 16:00 New York must be inside a 9-17 window evaluated in plan tz"
    );
}

// ---------------------------------------------------------------------------
// §C.2 — margin
// ---------------------------------------------------------------------------

#[test]
fn c2_margin_insufficient_free_margin_fails_order() {
    // Equity 10k, leverage 1:100 → max new notional 1,000,000
    // (= 10 lots of EURUSD at 1.08 ≈ 1,080,000 notional → just over).
    let acc = active_account();
    let mut ctx = RuleContext::new(acc.clone());
    ctx.instruments = fx_registry();
    ctx.account.equity = Money(dec!(10_000));
    ctx.account.balance = Money(dec!(10_000));
    let rule = MarginRule;

    // 5 lots (500,000 units) at 1.08 → notional 540,000 → margin 5,400
    // ≤ free 10,000 → pass.
    ctx.pending_order = Some({
        let mut o = order(&acc, "EURUSD", dec!(500_000));
        o.avg_fill_price = Some(Price(dec!(1.08)));
        o
    });
    assert!(matches!(
        rule.evaluate(&ctx).unwrap(),
        propfirm::rules::traits::RuleVerdict::Pass
    ));

    // 10 lots at 1.08 → notional 1,080,000 → margin 10,800 > 10,000 → fail.
    ctx.pending_order = Some({
        let mut o = order(&acc, "EURUSD", dec!(1_000_000));
        o.avg_fill_price = Some(Price(dec!(1.08)));
        o
    });
    let v = rule.evaluate(&ctx).unwrap();
    assert!(v.is_fail(), "margin over free margin must fail; got {v:?}");
}

#[test]
fn c2_margin_accounts_for_existing_exposure() {
    // Equity 10k; 5 lots already open (margin used 5,400). A new 5-lot
    // order needs another 5,400 → total 10,800 > 10,000 free-equity → fail.
    let acc = active_account();
    let mut ctx = RuleContext::new(acc.clone());
    ctx.instruments = fx_registry();
    ctx.account.equity = Money(dec!(10_000));
    ctx.account.balance = Money(dec!(10_000));
    ctx.open_positions = vec![open_position(&acc, "EURUSD", dec!(500_000))];
    let mut o = order(&acc, "EURUSD", dec!(500_000));
    o.avg_fill_price = Some(Price(dec!(1.08)));
    ctx.pending_order = Some(o);
    let v = MarginRule.evaluate(&ctx).unwrap();
    assert!(
        v.is_fail(),
        "existing exposure must consume free margin; got {v:?}"
    );
}
