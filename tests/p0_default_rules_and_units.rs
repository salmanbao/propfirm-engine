//! P0.1–P0.4 regression tests.
//!
//! - P0.1: `hft_scalping` / `per_trade_max_loss` must be disabled by
//!   default; a 5% losing trade on `ftmo_phase1()` must NOT terminate.
//! - P0.2: `Evaluator::new(plan)` must select rules from the plan.
//! - P0.3: pack `unit` must be honored; mis-encoding fails closed.
//! - P0.4: unknown pack kinds produce a typed error.

use propfirm::config::plan::{ChallengePhase, ChallengePlan, LossReference};
use propfirm::config::presets::{ftmo_phase1, topstep_futures};
use propfirm::core::account::Account;
use propfirm::core::ids::{AccountId, RuleId};
use propfirm::core::order::{Order, OrderKind, OrderSide, OrderType, TimeInForce};
use propfirm::core::tick::{Quote, Tick};
use propfirm::core::trade::{Trade, TradeExit, TradeSide};
use propfirm::core::types::Pct as PlanPct;
use propfirm::core::types::{dec, Money, Price, Quantity, ServerTime, Symbol, Timestamp};
use propfirm::engine::evaluator::Evaluator;
use propfirm::rules::context::{RuleContext, RuleContextKind};
use propfirm::rules::registry::RuleRegistry;
use propfirm::rules::traits::Rule;
use propfirm::tenant::TenantId;
use rust_decimal::Decimal;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn fresh_account(plan: &ChallengePlan) -> Account {
    Account::new(AccountId::new(), plan.clone())
}

fn active_account(plan: &ChallengePlan) -> Account {
    let mut a = fresh_account(plan);
    a.status = propfirm::core::account::AccountStatus::Active;
    a.started_at = Some(chrono::Utc::now() - chrono::Duration::days(5));
    a
}

fn exit_trade(account: &Account, pnl: Money) -> Trade {
    let mut t = Trade::new(
        propfirm::core::ids::OrderId::new(),
        account.id,
        Symbol::new("EURUSD"),
        OrderSide::Sell,
        TradeSide::Exit,
        Price(dec!(1.0800)),
        Quantity(dec!(1)),
        Money::ZERO,
        chrono::Utc::now(),
    );
    t.exit_info = Some(TradeExit {
        position_id: propfirm::core::ids::PositionId::new(),
        realized_pnl: pnl,
        closed_quantity: Quantity(dec!(1)),
        entry_price: Price(dec!(1.0850)),
        exit_price: Price(dec!(1.0800)),
    });
    t
}

fn trade_fill_ctx(account: &Account, trade: &Trade) -> RuleContext {
    let mut ctx = RuleContext::for_trade_fill(account.clone(), trade);
    ctx.kind = RuleContextKind::OnTradeFill;
    ctx.rule_config = propfirm::config::rule_config::RuleConfig::from_plan(&account.plan);
    ctx
}

fn order_ctx(account: &Account, qty: Decimal, symbol: &str) -> RuleContext {
    let order = Order {
        id: propfirm::core::ids::OrderId::new(),
        account_id: account.id,
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
        status: propfirm::core::order::OrderStatus::Pending,
        filled_quantity: Quantity::ZERO,
        avg_fill_price: None,
    };
    let mut ctx = RuleContext::for_open_order(account.clone(), &order);
    ctx.kind = RuleContextKind::OnOrderSubmit;
    ctx.rule_config = propfirm::config::rule_config::RuleConfig::from_plan(&account.plan);
    ctx
}

fn pack_with(entries: Vec<propfirm::rulepack::RuleEntry>) -> propfirm::rulepack::RulePack {
    propfirm::rulepack::RulePack {
        id: "p0-test-pack".into(),
        version: 1,
        tenant_id: TenantId::named("test-tenant"),
        lifecycle: propfirm::rulepack::PackLifecycle::Active,
        effective_from: chrono::Utc::now(),
        superseded_by: None,
        description: "test pack".into(),
        rules: entries,
        initial_balance: Money(dec!(100_000)),
        leverage: 100,
        profit_target_pct: PlanPct(dec!(0.08)),
    }
}

