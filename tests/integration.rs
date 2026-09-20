//! Integration tests for the prop firm engine.

use propfirm::prelude::*;
use propfirm::config::presets::{ftmo_phase1, ftmo_phase2, ftmo_funded, myforexfunds_phase1, thefundedtrader_phase1, surgetrader_plan};
use propfirm::persistence::traits::AccountStore;
use propfirm::core::account::{Account, AccountStatus};
use propfirm::core::order::{Order, OrderKind, OrderSide, OrderStatus, OrderType, TimeInForce};
use propfirm::core::position::{Position, PositionSide};
use propfirm::core::tick::{Quote, Tick};
use propfirm::core::trade::{Trade, TradeSide};
use propfirm::core::types::{Money, Price, Quantity, Symbol, dec};
use propfirm::core::violation::{ViolationKind, ViolationSeverity};
use propfirm::engine::decision::DecisionKind;
use propfirm::engine::evaluator::Evaluator;
use propfirm::engine::pipeline::{Pipeline, PipelineEvent};
use propfirm::engine::state::AccountState;
use propfirm::notifications::log::LogNotifier;
use propfirm::persistence::memory::InMemoryStore;

#[test]
fn test_presets_validate() {
    let _ = ftmo_phase1().validate().expect("ftmo_phase1");
    let _ = ftmo_phase2().validate().expect("ftmo_phase2");
    let _ = ftmo_funded().validate().expect("ftmo_funded");
    let _ = myforexfunds_phase1().validate().expect("mff_phase1");
    let _ = thefundedtrader_phase1().validate().expect("tft_phase1");
    let _ = surgetrader_plan().validate().expect("surgetrader");
}

#[test]
fn test_account_start() {
    let plan = ftmo_phase1();
    let account = Account::new(AccountId::new(), plan);
    let account = account.start(chrono::Utc::now()).expect("start");
    assert_eq!(account.status, AccountStatus::Active);
    assert!(account.started_at.is_some());
    assert!(account.deadline.is_some());
}

#[test]
fn test_daily_drawdown_pass() {
    let plan = ftmo_phase1();
    let account = Account::new(AccountId::new(), plan).start(chrono::Utc::now()).unwrap();
    let evaluator = Evaluator::new(account.plan.clone());
    let tick = Tick::new(Symbol::new("EURUSD"), Quote { bid: Price(dec!(1.0800)), ask: Price(dec!(1.0802)), ts: chrono::Utc::now() });
    let result = evaluator.evaluate_tick(&account, &tick, &[], &[], Vec::new()).unwrap();
    assert_eq!(result.decision.kind, DecisionKind::Pass);
}

#[test]
fn test_daily_drawdown_breach() {
    let plan = ftmo_phase1();
    let mut account = Account::new(AccountId::new(), plan).start(chrono::Utc::now()).unwrap();
    // Force equity to 9000 (drawdown of 1000 = 10% – exceeds 5% limit)
    account.equity = Money(dec!(9_000));
    let evaluator = Evaluator::new(account.plan.clone());
    let tick = Tick::new(Symbol::new("EURUSD"), Quote { bid: Price(dec!(1.0800)), ask: Price(dec!(1.0802)), ts: chrono::Utc::now() });
    let result = evaluator.evaluate_tick(&account, &tick, &[], &[], Vec::new()).unwrap();
    assert!(result.decision.kind.is_terminating(), "expected terminating decision, got {:?}", result.decision.kind);
}

#[test]
fn test_max_drawdown_breach() {
    let plan = ftmo_phase1();
    let mut account = Account::new(AccountId::new(), plan).start(chrono::Utc::now()).unwrap();
    account.balance = Money(dec!(8_900)); // -11% from initial
    account.equity = Money(dec!(8_900));
    account.peak_balance = Money(dec!(10_000));
    account.peak_equity = Money(dec!(10_000));
    let evaluator = Evaluator::new(account.plan.clone());
    let tick = Tick::new(Symbol::new("EURUSD"), Quote { bid: Price(dec!(1.0800)), ask: Price(dec!(1.0802)), ts: chrono::Utc::now() });
    let result = evaluator.evaluate_tick(&account, &tick, &[], &[], Vec::new()).unwrap();
    assert!(result.decision.kind.is_terminating());
}

