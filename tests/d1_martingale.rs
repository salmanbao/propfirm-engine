//! §D.1 — martingale lot-escalation and strengthened grid detection.

use chrono::TimeZone;
use propfirm::config::presets::ftmo_phase1;
use propfirm::core::account::Account;
use propfirm::core::ids::AccountId;
use propfirm::core::order::{Order, OrderKind, OrderSide, OrderStatus, OrderType, TimeInForce};
use propfirm::core::trade::{Trade, TradeSide};
use propfirm::core::types::{dec, Money, Price, Quantity, Symbol};
use propfirm::rulepack::{RuleBasis, RuleEntry, RuleUnit};
use propfirm::rules::context::{RuleContext, RuleContextKind};
use propfirm::rules::evaluators::grid_trading::GridTradingRule;
use propfirm::rules::traits::Rule;

type Decimal = rust_decimal::Decimal;

fn active_account() -> Account {
    let plan = ftmo_phase1();
    let mut acc = Account::new(AccountId::new(), plan)
        .start(chrono::Utc::now())
        .unwrap();
    acc.balance = Money(dec!(100_000));
    acc.equity = Money(dec!(100_000));
    acc.plan.grid_trading_allowed = false;
    acc
}

fn pending_order(acc: &Account, units: Decimal) -> Order {
    Order {
        id: propfirm::core::ids::OrderId::new(),
        account_id: acc.id,
        symbol: Symbol::new("EURUSD"),
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

fn entry(
    acc: &Account,
    units: Decimal,
    price: Decimal,
    at: chrono::DateTime<chrono::Utc>,
) -> Trade {
    Trade::new(
        propfirm::core::ids::OrderId::new(),
        acc.id,
        Symbol::new("EURUSD"),
        OrderSide::Buy,
        TradeSide::Entry,
        Price(price),
        Quantity(units),
        Money::ZERO,
        at,
    )
}

fn losing_exit(
    acc: &Account,
    units: Decimal,
    loss: Decimal,
    at: chrono::DateTime<chrono::Utc>,
) -> Trade {
    let mut t = Trade::new(
        propfirm::core::ids::OrderId::new(),
        acc.id,
        Symbol::new("EURUSD"),
        OrderSide::Sell,
        TradeSide::Exit,
        Price(dec!(1.07)),
        Quantity(units),
        Money::ZERO,
        at,
    );
    t.exit_info = Some(propfirm::core::trade::TradeExit {
        position_id: propfirm::core::ids::PositionId::new(),
        realized_pnl: Money(-loss),
        closed_quantity: Quantity(units),
        entry_price: Price(dec!(1.08)),
        exit_price: Price(dec!(1.07)),
    });
    t
}

fn rule_with(value: Decimal, params_json: &str) -> GridTradingRule {
    GridTradingRule::from_entry(&RuleEntry {
        id: "grid_trading".into(),
        kind: "grid_trading".into(),
        basis: RuleBasis::Static,
        unit: RuleUnit::Money, // count semantics
        value,
        tolerance_cents: None,
        early_warning_pct: None,
        priority: 100,
        enabled: true,
        params_json: params_json.to_string(),
        severity: None,
        failure_policy: None,
    })
}

fn ctx_with_trades(acc: &Account, trades: Vec<Trade>, units: Decimal) -> RuleContext {
    let o = pending_order(acc, units);
    let mut ctx = RuleContext::for_open_order(acc.clone(), &o);
    ctx.kind = RuleContextKind::OnOrderSubmit;
    ctx.today_trades = trades;
    ctx
}

/// Builds `losses` losing exits of `units` each, then an escalated entry.
fn escalating_sequence(acc: &Account, losses: usize, units: Decimal) -> (Vec<Trade>, Decimal) {
    let mut trades = Vec::new();
    let mut t = chrono::Utc.with_ymd_and_hms(2026, 9, 15, 9, 0, 0).unwrap();
    for _ in 0..losses {
        trades.push(losing_exit(acc, units, dec!(100), t));
        t += chrono::Duration::minutes(5);
    }
    // Escalated entry: 2× the losing size.
    let escalated = units * dec!(2);
    trades.push(entry(acc, escalated, dec!(1.08), t));
    (trades, escalated)
}

#[test]
fn d1_escalating_size_after_losses_fires() {
    // 2 consecutive losing 1-lot exits, then a 2-lot entry (2× ≥ 1.5×
    // default multiplier) → martingale detection must fire.
    let acc = active_account();
    let (trades, escalated) = escalating_sequence(&acc, 2, dec!(100_000));
    assert!(
        escalated > dec!(100_000) * dec!(1.5),
        "test setup: 200k units must exceed 1.5× the 100k first-loss size"
    );
    let rule = rule_with(dec!(3), "{}");
    let ctx = ctx_with_trades(&acc, trades, dec!(200_000));
    let v = rule.evaluate(&ctx).unwrap();
    assert!(
        matches!(v, propfirm::rules::traits::RuleVerdict::Warn(_)),
        "escalated entry after 2 losses must fire the martingale check; got {v:?}"
    );
}

#[test]
fn d1_uniform_size_sequence_does_not_fire() {
    // 2 consecutive losing exits of 1 lot, then a NEW entry of the SAME
    // size (1× < 1.5× multiplier) → no martingale; spacing irregular →
    // no grid either → Pass.
    let acc = active_account();
    let mut t = chrono::Utc.with_ymd_and_hms(2026, 9, 15, 9, 0, 0).unwrap();
    let mut trades = Vec::new();
    trades.push(losing_exit(&acc, dec!(100_000), dec!(100), t));
    t += chrono::Duration::minutes(7);
    trades.push(losing_exit(&acc, dec!(100_000), dec!(100), t));
    t += chrono::Duration::minutes(13);
    trades.push(entry(&acc, dec!(100_000), dec!(1.08), t));
    let rule = rule_with(dec!(3), "{}");
    let ctx = ctx_with_trades(&acc, trades, dec!(100_000));
    assert!(matches!(
        rule.evaluate(&ctx).unwrap(),
        propfirm::rules::traits::RuleVerdict::Pass
    ));
}

#[test]
fn d1_irregular_spacing_does_not_fire_grid() {
    // Uniform-ish spacing was already covered (ab_batch); here: wildly
    // irregular spacing must NOT fire the grid check. Prices with gaps of
    // 10, 90, 20 pips → CV >> threshold. No losses → no martingale.
    let acc = active_account();
    let mut t = chrono::Utc.with_ymd_and_hms(2026, 9, 15, 9, 0, 0).unwrap();
    let prices = [dec!(1.0800), dec!(1.0810), dec!(1.0900), dec!(1.0920)];
    let trades: Vec<Trade> = prices
        .iter()
        .map(|p| {
            let trade = entry(&acc, dec!(100_000), *p, t);
            t += chrono::Duration::minutes(5);
            trade
        })
        .collect();
    let rule = rule_with(dec!(3), "{}");
    let ctx = ctx_with_trades(&acc, trades, dec!(100_000));
    assert!(matches!(
        rule.evaluate(&ctx).unwrap(),
        propfirm::rules::traits::RuleVerdict::Pass
    ));
}

#[test]
fn d1_old_entries_outside_window_do_not_form_grid() {
    // Four uniform entries BUT the oldest is 10 hours back — outside the
    // default 120-minute window → not an active grid → Pass.
    let acc = active_account();
    let now = chrono::Utc::now();
    let prices = [dec!(1.0800), dec!(1.0810), dec!(1.0820), dec!(1.0830)];
    let trades: Vec<Trade> = prices
        .iter()
        .enumerate()
        .map(|(i, p)| {
            entry(
                &acc,
                dec!(100_000),
                *p,
                now - chrono::Duration::hours(10) + chrono::Duration::minutes(i as i64),
            )
        })
        .collect();
    let rule = rule_with(dec!(3), "{}");
    let ctx = ctx_with_trades(&acc, trades, dec!(100_000));
    // The escalated entry (100k) equals the first-loss size... there are
    // no losses at all here, so only the grid check could fire — it must
    // not, because the entries are stale.
    assert!(matches!(
        rule.evaluate(&ctx).unwrap(),
        propfirm::rules::traits::RuleVerdict::Pass
    ));
}

#[test]
fn d1_severity_configurable_hard() {
    // params_json severity_hard=1 → Hard severity violation on detection.
    let acc = active_account();
    let (trades, _) = escalating_sequence(&acc, 2, dec!(100_000));
    let rule = rule_with(dec!(3), "{\"severity_hard\": 1}");
    let ctx = ctx_with_trades(&acc, trades, dec!(200_000));
    let v = rule.evaluate(&ctx).unwrap();
    match v {
        propfirm::rules::traits::RuleVerdict::Warn(v) => {
            assert_eq!(
                v.severity,
                propfirm::core::violation::ViolationSeverity::Hard,
                "severity_hard=1 must produce a Hard-severity violation"
            );
        }
        other => panic!("expected Warn verdict with Hard severity; got {other:?}"),
    }
}

#[test]
fn d1_multiplier_pack_knob_honoured() {
    // escalation_multiplier=3 → the 2× escalated entry is BELOW the
    // threshold → no fire.
    let acc = active_account();
    let (trades, _) = escalating_sequence(&acc, 2, dec!(100_000));
    let rule = rule_with(dec!(3), "{\"escalation_multiplier\": 3}");
    let ctx = ctx_with_trades(&acc, trades, dec!(200_000));
    assert!(matches!(
        rule.evaluate(&ctx).unwrap(),
        propfirm::rules::traits::RuleVerdict::Pass
    ));
}

#[test]
fn d1_losses_count_pack_knob_honoured() {
    // escalation_losses=3 → only 2 losses present → no fire.
    let acc = active_account();
    let (trades, _) = escalating_sequence(&acc, 2, dec!(100_000));
    let rule = rule_with(dec!(3), "{\"escalation_losses\": 3}");
    let ctx = ctx_with_trades(&acc, trades, dec!(200_000));
    assert!(matches!(
        rule.evaluate(&ctx).unwrap(),
        propfirm::rules::traits::RuleVerdict::Pass
    ));
}