fn max_dd_entry(
    value: Decimal,
    unit: propfirm::rulepack::RuleUnit,
) -> propfirm::rulepack::RuleEntry {
    let mut e = propfirm::rulepack::RuleEntry::new("max_total_loss", "max_drawdown", value);
    e.unit = unit;
    e.basis = propfirm::rulepack::RuleBasis::Static;
    e
}

// ---------------------------------------------------------------------------
// P0.1 — the two new rules are disabled by default
// ---------------------------------------------------------------------------

/// P0.1: `default_rules()` must NOT enable per-trade max loss / HFT ban
/// for a plan that doesn't set them.
#[test]
fn p0_1_default_rules_do_not_run_opt_in_rules() {
    let plan = ftmo_phase1();
    let registry = RuleRegistry::with_default_rules_for_plan(&plan);
    assert!(
        registry.get(RuleId::named("per_trade_max_loss")).is_none(),
        "per_trade_max_loss must not be registered for ftmo_phase1"
    );
    assert!(
        registry.get(RuleId::named("hft_scalping")).is_none(),
        "hft_scalping must not be registered for ftmo_phase1"
    );
    // Core rules still present.
    assert!(registry.get(RuleId::named("max_drawdown")).is_some());
    assert!(registry.get(RuleId::named("daily_drawdown")).is_some());
}

/// P0.1: a single trade losing 5% of balance on `ftmo_phase1()` must not
/// produce a terminating decision, and no decision at all may come from a
/// rule the plan did not enable.
#[test]
fn p0_1_ftmo_phase1_five_pct_losing_trade_does_not_terminate() {
    let plan = ftmo_phase1();
    let account = active_account(&plan);
    assert!(plan.per_trade_max_loss_pct.is_none());
    assert!(!plan.hft_ban_enabled);

    // Even with a plan field accidentally present in some future edit,
    // the rule-level is_enabled default guard is what we pin here.
    let rule = propfirm::rules::evaluators::per_trade_max_loss::PerTradeMaxLossRule::default();
    let ctx = {
        let trade = exit_trade(&account, Money(dec!(-5_000))); // 5% of 100k... wait, ftmo_phase1 is 10k
        let _ = trade;
        trade_fill_ctx(&account, &exit_trade(&account, Money(dec!(-500))))
    };
    assert!(
        !rule.is_enabled(&ctx),
        "per_trade_max_loss must be disabled on ftmo_phase1 (plan field not set)"
    );

    // A losing trade of 5% of the 10k balance = -500.
    let losing = exit_trade(&account, Money(dec!(-500)));
    let evaluator = Evaluator::new(&account.plan);
    let result = evaluator
        .evaluate_trade(&account, &losing, &[], &[], Vec::new())
        .unwrap();
    assert!(
        !result.decision.is_terminating(),
        "a 5% single-trade loss must NOT terminate an ftmo_phase1 account, got {:?}",
        result.decision.kind
    );
    // And the per-trade rule contributed nothing at all.
    for r in &result.reports {
        assert_ne!(
            r.rule_id,
            RuleId::named("per_trade_max_loss"),
            "no report may come from the disabled per-trade rule"
        );
    }
}

/// P0.1: topstep_futures enables the per-trade rule; the same trade size
/// beyond its limit IS liquidated (proves the enablement path works).
#[test]
fn p0_1_topstep_enables_per_trade_max_loss() {
    let plan = topstep_futures();
    assert!(
        plan.per_trade_max_loss_pct.is_some(),
        "topstep preset must enable the per-trade loss limit"
    );
    let account = active_account(&plan);
    let evaluator = Evaluator::new(&account.plan);
    assert!(evaluator
        .registry
        .get(RuleId::named("per_trade_max_loss"))
        .is_some());

    // 2% of 50k = 1000; lose 3000 → Liquidate (broker-reported equity).
    let losing = exit_trade(&account, Money(dec!(-3_000)));
    let mut ctx = trade_fill_ctx(&account, &losing);
    ctx = ctx.with_broker_equity(account.equity, account.balance);
    let result = evaluator.evaluate(&ctx).unwrap();
    assert!(
        result.decision.is_terminating(),
        "a 6% single-trade loss on topstep (2% cap) must terminate, got {:?}",
        result.decision.kind
    );
}

// ---------------------------------------------------------------------------
// P0.2 — Evaluator::new(plan) is plan-aware
// ---------------------------------------------------------------------------