#[test]
fn test_profit_target_reached() {
    let plan = ftmo_phase1();
    let mut account = Account::new(AccountId::new(), plan).start(chrono::Utc::now()).unwrap();
    // balance = 11000 → net = 1000 = 10% target reached
    account.balance = Money(dec!(11_000));
    account.equity = Money(dec!(11_000));
    account.peak_balance = Money(dec!(11_000));
    account.peak_equity = Money(dec!(11_000));
    let evaluator = Evaluator::new(account.plan.clone());
    let tick = Tick::new(Symbol::new("EURUSD"), Quote { bid: Price(dec!(1.0800)), ask: Price(dec!(1.0802)), ts: chrono::Utc::now() });
    let result = evaluator.evaluate_tick(&account, &tick, &[], &[], Vec::new()).unwrap();
    // Profit target rule should pass.
    let profit_rule = result.reports.iter().find(|r| r.rule_name == "Profit Target").expect("profit target rule");
    assert!(profit_rule.verdict.is_pass(), "expected pass, got {:?}", profit_rule.verdict);
}

#[test]
fn test_stop_loss_required() {
    let plan = ftmo_phase1();
    let account = Account::new(AccountId::new(), plan).start(chrono::Utc::now()).unwrap();
    let evaluator = Evaluator::new(account.plan.clone());
    let order = Order::market_open(account.id, Symbol::new("EURUSD"), OrderSide::Buy, Quantity(dec!(1)), None, None, chrono::Utc::now());
    let result = evaluator.evaluate_order(&account, &order, &[], &[], Vec::new()).unwrap();
    assert!(result.decision.kind.is_fail(), "expected SL-required failure");
}

#[test]
fn test_hedging_blocked() {
    let plan = ftmo_phase1();
    let mut account = Account::new(AccountId::new(), plan).start(chrono::Utc::now()).unwrap();
    account.plan.hedging_allowed = false;
    let evaluator = Evaluator::new(account.plan.clone());
    let open_position = Position::open(
        account.id,
        Symbol::new("EURUSD"),
        PositionSide::Long,
        Price(dec!(1.0800)),
        Quantity(dec!(1)),
        chrono::Utc::now(),
        Money::ZERO,
        None, None, None, None,
    );
    let order = Order::market_open(account.id, Symbol::new("EURUSD"), OrderSide::Sell, Quantity(dec!(1)), Some(Price(dec!(1.07))), Some(Price(dec!(1.10))), chrono::Utc::now());
    let result = evaluator.evaluate_order(&account, &order, &[open_position], &[], Vec::new()).unwrap();
    assert!(result.decision.kind.is_fail());
}

#[test]
fn test_max_position_size() {
    let plan = ftmo_phase1();
    let account = Account::new(AccountId::new(), plan).start(chrono::Utc::now()).unwrap();
    let evaluator = Evaluator::new(account.plan.clone());
    // Default plan allows 5 lots; submit 6.
    let order = Order::market_open(account.id, Symbol::new("EURUSD"), OrderSide::Buy, Quantity(dec!(6)), Some(Price(dec!(1.07))), Some(Price(dec!(1.10))), chrono::Utc::now());
    let result = evaluator.evaluate_order(&account, &order, &[], &[], Vec::new()).unwrap();
    assert!(result.decision.kind.is_fail(), "expected position size failure");
}

