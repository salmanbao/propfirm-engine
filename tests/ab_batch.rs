//! Tests for the §A.2 / §B batch:
//!
//! - cross-account copy-trading detection (wire-in, not deletion);
//! - pack-driven `time_limit` (pack value overrides plan deadline);
//! - pack-driven `grid_trading` (pack value = min entry count);
//! - pack-driven `copy_trading` (pack value = fail threshold);
//! - `TickEstimated` single-quote valuation semantics (documented
//!   limitation after the `equity_after_tick_multi` deletion).

use propfirm::config::presets::ftmo_phase1;
use propfirm::core::account::Account;
use propfirm::core::ids::AccountId;
use propfirm::core::order::{Order, OrderKind, OrderSide, OrderStatus, OrderType, TimeInForce};
use propfirm::core::tick::{Quote, Tick};
use propfirm::core::trade::{Trade, TradeSide};
use propfirm::core::types::{dec, Money, Price, Quantity, Symbol};
use propfirm::persistence::traits::AccountStore;
use propfirm::rulepack::{RuleBasis, RuleEntry, RuleUnit};
use propfirm::rules::context::{RuleContext, RuleContextKind};
use propfirm::rules::evaluators::copy_trading::CopyTradingRule;
use propfirm::rules::evaluators::grid_trading::GridTradingRule;
use propfirm::rules::evaluators::time_limit::TimeLimitRule;
use propfirm::rules::traits::Rule;

type Decimal = rust_decimal::Decimal;

fn active_account() -> Account {
    let plan = ftmo_phase1();
    let mut acc = Account::new(AccountId::new(), plan)
        .start(chrono::Utc::now())
        .unwrap();
    acc.balance = Money(dec!(100_000));
    acc.equity = Money(dec!(100_000));
    acc
}

fn order(acc: &Account, symbol: &str, qty: Decimal) -> Order {
    Order {
        id: propfirm::core::ids::OrderId::new(),
        account_id: acc.id,
        symbol: Symbol::new(symbol),
        side: OrderSide::Buy,
        kind: OrderKind::Open,
        order_type: OrderType::Market,
        quantity: Quantity(qty),
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

fn entry_trade(
    acc: &Account,
    symbol: &str,
    side: OrderSide,
    qty: Decimal,
    at: chrono::DateTime<chrono::Utc>,
) -> Trade {
    Trade::new(
        propfirm::core::ids::OrderId::new(),
        acc.id,
        Symbol::new(symbol),
        side,
        TradeSide::Entry,
        Price(dec!(1.08)),
        Quantity(qty),
        Money::ZERO,
        at,
    )
}

/// Builds a `time_limit` pack entry with the given day count.
fn time_limit_entry(value: Decimal, enabled: bool) -> RuleEntry {
    RuleEntry {
        id: "time_limit".into(),
        kind: "time_limit".into(),
        basis: RuleBasis::Static,
        unit: RuleUnit::Money, // count semantics: absolute number
        value,
        tolerance_cents: None,
        early_warning_pct: None,
        priority: 100,
        enabled,
        params_json: "{}".into(),
        severity: None,
    }
}

// ---------------------------------------------------------------------------
// §B — time_limit pack-driven
// ---------------------------------------------------------------------------

#[test]
fn b_time_limit_pack_value_overrides_plan() {
    // Account started 10 days ago; plan deadline = +30 days from now
    // (not yet expired). Pack entry says 5 days → deadline = start + 5d
    // → expired → Fail.
    let mut acc = active_account();
    acc.started_at = Some(chrono::Utc::now() - chrono::Duration::days(10));
    acc.deadline = Some(chrono::Utc::now() + chrono::Duration::days(30)); // plan-derived; NOT expired

    // Plan-only rule (no pack): passes (deadline in the future).
    let plan_rule = TimeLimitRule::default();
    let ctx = RuleContext::new(acc.clone());
    assert!(matches!(
        plan_rule.evaluate(&ctx).unwrap(),
        propfirm::rules::traits::RuleVerdict::Pass
    ));

    // Pack rule with value = 5 days: expired → Fail.
    let pack_rule = TimeLimitRule::from_entry(&time_limit_entry(dec!(5), true));
    let verdict = pack_rule.evaluate(&ctx).unwrap();
    assert!(
        verdict.is_fail(),
        "pack value of 5 days must expire an account started 10 days ago; got {verdict:?}"
    );
}

#[test]
fn b_time_limit_pack_enabled_false_disables() {
    let mut acc = active_account();
    acc.started_at = Some(chrono::Utc::now() - chrono::Duration::days(40));
    acc.deadline = Some(chrono::Utc::now() - chrono::Duration::days(10)); // expired

    let rule = TimeLimitRule::from_entry(&time_limit_entry(dec!(30), false));
    let ctx = RuleContext::new(acc);
    assert!(
        !rule.is_enabled(&ctx),
        "enabled:false pack entry must disable the rule"
    );
    assert!(matches!(
        rule.evaluate(&ctx).unwrap(),
        propfirm::rules::traits::RuleVerdict::Pass
    ));
}

// ---------------------------------------------------------------------------
// §B — grid_trading pack-driven
// ---------------------------------------------------------------------------

fn grid_rule(value: Decimal, cv: Option<f64>) -> GridTradingRule {
    let params_json = cv
        .map(|c| format!("{{\"cv_threshold\": {c}}}"))
        .unwrap_or_else(|| "{}".into());
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
        params_json,
        severity: None,
    })
}