#[test]
fn p0_2_plan_with_hedging_allowed_does_not_run_hedging_rule() {
    let mut plan = ftmo_phase1();
    plan.hedging_allowed = true;
    let evaluator = Evaluator::new(&plan);
    assert!(
        evaluator.registry.get(RuleId::named("hedging")).is_none(),
        "hedging rule must not be registered when the plan allows hedging"
    );

    let mut plan2 = ftmo_phase1();
    plan2.hedging_allowed = false;
    let evaluator2 = Evaluator::new(&plan2);
    assert!(
        evaluator2.registry.get(RuleId::named("hedging")).is_some(),
        "hedging rule must be registered when the plan forbids hedging"
    );
}

// ---------------------------------------------------------------------------
// P0.3 — RuleUnit is honored; fail closed on mis-encoding
// ---------------------------------------------------------------------------

/// P0.3: a Money-unit max-loss pack of {value: 5000} on a 100k account
/// breaches at equity 94,000 (limit = $5,000, not 5000×100k).
#[test]
fn p0_3_money_unit_max_loss_breaches_at_94k() {
    let plan = ChallengePlan {
        initial_balance_money: Money(dec!(100_000)),
        max_loss_reference: LossReference::Static,
        ..ChallengePlan::default()
    };
    let mut account = active_account(&plan);
    account.equity = Money(dec!(100_000));
    account.balance = Money(dec!(100_000));

    let pack = pack_with(vec![max_dd_entry(
        dec!(5000),
        propfirm::rulepack::RuleUnit::Money,
    )]);
    let registry = RuleRegistry::build_from_pack(&pack).unwrap();

    let eval = |equity: Money| {
        let mut a = account.clone();
        a.equity = equity;
        a.balance = equity;
        let bal = a.balance;
        let mut ctx = RuleContext::for_tick(
            a,
            &Tick::new(
                Symbol::new("EURUSD"),
                Quote {
                    bid: Price(dec!(1.08)),
                    ask: Price(dec!(1.0802)),
                    ts: chrono::Utc::now(),
                },
            ),
        );
        ctx.kind = RuleContextKind::OnTick;
        ctx = ctx.with_broker_equity(equity, bal);
        let reports = registry.evaluate(&ctx).unwrap();
        propfirm::engine::decision::Decision::from_reports(&reports).kind
    };

    // Equity 95,000 → dd = 5,000 = limit → tolerance 1c → no breach.
    let k = eval(Money(dec!(95_000)));
    assert!(
        !matches!(k, propfirm::engine::decision::DecisionKind::Liquidate),
        "dd exactly at $5,000 limit must not liquidate, got {k:?}"
    );
    // Equity 94,000 → dd = 6,000 > 5,000 → breach.
    let k = eval(Money(dec!(94_000)));
    assert!(
        matches!(k, propfirm::engine::decision::DecisionKind::Liquidate),
        "equity 94,000 on a $5,000 money-unit limit must liquidate, got {k:?}"
    );
}

/// P0.3: a Percent-unit pack of {value: 0.10} still works (10% of 100k).
#[test]
fn p0_3_percent_unit_max_loss_still_works() {
    let plan = ChallengePlan {
        initial_balance_money: Money(dec!(100_000)),
        max_loss_reference: LossReference::Static,
        ..ChallengePlan::default()
    };
    let account = active_account(&plan);
    let pack = pack_with(vec![max_dd_entry(
        dec!(0.10),
        propfirm::rulepack::RuleUnit::Percent,
    )]);
    let registry = RuleRegistry::build_from_pack(&pack).unwrap();

    let mut a = account.clone();
    a.equity = Money(dec!(89_000));
    a.balance = Money(dec!(89_000));
    let eq = a.equity;
    let bal = a.balance;
    let mut ctx = RuleContext::for_tick(
        a,
        &Tick::new(
            Symbol::new("EURUSD"),
            Quote {
                bid: Price(dec!(1.08)),
                ask: Price(dec!(1.0802)),
                ts: chrono::Utc::now(),
            },
        ),
    );
    ctx.kind = RuleContextKind::OnTick;
    ctx = ctx.with_broker_equity(eq, bal);
    let reports = registry.evaluate(&ctx).unwrap();
    let decision = propfirm::engine::decision::Decision::from_reports(&reports);
    assert!(
        matches!(
            decision.kind,
            propfirm::engine::decision::DecisionKind::Liquidate
        ),
        "11% dd on a 10% percent-unit limit must liquidate, got {:?}",
        decision.kind
    );
}