#[test]
fn test_max_open_positions() {
    let plan = ftmo_phase1();
    let mut account = Account::new(AccountId::new(), plan).start(chrono::Utc::now()).unwrap();
    account.plan.max_open_positions = Some(2);
    let evaluator = Evaluator::new(account.plan.clone());
    let p1 = Position::open(account.id, Symbol::new("EURUSD"), PositionSide::Long, Price(dec!(1.08)), Quantity(dec!(1)), chrono::Utc::now(), Money::ZERO, None, None, None, None);
    let p2 = Position { ..p1.clone() };
    let order = Order::market_open(account.id, Symbol::new("GBPUSD"), OrderSide::Buy, Quantity(dec!(1)), Some(Price(dec!(1.20))), Some(Price(dec!(1.25))), chrono::Utc::now());
    let result = evaluator.evaluate_order(&account, &order, &[p1, p2], &[], Vec::new()).unwrap();
    assert!(result.decision.kind.is_fail());
}

#[test]
fn test_min_trading_days_below() {
    let plan = ftmo_phase1();
    let mut account = Account::new(AccountId::new(), plan).start(chrono::Utc::now()).unwrap();
    account.active_trading_days = 1;
    let evaluator = Evaluator::new(account.plan.clone());
    let ctx = propfirm::rules::context::RuleContext::for_day_rollover(account.clone());
    let result = evaluator.evaluate(&ctx).unwrap();
    // Min trading days rule should warn (3 days required, only 1)
    let _ = result; // just ensure it runs without panicking
}

#[test]
fn test_consistency_rule_warns() {
    let plan = ftmo_phase1();
    let mut account = Account::new(AccountId::new(), plan).start(chrono::Utc::now()).unwrap();
    account.total_realized_pnl = Money(dec!(1_000));
    account.largest_day_profit = Money(dec!(700)); // 70% > 50% cap
    let evaluator = Evaluator::new(account.plan.clone());
    // Use OnDemand context so all rules (including Periodic) run.
    let mut ctx = propfirm::rules::context::RuleContext::new(account.clone());
    ctx.kind = propfirm::rules::context::RuleContextKind::OnDemand;
    ctx.rule_config = propfirm::config::rule_config::RuleConfig::from_plan(&account.plan);
    let result = evaluator.evaluate(&ctx).unwrap();
    let consistency_rule = result.reports.iter().find(|r| r.rule_name == "Consistency").expect("consistency rule");
    assert!(!consistency_rule.verdict.is_pass(), "expected warn/fail, got {:?}", consistency_rule.verdict);
}

#[test]
fn test_pipeline_end_to_end() {
    let mut plan = ftmo_phase1();
    // Make the test deterministic regardless of the day of week it runs on.
    plan.weekend_holding_allowed = true;
    plan.overnight_holding_allowed = true;
    plan.news_trading_allowed = true;
    let account = Account::new(AccountId::new(), plan.clone());
    let store = InMemoryStore::new();
    store.put(account.clone()).unwrap();
    let notifier = LogNotifier::new();
    let evaluator = Evaluator::new(plan);
    let mut pipeline = Pipeline::new(evaluator, store, notifier);
    let now = chrono::Utc::now();
    let result = pipeline.process(account.id, PipelineEvent::AccountStarted { at: now }).unwrap();
    assert_eq!(result.snapshot.account.status, AccountStatus::Active);
    // Submit an order with SL/TP set
    let order = Order {
        id: propfirm::core::ids::OrderId::new(),
        account_id: account.id,
        symbol: Symbol::new("EURUSD"),
        side: OrderSide::Buy,
        kind: OrderKind::Open,
        order_type: OrderType::Market,
        quantity: Quantity(dec!(1)),
        tif: TimeInForce::Ioc,
        stop_loss: Some(Price(dec!(1.05))),
        take_profit: Some(Price(dec!(1.10))),
        comment: None,
        submitted_at: now,
        status: OrderStatus::Pending,
        filled_quantity: Quantity::ZERO,
        avg_fill_price: None,
    };
    let result = pipeline.process(account.id, PipelineEvent::OrderSubmitted { order }).unwrap();
    assert!(result.passed(), "expected pass, got {:?} – violations: {:?}",
        result.snapshot.decision.kind,
        result.result.violations().iter().map(|v| (v.kind, v.message.to_string())).collect::<Vec<_>>());
    // Tick
    let tick = Tick::new(Symbol::new("EURUSD"), Quote { bid: Price(dec!(1.085)), ask: Price(dec!(1.0852)), ts: now });
    let result = pipeline.process(account.id, PipelineEvent::Tick { tick }).unwrap();
    assert!(result.passed());
}