fn grid_ctx(acc: &Account, prices: &[Decimal]) -> RuleContext {
    let o = order(acc, "EURUSD", dec!(1));
    let mut ctx = RuleContext::for_open_order(acc.clone(), &o);
    ctx.kind = RuleContextKind::OnOrderSubmit;
    let now = chrono::Utc::now();
    ctx.today_trades = prices
        .iter()
        .enumerate()
        .map(|(i, p)| Trade {
            id: propfirm::core::ids::TradeId::new(),
            order_id: propfirm::core::ids::OrderId::new(),
            account_id: acc.id,
            symbol: Symbol::new("EURUSD"),
            side: OrderSide::Buy,
            trade_side: TradeSide::Entry,
            price: Price(*p),
            quantity: Quantity(dec!(1)),
            commission: Money::ZERO,
            swap: Money::ZERO,
            executed_at: now - chrono::Duration::minutes((prices.len() - i) as i64),
            exit_info: None,
            comment: None,
        })
        .collect();
    ctx
}

#[test]
fn b_grid_pack_value_changes_verdict() {
    let mut acc = active_account();
    acc.plan.grid_trading_allowed = false;
    // Four entries at uniform 10-pip spacing (CV = 0) — a grid under any
    // threshold.
    let prices = [dec!(1.0800), dec!(1.0810), dec!(1.0820), dec!(1.0830)];

    // Pack requires 5 entries → 4 uniform entries do NOT fire.
    let strict = grid_rule(dec!(5), None);
    let ctx = grid_ctx(&acc, &prices);
    assert!(matches!(
        strict.evaluate(&ctx).unwrap(),
        propfirm::rules::traits::RuleVerdict::Pass
    ));

    // Pack requires 3 entries → 4 uniform entries DO fire (Warn).
    let loose = grid_rule(dec!(3), None);
    let ctx = grid_ctx(&acc, &prices);
    let verdict = loose.evaluate(&ctx).unwrap();
    assert!(
        matches!(verdict, propfirm::rules::traits::RuleVerdict::Warn(_)),
        "pack value of 3 must fire on 4 uniform entries; got {verdict:?}"
    );
}

#[test]
fn b_grid_irregular_spacing_does_not_fire() {
    let mut acc = active_account();
    acc.plan.grid_trading_allowed = false;
    // Irregular spacing: gaps of 10, 90, 20 pips → CV far above 0.05.
    let prices = [dec!(1.0800), dec!(1.0810), dec!(1.0900), dec!(1.0920)];
    let rule = grid_rule(dec!(3), None);
    let ctx = grid_ctx(&acc, &prices);
    assert!(matches!(
        rule.evaluate(&ctx).unwrap(),
        propfirm::rules::traits::RuleVerdict::Pass
    ));
}