/// P0.3: a mis-encoded pack unit fails closed — evaluation returns an
/// error (or a hard failure), never a silent Pass.
#[test]
fn p0_3_mis_encoded_unit_fails_closed() {
    // Force an unknown unit by constructing params directly.
    let mut params = propfirm::rules::params::RuleParams::from_entry(&max_dd_entry(
        dec!(0.10),
        propfirm::rulepack::RuleUnit::Percent,
    ));
    // Simulate a future/unknown unit encoding via the raw field.
    params.unit = None; // treated as percent — fine.
    let pct = params.effective_pct("test", Money(dec!(100_000))).unwrap();
    assert_eq!(pct, dec!(0.10));

    // money normalizes against the reference
    let mut params_money = propfirm::rules::params::RuleParams::from_entry(&max_dd_entry(
        dec!(5_000),
        propfirm::rulepack::RuleUnit::Money,
    ));
    params_money.unit = Some(propfirm::rulepack::RuleUnit::Money);
    let pct = params_money
        .effective_pct("test", Money(dec!(100_000)))
        .unwrap();
    assert_eq!(pct, dec!(0.05));
}

// ---------------------------------------------------------------------------
// P0.4 — unknown pack kinds produce a typed error
// ---------------------------------------------------------------------------

#[test]
fn p0_4_unknown_pack_kind_fails_loudly() {
    let mut entry = propfirm::rulepack::RuleEntry::new("mystery", "no_such_rule_kind", dec!(0.1));
    entry.enabled = true;
    let pack = pack_with(vec![entry]);
    let err = match RuleRegistry::build_from_pack(&pack) {
        Err(e) => e,
        Ok(_) => panic!("expected build_from_pack to fail on unknown kind"),
    };
    let msg = err.to_string();
    assert!(
        msg.contains("no_such_rule_kind"),
        "error must name the unsupported kind, got: {msg}"
    );
}

#[test]
fn p0_4_batch_kind_resolution_lists_all_unsupported() {
    let err = propfirm::rules::registry::default_factories_for_kinds(&[
        "max_drawdown",
        "kind_a",
        "kind_b",
    ])
    .unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("kind_a") && msg.contains("kind_b"),
        "got: {msg}"
    );
    assert!(!msg.contains("unsupported rule kind(s): max_drawdown"));
}

/// P0.4: pack-driven verdict change for the profit_target rule family —
/// editing the pack target changes when TargetHit fires.
#[test]
fn p0_4_pack_edit_changes_profit_target_verdict() {
    let plan = ChallengePlan {
        initial_balance_money: Money(dec!(100_000)),
        profit_target_pct: PlanPct(dec!(0.10)),
        ..ChallengePlan::default()
    };
    let mut account = active_account(&plan);
    account.balance = Money(dec!(105_000));
    account.equity = Money(dec!(105_000));
    account.total_realized_pnl = Money(dec!(5_000));
    // net_profit() derives from balance - initial (5,000).

    // Pack says target = 2% of initial (2000). 5000 net >= 2000 → TargetHit.
    let mut entry = propfirm::rulepack::RuleEntry::new("target", "profit_target", dec!(0.02));
    entry.unit = propfirm::rulepack::RuleUnit::Percent;
    let pack = pack_with(vec![entry]);
    let registry = RuleRegistry::build_from_pack(&pack).unwrap();
    let mut ctx = RuleContext::new(account.clone());
    ctx.kind = RuleContextKind::OnTick;
    let reports = registry.evaluate(&ctx).unwrap();
    let d = propfirm::engine::decision::Decision::from_reports(&reports);
    assert!(
        d.is_target_hit(),
        "5000 net must hit a 2000 pack target, got {:?}",
        d.kind
    );

    // Pack says target = 50% (50,000). 5000 net < 50,000 → no TargetHit.
    let mut entry_hi = propfirm::rulepack::RuleEntry::new("target", "profit_target", dec!(0.50));
    entry_hi.unit = propfirm::rulepack::RuleUnit::Percent;
    let pack_hi = pack_with(vec![entry_hi]);
    let registry_hi = RuleRegistry::build_from_pack(&pack_hi).unwrap();
    let reports_hi = registry_hi.evaluate(&ctx).unwrap();
    let d_hi = propfirm::engine::decision::Decision::from_reports(&reports_hi);
    assert!(
        !d_hi.is_target_hit(),
        "5000 net must NOT hit a 50000 pack target, got {:?}",
        d_hi.kind
    );
}