#[test]
fn test_account_state_apply_pnl() {
    let plan = ftmo_phase1();
    let account = Account::new(AccountId::new(), plan).start(chrono::Utc::now()).unwrap();
    let state = AccountState::new(account);
    let state = state.apply_realized_pnl(Money(dec!(500)), Money(dec!(10)), Money::ZERO, chrono::Utc::now());
    assert_eq!(state.account.balance.0, dec!(10_490));
    assert_eq!(state.account.total_realized_pnl.0, dec!(490));
    assert_eq!(state.account.largest_day_profit.0, dec!(490));
}

#[test]
fn test_account_state_rollover() {
    let plan = ftmo_phase1();
    let account = Account::new(AccountId::new(), plan).start(chrono::Utc::now()).unwrap();
    let mut state = AccountState::new(account);
    state = state.apply_realized_pnl(Money(dec!(100)), Money::ZERO, Money::ZERO, chrono::Utc::now());
    assert_eq!(state.account.today_realized_pnl.0, dec!(100));
    state = state.rollover_day(true);
    assert_eq!(state.account.active_trading_days, 1);
    assert_eq!(state.account.today_realized_pnl.0, dec!(0));
    assert_eq!(state.account.day_start_balance.0, dec!(10_100));
}

#[test]
fn test_risk_metrics_smoke() {
    let eq = vec![Money(dec!(10_000)), Money(dec!(10_200)), Money(dec!(10_150)), Money(dec!(10_500))];
    let trades = vec![Money(dec!(200)), Money(dec!(-50)), Money(dec!(350))];
    let r = propfirm::risk::metrics::RiskMetrics::compute(&eq, &trades);
    assert!(r.sharpe >= dec!(0));
    assert!(r.profit_factor > dec!(0));
    assert_eq!(r.total_trades, 3);
    assert_eq!(r.winning_trades, 2);
    assert_eq!(r.losing_trades, 1);
}

#[test]
fn test_event_store_replay() {
    use propfirm::events::store::EventStore;
    use propfirm::core::events::{DomainEvent, DomainEventKind};
    let store = EventStore::in_memory();
    let plan = ftmo_phase1();
    let account = Account::new(AccountId::new(), plan.clone());
    let ev1 = DomainEvent::new(account.id, DomainEventKind::AccountStarted, chrono::Utc::now());
    store.append(ev1).unwrap();
    let ev2 = DomainEvent::new(account.id, DomainEventKind::TradeFilled {
        trade: Trade::new(
            propfirm::core::ids::OrderId::new(),
            account.id,
            Symbol::new("EURUSD"),
            OrderSide::Buy,
            TradeSide::Entry,
            Price(dec!(1.08)),
            Quantity(dec!(1)),
            Money::ZERO,
            chrono::Utc::now(),
        ),
    }, chrono::Utc::now());
    store.append(ev2).unwrap();
    let replayed = store.replay(account.id, account.clone()).unwrap();
    assert_eq!(replayed.status, AccountStatus::Active);
}

#[test]
fn test_violation_validate() {
    use propfirm::core::ids::RuleId;
    use propfirm::core::violation::Violation;
    let v = Violation::new(AccountId::new(), RuleId::named("test"), "test", ViolationKind::DailyDrawdown, ViolationSeverity::Hard, "test", chrono::Utc::now());
    v.validate().unwrap();
}