// ---------------------------------------------------------------------------
// §A.2 — cross-account copy-trading detection
// ---------------------------------------------------------------------------

fn copy_ctx(acc: &Account, trade: &Trade, references: &[Trade]) -> RuleContext {
    let mut ctx = RuleContext::for_trade_fill(acc.clone(), trade);
    ctx.kind = RuleContextKind::OnTradeFill;
    ctx.cross_reference_trades = references.to_vec();
    ctx
}

fn other_account_trade(
    symbol: &str,
    side: OrderSide,
    qty: Decimal,
    at: chrono::DateTime<chrono::Utc>,
) -> Trade {
    Trade::new(
        propfirm::core::ids::OrderId::new(),
        AccountId::new(), // a different account
        Symbol::new(symbol),
        side,
        TradeSide::Entry,
        Price(dec!(1.08)),
        Quantity(qty),
        Money::ZERO,
        at,
    )
}

#[test]
fn a2_cross_account_copy_detected() {
    // Other accounts trading the same symbol/side/size within the window:
    // 2 correlations → Warn (suspicious), 3 → Fail (default threshold).
    let mut acc = active_account();
    acc.plan.copy_trading_allowed = false;
    let now = chrono::Utc::now();
    let trade = entry_trade(&acc, "EURUSD", OrderSide::Buy, dec!(1), now);
    let references: Vec<Trade> = (0..2)
        .map(|_| other_account_trade("EURUSD", OrderSide::Buy, dec!(1), now))
        .collect();

    let rule = CopyTradingRule::default();
    let ctx = copy_ctx(&acc, &trade, &references);
    let verdict = rule.evaluate(&ctx).unwrap();
    assert!(
        matches!(verdict, propfirm::rules::traits::RuleVerdict::Warn(_)),
        "2 correlated reference trades must Warn; got {verdict:?}"
    );

    let references3: Vec<Trade> = references
        .iter()
        .cloned()
        .chain(std::iter::once(other_account_trade(
            "EURUSD",
            OrderSide::Buy,
            dec!(1),
            now,
        )))
        .collect();
    let ctx = copy_ctx(&acc, &trade, &references3);
    let verdict = rule.evaluate(&ctx).unwrap();
    assert!(
        verdict.is_fail(),
        "3 correlated reference trades must Fail; got {verdict:?}"
    );
}

#[test]
fn a2_same_account_trades_never_correlate() {
    // The old self-referential rule fired on the account's own fills.
    // The new rule must ignore own-account trades entirely.
    let mut acc = active_account();
    acc.plan.copy_trading_allowed = false;
    let now = chrono::Utc::now();
    let trade = entry_trade(&acc, "EURUSD", OrderSide::Buy, dec!(1), now);
    let own_trades: Vec<Trade> = (0..5)
        .map(|_| entry_trade(&acc, "EURUSD", OrderSide::Buy, dec!(1), now))
        .collect();
    let rule = CopyTradingRule::default();
    let ctx = copy_ctx(&acc, &trade, &own_trades);
    assert!(matches!(
        rule.evaluate(&ctx).unwrap(),
        propfirm::rules::traits::RuleVerdict::Pass
    ));
}

#[test]
fn a2_outside_window_does_not_correlate() {
    let mut acc = active_account();
    acc.plan.copy_trading_allowed = false;
    let now = chrono::Utc::now();
    let trade = entry_trade(&acc, "EURUSD", OrderSide::Buy, dec!(1), now);
    // 30 seconds away — outside the 5s default window.
    let references: Vec<Trade> = (0..4)
        .map(|_| {
            other_account_trade(
                "EURUSD",
                OrderSide::Buy,
                dec!(1),
                now - chrono::Duration::seconds(30),
            )
        })
        .collect();
    let rule = CopyTradingRule::default();
    let ctx = copy_ctx(&acc, &trade, &references);
    assert!(matches!(
        rule.evaluate(&ctx).unwrap(),
        propfirm::rules::traits::RuleVerdict::Pass
    ));
}