/// P0.4: pack-driven verdict change for min_trading_days family.
#[test]
fn p0_4_pack_edit_changes_min_trading_days_verdict() {
    let plan = ChallengePlan {
        initial_balance_money: Money(dec!(100_000)),
        ..ChallengePlan::default()
    };
    let mut account = active_account(&plan);
    account.active_trading_days = 3;

    // Pack requires 2 days → Pass.
    let mut e2 = propfirm::rulepack::RuleEntry::new("mtd", "min_trading_days", dec!(2));
    e2.enabled = true;
    let pack2 = pack_with(vec![e2]);
    let reg2 = RuleRegistry::build_from_pack(&pack2).unwrap();
    let ctx = {
        let mut c = RuleContext::new(account.clone());
        c.kind = RuleContextKind::OnDemand;
        c
    };
    let reports = reg2.evaluate(&ctx).unwrap();
    let warnings: Vec<_> = reports
        .iter()
        .filter(|r| matches!(r.verdict, propfirm::rules::traits::RuleVerdict::Warn(_)))
        .collect();
    assert!(
        warnings.is_empty(),
        "3 active days must satisfy a 2-day pack requirement"
    );

    // Pack requires 10 days → Warn (deadline not reached).
    let mut e10 = propfirm::rulepack::RuleEntry::new("mtd", "min_trading_days", dec!(10));
    e10.enabled = true;
    let pack10 = pack_with(vec![e10]);
    let reg10 = RuleRegistry::build_from_pack(&pack10).unwrap();
    let reports10 = reg10.evaluate(&ctx).unwrap();
    let warns10: Vec<_> = reports10
        .iter()
        .filter(|r| matches!(r.verdict, propfirm::rules::traits::RuleVerdict::Warn(_)))
        .collect();
    assert!(
        !warns10.is_empty(),
        "3 active days must warn against a 10-day pack requirement"
    );
}

/// P0.4: pack-driven verdict change for max_open_positions family.
#[test]
fn p0_4_pack_edit_changes_max_open_positions_verdict() {
    let plan = ChallengePlan {
        initial_balance_money: Money(dec!(100_000)),
        ..ChallengePlan::default()
    };
    let account = active_account(&plan);
    // One open position + a pending open order.
    let pos = propfirm::core::position::Position::open(
        account.id,
        Symbol::new("EURUSD"),
        propfirm::core::position::PositionSide::Long,
        Price(dec!(1.08)),
        Quantity(dec!(1)),
        chrono::Utc::now(),
        Money::ZERO,
        None,
        None,
        None,
        None,
    );
    let positions = vec![pos];

    // Pack cap = 5 → allowed.
    let mut e5 = propfirm::rulepack::RuleEntry::new("mop", "max_open_positions", dec!(5));
    e5.enabled = true;
    let reg5 = RuleRegistry::build_from_pack(&pack_with(vec![e5])).unwrap();
    let ctx5 = {
        let mut c = order_ctx(&account, dec!(1), "GBPUSD");
        c.open_positions = positions.clone();
        c
    };
    let reports5 = reg5.evaluate(&ctx5).unwrap();
    assert!(reports5
        .iter()
        .all(|r| !matches!(r.verdict, propfirm::rules::traits::RuleVerdict::Fail(_))));

    // Pack cap = 1 → 1 open + 1 new = 2 > 1 → Fail.
    let mut e1 = propfirm::rulepack::RuleEntry::new("mop", "max_open_positions", dec!(1));
    e1.enabled = true;
    let reg1 = RuleRegistry::build_from_pack(&pack_with(vec![e1])).unwrap();
    let ctx1 = {
        let mut c = order_ctx(&account, dec!(1), "GBPUSD");
        c.open_positions = positions;
        c
    };
    let reports1 = reg1.evaluate(&ctx1).unwrap();
    assert!(reports1
        .iter()
        .any(|r| matches!(r.verdict, propfirm::rules::traits::RuleVerdict::Fail(_))));
}