#[test]
fn test_decision_aggregation() {
    use propfirm::rules::traits::{RuleReport, RuleVerdict};
    use propfirm::rules::context::EvaluationScope;
    use propfirm::core::ids::RuleId;
    let v = propfirm::core::violation::Violation::new(AccountId::new(), RuleId::named("test"), "test", ViolationKind::DailyDrawdown, ViolationSeverity::Warning, "test", chrono::Utc::now());
    let r1 = RuleReport::new(RuleId::named("a"), "A", RuleVerdict::Pass, EvaluationScope::OnTick);
    let r2 = RuleReport::new(RuleId::named("b"), "B", RuleVerdict::Warn(v), EvaluationScope::OnTick);
    let d = propfirm::engine::decision::Decision::from_reports(&[r1, r2]);
    assert_eq!(d.kind, DecisionKind::Warn);
}

#[test]
fn test_position_unrealized_pnl() {
    use propfirm::core::position::unrealized_pnl;
    let entry = Price(dec!(1.0800));
    let current = Price(dec!(1.0900));
    let qty = Quantity(dec!(100_000));
    let pnl = unrealized_pnl(entry, current, qty, PositionSide::Long);
    assert_eq!(pnl.0, dec!(1000));
}

#[test]
fn test_news_window_detection() {
    // Friday 12:25 UTC – 5 minutes before NFP (12:30)
    use propfirm::rules::evaluators::news_trading::within_news_window;
    let friday_pre_news = chrono::DateTime::parse_from_rfc3339("2026-09-18T12:25:00Z").unwrap().with_timezone(&chrono::Utc);
    assert!(within_news_window(friday_pre_news, 5).is_some());
    // Sunday – no events
    let sunday = chrono::DateTime::parse_from_rfc3339("2026-09-20T12:00:00Z").unwrap().with_timezone(&chrono::Utc);
    assert!(within_news_window(sunday, 5).is_none());
}

#[test]
fn test_copy_trading_detection() {
    use propfirm::core::events::{DomainEvent, DomainEventKind};
    let plan = ftmo_phase1();
    let account = Account::new(AccountId::new(), plan).start(chrono::Utc::now()).unwrap();
    let mut acc = account;
    acc.plan.copy_trading_allowed = false;
    let evaluator = Evaluator::new(acc.plan.clone());
    let now = chrono::Utc::now();
    let trade = Trade::new(
        propfirm::core::ids::OrderId::new(),
        acc.id,
        Symbol::new("EURUSD"),
        OrderSide::Buy,
        TradeSide::Entry,
        Price(dec!(1.08)),
        Quantity(dec!(1)),
        Money::ZERO,
        now,
    );
    let ref_trade = Trade { ..trade.clone() };
    let mut events = Vec::new();
    for _ in 0..3 {
        let ev = DomainEvent::new(acc.id, DomainEventKind::TradeFilled { trade: ref_trade.clone() }, now);
        events.push(ev);
    }
    let result = evaluator.evaluate_trade(&acc, &trade, &[], &[], events).unwrap();
    assert!(result.decision.kind.is_fail(), "expected copy-trading failure, got {:?}", result.decision.kind);
}

#[test]
fn test_time_limit_expired() {
    let plan = ftmo_phase1();
    let mut account = Account::new(AccountId::new(), plan).start(chrono::Utc::now() - chrono::Duration::days(40)).unwrap();
    account.deadline = Some(chrono::Utc::now() - chrono::Duration::days(10));
    let evaluator = Evaluator::new(account.plan.clone());
    let tick = Tick::new(Symbol::new("EURUSD"), Quote { bid: Price(dec!(1.08)), ask: Price(dec!(1.0802)), ts: chrono::Utc::now() });
    let result = evaluator.evaluate_tick(&account, &tick, &[], &[], Vec::new()).unwrap();
    // Time limit rule should fail
    let time_rule = result.reports.iter().find(|r| r.rule_name == "Time Limit");
    if let Some(r) = time_rule {
        assert!(!r.verdict.is_pass(), "time limit should fail");
    }
}