#[test]
fn b_copy_pack_value_changes_threshold() {
    // Pack value = 1 → a SINGLE correlated reference trade fails
    // (default threshold 3 would only Warn).
    let mut acc = active_account();
    acc.plan.copy_trading_allowed = false;
    let now = chrono::Utc::now();
    let trade = entry_trade(&acc, "EURUSD", OrderSide::Buy, dec!(1), now);
    let reference = other_account_trade("EURUSD", OrderSide::Buy, dec!(1), now);

    let entry = RuleEntry {
        id: "copy_trading".into(),
        kind: "copy_trading".into(),
        basis: RuleBasis::Static,
        unit: RuleUnit::Money, // count semantics
        value: dec!(1),
        tolerance_cents: None,
        early_warning_pct: None,
        priority: 100,
        enabled: true,
        params_json: "{\"window_seconds\": 10}".into(),
        severity: None,
    };
    let rule = CopyTradingRule::from_entry(&entry);
    let ctx = copy_ctx(&acc, &trade, &[reference]);
    let verdict = rule.evaluate(&ctx).unwrap();
    assert!(
        verdict.is_fail(),
        "pack threshold 1 must Fail on a single correlation; got {verdict:?}"
    );
}

// ---------------------------------------------------------------------------
// §A.2 — TickEstimated single-quote valuation (documented limitation)
// ---------------------------------------------------------------------------

#[test]
fn a2_tick_estimated_values_book_with_ticked_symbol_quote_only() {
    // The estimate path values EVERY open position with the single quote
    // on the tick. A position on a different symbol gets an economically
    // meaningless P&L — this test pins the actual (documented) semantics
    // so a future multi-symbol fix changes the test deliberately.
    use propfirm::engine::evaluator::Evaluator;
    use propfirm::engine::pipeline::{Pipeline, PipelineEvent};
    use propfirm::notifications::log::LogNotifier;
    use propfirm::persistence::memory::InMemoryStore;

    let plan = ftmo_phase1();
    let store = InMemoryStore::new();
    let mut pipeline = Pipeline::new(Evaluator::new(&plan), store.clone(), LogNotifier::new());
    let mut acc = Account::new(AccountId::new(), plan)
        .start(chrono::Utc::now())
        .unwrap();
    acc.tenant_id = propfirm::tenant::TenantId::named("t");
    acc.balance = Money(dec!(100_000));
    acc.equity = Money(dec!(100_000));
    store.put(acc.clone()).unwrap();

    // Open position on GBPUSD; tick arrives on EURUSD.
    let position = propfirm::core::position::Position::open(
        acc.id,
        Symbol::new("GBPUSD"),
        propfirm::core::position::PositionSide::Long,
        Price(dec!(1.26)),
        Quantity(dec!(100_000)),
        chrono::Utc::now(),
        Money::ZERO,
        None,
        None,
        None,
        None,
    );
    store.add_position(position).unwrap();

    let tick = Tick::new(
        Symbol::new("EURUSD"),
        Quote {
            bid: Price(dec!(1.08)),
            ask: Price(dec!(1.0802)),
            ts: chrono::Utc::now(),
        },
    );
    let result = pipeline
        .process_for_tenant(acc.tenant_id, acc.id, PipelineEvent::TickEstimated { tick })
        .unwrap();
    // With contract_size 1 and the EURUSD quote applied to a GBPUSD
    // position: equity = balance + (1.08 - 1.26) × 100_000 = 82_000.
    // This pins the single-quote semantics (documented limitation).
    assert_eq!(
        result.snapshot.account.equity.0,
        dec!(82_000),
        "TickEstimated must value the whole book with the ticked symbol's quote (documented limitation)"
    );
}