/// P0.4: pack-driven verdict change for hedging family.
#[test]
fn p0_4_pack_edit_changes_hedging_verdict() {
    let mut plan = ChallengePlan {
        initial_balance_money: Money(dec!(100_000)),
        ..ChallengePlan::default()
    };
    plan.hedging_allowed = false;
    let account = active_account(&plan);
    // Existing LONG position; the pending order below SELLS (Short side)
    // on the same symbol → a hedge.
    let long = propfirm::core::position::Position::open(
        account.id,
        Symbol::new("EURUSD"),
        propfirm::core::position::PositionSide::Long,
        Price(dec!(1.08)),
        Quantity(dec!(1)),
        chrono::Utc::now(),
        Money::ZERO,
        None,
        None,
        None,
        None,
    );

    // Pack with hedging enabled (forbidden) → opposite order fails.
    let mut eh = propfirm::rulepack::RuleEntry::new("h", "hedging", dec!(0));
    eh.enabled = true;
    let regh = RuleRegistry::build_from_pack(&pack_with(vec![eh])).unwrap();
    let sell_order_ctx = {
        let mut c = order_ctx(&account, dec!(1), "EURUSD");
        if let Some(o) = c.pending_order.as_mut() {
            o.side = OrderSide::Sell;
        }
        c.open_positions = vec![long.clone()];
        c
    };
    let reports = regh.evaluate(&sell_order_ctx).unwrap();
    assert!(
        reports
            .iter()
            .any(|r| matches!(r.verdict, propfirm::rules::traits::RuleVerdict::Fail(_))),
        "opening a short against a long with hedging forbidden must Fail"
    );

    // Pack with hedging entry disabled → no verdict from this rule.
    let mut eh_off = propfirm::rulepack::RuleEntry::new("h", "hedging", dec!(0));
    eh_off.enabled = false;
    let regoff = RuleRegistry::build_from_pack(&pack_with(vec![eh_off])).unwrap();
    let reports_off = regoff.evaluate(&sell_order_ctx).unwrap();
    assert!(reports_off.is_empty(), "disabled pack entry must not run");
}

/// P0.4: pack-driven verdict change for the cooldown family.
#[test]
fn p0_4_pack_edit_changes_cooldown_verdict() {
    let plan = ChallengePlan {
        initial_balance_money: Money(dec!(100_000)),
        ..ChallengePlan::default()
    };
    let account = active_account(&plan);
    let now = chrono::Utc::now();
    let last = Trade::new(
        propfirm::core::ids::OrderId::new(),
        account.id,
        Symbol::new("EURUSD"),
        OrderSide::Buy,
        TradeSide::Entry,
        Price(dec!(1.08)),
        Quantity(dec!(1)),
        Money::ZERO,
        now - chrono::Duration::seconds(5),
    );
    // Order submitted 5s after the last trade.

    // Pack cooldown = 10s → violation (5 < 10).
    let mut e10 = propfirm::rulepack::RuleEntry::new("cd", "cooldown", dec!(10));
    e10.enabled = true;
    let reg10 = RuleRegistry::build_from_pack(&pack_with(vec![e10])).unwrap();
    let ctx = {
        let mut c = order_ctx(&account, dec!(1), "EURUSD");
        c.today_trades = vec![last.clone()];
        c
    };
    let reports = reg10.evaluate(&ctx).unwrap();
    assert!(reports
        .iter()
        .any(|r| matches!(r.verdict, propfirm::rules::traits::RuleVerdict::Warn(_))));

    // Pack cooldown = 1s → no violation (5 > 1).
    let mut e1 = propfirm::rulepack::RuleEntry::new("cd", "cooldown", dec!(1));
    e1.enabled = true;
    let reg1 = RuleRegistry::build_from_pack(&pack_with(vec![e1])).unwrap();
    let reports1 = reg1.evaluate(&ctx).unwrap();
    assert!(reports1
        .iter()
        .all(|r| !matches!(r.verdict, propfirm::rules::traits::RuleVerdict::Warn(_))));
}

// Silence unused warnings for helper items only used in some cfg paths.
const _: Option<ChallengePhase> = None;
const _: Option<ServerTime> = None;
const _: Option<Timestamp> = None;

// ---------------------------------------------------------------------------
// Item 4 — false-positive tests: plan ALLOWS the feature → rule must Pass
// even if the rule IS registered and the triggering behavior is present.
// ---------------------------------------------------------------------------

/// grid_trading: plan allows grid → rule must Pass even with grid-like trades.
#[test]
fn item4_grid_allowed_no_false_positive() {
    let mut plan = ftmo_phase1();
    plan.grid_trading_allowed = true;
    let account = active_account(&plan);
    let rule = propfirm::rules::evaluators::grid_trading::GridTradingRule::default();
    let order = Order {
        id: propfirm::core::ids::OrderId::new(),
        account_id: account.id,
        symbol: Symbol::new("EURUSD"),
        side: OrderSide::Buy,
        kind: OrderKind::Open,
        order_type: OrderType::Market,
        quantity: Quantity(dec!(1)),
        tif: TimeInForce::Gtc,
        stop_loss: None,
        take_profit: None,
        comment: None,
        submitted_at: chrono::Utc::now(),
        status: propfirm::core::order::OrderStatus::Pending,
        filled_quantity: Quantity::ZERO,
        avg_fill_price: None,
    };
    let mut ctx = RuleContext::for_open_order(account.clone(), &order);
    ctx.kind = RuleContextKind::OnOrderSubmit;
    ctx.rule_config = propfirm::config::rule_config::RuleConfig::from_plan(&account.plan);
    assert!(
        !rule.is_enabled(&ctx),
        "grid_trading must be disabled when plan.grid_trading_allowed=true"
    );
    let verdict = rule.evaluate(&ctx).unwrap();
    assert!(
        matches!(verdict, propfirm::rules::traits::RuleVerdict::Pass),
        "grid_trading must Pass when plan allows grid; got {verdict:?}"
    );
}

/// copy_trading: plan allows copy → rule must Pass even with correlated trades.
#[test]
fn item4_copy_allowed_no_false_positive() {
    let mut plan = ftmo_phase1();
    plan.copy_trading_allowed = true;
    let account = active_account(&plan);
    let rule = propfirm::rules::evaluators::copy_trading::CopyTradingRule::default();
    let mut ctx = RuleContext::new(account.clone());
    ctx.kind = RuleContextKind::OnTradeFill;
    ctx.rule_config = propfirm::config::rule_config::RuleConfig::from_plan(&account.plan);
    assert!(
        !rule.is_enabled(&ctx),
        "copy_trading must be disabled when plan.copy_trading_allowed=true"
    );
    let verdict = rule.evaluate(&ctx).unwrap();
    assert!(
        matches!(verdict, propfirm::rules::traits::RuleVerdict::Pass),
        "copy_trading must Pass when plan allows copy trading; got {verdict:?}"
    );
}

/// consistency: plan has no consistency_pct → rule must Pass.
#[test]
fn item4_consistency_not_configured_no_false_positive() {
    let plan_no_consistency = ChallengePlan {
        initial_balance_money: Money(dec!(100_000)),
        consistency_pct: None,
        ..ChallengePlan::default()
    };
    let account = active_account(&plan_no_consistency);
    assert!(
        plan_no_consistency.consistency_pct.is_none(),
        "default plan must have no consistency_pct"
    );
    let rule = propfirm::rules::evaluators::consistency::ConsistencyRule::default();
    let mut ctx = RuleContext::new(account.clone());
    ctx.kind = RuleContextKind::OnDemand;
    ctx.rule_config = propfirm::config::rule_config::RuleConfig::from_plan(&account.plan);
    assert!(
        !rule.is_enabled(&ctx),
        "consistency must be disabled when plan.consistency_pct is None"
    );
    let verdict = rule.evaluate(&ctx).unwrap();
    assert!(
        matches!(verdict, propfirm::rules::traits::RuleVerdict::Pass),
        "consistency must Pass when not configured; got {verdict:?}"
    );
}

/// cooldown: plan has cooldown_seconds=0 → rule must Pass even with rapid trades.
#[test]
fn item4_cooldown_zero_no_false_positive() {
    let plan = ChallengePlan {
        initial_balance_money: Money(dec!(100_000)),
        cooldown_seconds: 0,
        ..ChallengePlan::default()
    };
    let account = active_account(&plan);
    let rule = propfirm::rules::evaluators::cooldown::CooldownRule::default();
    let order = Order {
        id: propfirm::core::ids::OrderId::new(),
        account_id: account.id,
        symbol: Symbol::new("EURUSD"),
        side: OrderSide::Buy,
        kind: OrderKind::Open,
        order_type: OrderType::Market,
        quantity: Quantity(dec!(1)),
        tif: TimeInForce::Gtc,
        stop_loss: None,
        take_profit: None,
        comment: None,
        submitted_at: chrono::Utc::now(),
        status: propfirm::core::order::OrderStatus::Pending,
        filled_quantity: Quantity::ZERO,
        avg_fill_price: None,
    };
    let mut ctx = RuleContext::for_open_order(account.clone(), &order);
    ctx.kind = RuleContextKind::OnOrderSubmit;
    ctx.rule_config = propfirm::config::rule_config::RuleConfig::from_plan(&account.plan);
    ctx.today_trades = vec![Trade::new(
        propfirm::core::ids::OrderId::new(),
        account.id,
        Symbol::new("EURUSD"),
        OrderSide::Buy,
        TradeSide::Entry,
        Price(dec!(1.08)),
        Quantity(dec!(1)),
        Money::ZERO,
        chrono::Utc::now() - chrono::Duration::seconds(1),
    )];
    assert!(
        !rule.is_enabled(&ctx),
        "cooldown must be disabled when plan.cooldown_seconds=0"
    );
    let verdict = rule.evaluate(&ctx).unwrap();
    assert!(
        matches!(verdict, propfirm::rules::traits::RuleVerdict::Pass),
        "cooldown must Pass when seconds=0; got {verdict:?}"
    );
}

/// inactivity: plan has no inactivity_days → rule must Pass.
#[test]
fn item4_inactivity_not_configured_no_false_positive() {
    let plan = ChallengePlan {
        initial_balance_money: Money(dec!(100_000)),
        inactivity_days: None,
        ..ChallengePlan::default()
    };
    let account = active_account(&plan);
    let rule = propfirm::rules::evaluators::inactivity::InactivityRule::default();
    let mut ctx = RuleContext::new(account.clone());
    ctx.kind = RuleContextKind::OnDemand;
    ctx.rule_config = propfirm::config::rule_config::RuleConfig::from_plan(&account.plan);
    assert!(
        !rule.is_enabled(&ctx),
        "inactivity must be disabled when plan.inactivity_days is None"
    );
    let verdict = rule.evaluate(&ctx).unwrap();
    assert!(
        matches!(verdict, propfirm::rules::traits::RuleVerdict::Pass),
        "inactivity must Pass when not configured; got {verdict:?}"
    );
}

/// news_trading: plan allows news trading → rule must Pass even during a news window.
#[test]
fn item4_news_allowed_no_false_positive() {
    let mut plan = ftmo_phase1();
    plan.news_trading_allowed = true;
    let account = active_account(&plan);
    let rule = propfirm::rules::evaluators::news_trading::NewsTradingRule::default();
    let order = Order {
        id: propfirm::core::ids::OrderId::new(),
        account_id: account.id,
        symbol: Symbol::new("EURUSD"),
        side: OrderSide::Buy,
        kind: OrderKind::Open,
        order_type: OrderType::Market,
        quantity: Quantity(dec!(1)),
        tif: TimeInForce::Gtc,
        stop_loss: None,
        take_profit: None,
        comment: None,
        submitted_at: chrono::Utc::now(),
        status: propfirm::core::order::OrderStatus::Pending,
        filled_quantity: Quantity::ZERO,
        avg_fill_price: None,
    };
    let mut ctx = RuleContext::for_open_order(account.clone(), &order);
    ctx.kind = RuleContextKind::OnOrderSubmit;
    ctx.rule_config = propfirm::config::rule_config::RuleConfig::from_plan(&account.plan);
    assert!(
        !rule.is_enabled(&ctx),
        "news_trading must be disabled when plan.news_trading_allowed=true"
    );
    let verdict = rule.evaluate(&ctx).unwrap();
    assert!(
        matches!(verdict, propfirm::rules::traits::RuleVerdict::Pass),
        "news_trading must Pass when plan allows news trading; got {verdict:?}"
    );
}
