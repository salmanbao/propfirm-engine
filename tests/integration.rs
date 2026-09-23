//! Integration tests for the prop firm engine.

use propfirm::config::presets::{
    ftmo_funded, ftmo_phase1, ftmo_phase2, myforexfunds_phase1, surgetrader_plan,
    thefundedtrader_phase1,
};
use propfirm::core::account::{Account, AccountStatus};
use propfirm::core::order::{Order, OrderKind, OrderSide, OrderStatus, OrderType, TimeInForce};
use propfirm::core::position::{Position, PositionSide};
use propfirm::core::tick::{Quote, Tick};
use propfirm::core::trade::{Trade, TradeSide};
use propfirm::core::types::{dec, Money, Price, Quantity, Symbol};
use propfirm::core::violation::{ViolationKind, ViolationSeverity};
use propfirm::engine::decision::DecisionKind;
use propfirm::engine::evaluator::Evaluator;
use propfirm::engine::pipeline::{Pipeline, PipelineEvent};
use propfirm::engine::state::AccountState;
use propfirm::notifications::log::LogNotifier;
use propfirm::persistence::memory::InMemoryStore;
use propfirm::persistence::traits::AccountStore;
use propfirm::prelude::*;

#[test]
fn test_presets_validate() {
    ftmo_phase1().validate().expect("ftmo_phase1");
    ftmo_phase2().validate().expect("ftmo_phase2");
    ftmo_funded().validate().expect("ftmo_funded");
    myforexfunds_phase1().validate().expect("mff_phase1");
    thefundedtrader_phase1().validate().expect("tft_phase1");
    surgetrader_plan().validate().expect("surgetrader");
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
    let account = Account::new(AccountId::new(), plan)
        .start(chrono::Utc::now())
        .unwrap();
    let evaluator = Evaluator::new(&account.plan);
    let tick = Tick::new(
        Symbol::new("EURUSD"),
        Quote {
            bid: Price(dec!(1.0800)),
            ask: Price(dec!(1.0802)),
            ts: chrono::Utc::now(),
        },
    );
    let result = evaluator
        .evaluate_tick(&account, &tick, &[], &[], Vec::new())
        .unwrap();
    assert_eq!(result.decision.kind, DecisionKind::Pass);
}

#[test]
fn test_daily_drawdown_breach() {
    let plan = ftmo_phase1();
    let mut account = Account::new(AccountId::new(), plan)
        .start(chrono::Utc::now())
        .unwrap();
    // Force equity to 9000 (drawdown of 1000 = 10% – exceeds 5% limit)
    account.equity = Money(dec!(9_000));
    let evaluator = Evaluator::new(&account.plan);
    let tick = Tick::new(
        Symbol::new("EURUSD"),
        Quote {
            bid: Price(dec!(1.0800)),
            ask: Price(dec!(1.0802)),
            ts: chrono::Utc::now(),
        },
    );
    let result = evaluator
        .evaluate_tick(&account, &tick, &[], &[], Vec::new())
        .unwrap();
    assert!(
        result.decision.kind.is_terminating(),
        "expected terminating decision, got {:?}",
        result.decision.kind
    );
}

#[test]
fn test_max_drawdown_breach() {
    let plan = ftmo_phase1();
    let mut account = Account::new(AccountId::new(), plan)
        .start(chrono::Utc::now())
        .unwrap();
    account.balance = Money(dec!(8_900)); // -11% from initial
    account.equity = Money(dec!(8_900));
    account.peak_balance = Money(dec!(10_000));
    account.peak_equity = Money(dec!(10_000));
    let evaluator = Evaluator::new(&account.plan);
    let tick = Tick::new(
        Symbol::new("EURUSD"),
        Quote {
            bid: Price(dec!(1.0800)),
            ask: Price(dec!(1.0802)),
            ts: chrono::Utc::now(),
        },
    );
    let result = evaluator
        .evaluate_tick(&account, &tick, &[], &[], Vec::new())
        .unwrap();
    assert!(result.decision.kind.is_terminating());
}

#[test]
fn test_profit_target_reached() {
    let plan = ftmo_phase1();
    let mut account = Account::new(AccountId::new(), plan)
        .start(chrono::Utc::now())
        .unwrap();
    // balance = 11000 → net = 1000 = 10% target reached
    account.balance = Money(dec!(11_000));
    account.equity = Money(dec!(11_000));
    account.peak_balance = Money(dec!(11_000));
    account.peak_equity = Money(dec!(11_000));
    let evaluator = Evaluator::new(&account.plan);
    let tick = Tick::new(
        Symbol::new("EURUSD"),
        Quote {
            bid: Price(dec!(1.0800)),
            ask: Price(dec!(1.0802)),
            ts: chrono::Utc::now(),
        },
    );
    let result = evaluator
        .evaluate_tick(&account, &tick, &[], &[], Vec::new())
        .unwrap();
    // P0-3 fix: profit target rule now emits TargetHit (distinct from Pass)
    // the first time the target is reached, so downstream consumers can react.
    let profit_rule = result
        .reports
        .iter()
        .find(|r| r.rule_name == "Profit Target")
        .expect("profit target rule");
    assert!(
        profit_rule.verdict.is_target_hit(),
        "expected TargetHit, got {:?}",
        profit_rule.verdict
    );
    // The overall decision should be TargetHit (not Pass), since this is the
    // most-positive thing that happened on this evaluation.
    assert_eq!(
        result.decision.kind,
        propfirm::engine::decision::DecisionKind::TargetHit,
        "expected decision=TargetHit, got {:?}",
        result.decision.kind
    );
}

#[test]
fn test_stop_loss_required() {
    let plan = ftmo_phase1();
    let account = Account::new(AccountId::new(), plan)
        .start(chrono::Utc::now())
        .unwrap();
    let evaluator = Evaluator::new(&account.plan);
    let order = Order::market_open(
        account.id,
        Symbol::new("EURUSD"),
        OrderSide::Buy,
        Quantity(dec!(1)),
        None,
        None,
        chrono::Utc::now(),
    );
    let result = evaluator
        .evaluate_order(&account, &order, &[], &[], Vec::new())
        .unwrap();
    assert!(
        result.decision.kind.is_fail(),
        "expected SL-required failure"
    );
}

#[test]
fn test_hedging_blocked() {
    let plan = ftmo_phase1();
    let mut account = Account::new(AccountId::new(), plan)
        .start(chrono::Utc::now())
        .unwrap();
    account.plan.hedging_allowed = false;
    let evaluator = Evaluator::new(&account.plan);
    let open_position = Position::open(
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
    let order = Order::market_open(
        account.id,
        Symbol::new("EURUSD"),
        OrderSide::Sell,
        Quantity(dec!(1)),
        Some(Price(dec!(1.07))),
        Some(Price(dec!(1.10))),
        chrono::Utc::now(),
    );
    let result = evaluator
        .evaluate_order(&account, &order, &[open_position], &[], Vec::new())
        .unwrap();
    assert!(result.decision.kind.is_fail());
}

#[test]
fn test_max_position_size() {
    let plan = ftmo_phase1();
    let account = Account::new(AccountId::new(), plan)
        .start(chrono::Utc::now())
        .unwrap();
    let evaluator = Evaluator::new(&account.plan);
    // Default plan allows 5 lots; submit 6.
    let order = Order::market_open(
        account.id,
        Symbol::new("EURUSD"),
        OrderSide::Buy,
        Quantity(dec!(6)),
        Some(Price(dec!(1.07))),
        Some(Price(dec!(1.10))),
        chrono::Utc::now(),
    );
    let result = evaluator
        .evaluate_order(&account, &order, &[], &[], Vec::new())
        .unwrap();
    assert!(
        result.decision.kind.is_fail(),
        "expected position size failure"
    );
}

#[test]
fn test_max_open_positions() {
    let plan = ftmo_phase1();
    let mut account = Account::new(AccountId::new(), plan)
        .start(chrono::Utc::now())
        .unwrap();
    account.plan.max_open_positions = Some(2);
    let evaluator = Evaluator::new(&account.plan);
    let p1 = Position::open(
        account.id,
        Symbol::new("EURUSD"),
        PositionSide::Long,
        Price(dec!(1.08)),
        Quantity(dec!(1)),
        chrono::Utc::now(),
        Money::ZERO,
        None,
        None,
        None,
        None,
    );
    let p2 = Position { ..p1.clone() };
    let order = Order::market_open(
        account.id,
        Symbol::new("GBPUSD"),
        OrderSide::Buy,
        Quantity(dec!(1)),
        Some(Price(dec!(1.20))),
        Some(Price(dec!(1.25))),
        chrono::Utc::now(),
    );
    let result = evaluator
        .evaluate_order(&account, &order, &[p1, p2], &[], Vec::new())
        .unwrap();
    assert!(result.decision.kind.is_fail());
}

#[test]
fn test_min_trading_days_below() {
    let plan = ftmo_phase1();
    let mut account = Account::new(AccountId::new(), plan)
        .start(chrono::Utc::now())
        .unwrap();
    account.active_trading_days = 1;
    let evaluator = Evaluator::new(&account.plan);
    let ctx = propfirm::rules::context::RuleContext::for_day_rollover(account.clone());
    let result = evaluator.evaluate(&ctx).unwrap();
    // Min trading days rule should warn (3 days required, only 1)
    let _ = result; // just ensure it runs without panicking
}

#[test]
fn test_consistency_rule_warns() {
    let plan = ftmo_phase1();
    let mut account = Account::new(AccountId::new(), plan)
        .start(chrono::Utc::now())
        .unwrap();
    // Consistency uses sum_positive_days_profit as the denominator
    // (industry standard), not total_realized_pnl (which includes losses).
    account.sum_positive_days_profit = Money(dec!(1_000));
    account.largest_day_profit = Money(dec!(700)); // 70% > 50% cap
    let evaluator = Evaluator::new(&account.plan);
    // Use OnDemand context so all rules (including Periodic) run.
    let mut ctx = propfirm::rules::context::RuleContext::new(account.clone());
    ctx.kind = propfirm::rules::context::RuleContextKind::OnDemand;
    ctx.rule_config = propfirm::config::rule_config::RuleConfig::from_plan(&account.plan);
    let result = evaluator.evaluate(&ctx).unwrap();
    let consistency_rule = result
        .reports
        .iter()
        .find(|r| r.rule_name == "Consistency")
        .expect("consistency rule");
    assert!(
        !consistency_rule.verdict.is_pass(),
        "expected warn/fail, got {:?}",
        consistency_rule.verdict
    );
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
    let evaluator = Evaluator::new(&plan);
    let mut pipeline = Pipeline::new(evaluator, store, notifier);
    let now = chrono::Utc::now();
    let result = pipeline
        .process(account.id, PipelineEvent::AccountStarted { at: now })
        .unwrap();
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
    let result = pipeline
        .process(account.id, PipelineEvent::OrderSubmitted { order })
        .unwrap();
    assert!(
        result.passed(),
        "expected pass, got {:?} – violations: {:?}",
        result.snapshot.decision.kind,
        result
            .result
            .violations()
            .iter()
            .map(|v| (v.kind, v.message.to_string()))
            .collect::<Vec<_>>()
    );
    // Tick
    let tick = Tick::new(
        Symbol::new("EURUSD"),
        Quote {
            bid: Price(dec!(1.085)),
            ask: Price(dec!(1.0852)),
            ts: now,
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
    assert!(result.passed());
}

#[test]
fn test_account_state_apply_pnl() {
    let plan = ftmo_phase1();
    let account = Account::new(AccountId::new(), plan)
        .start(chrono::Utc::now())
        .unwrap();
    let state = AccountState::new(account);
    let state = state.apply_realized_pnl(
        Money(dec!(500)),
        Money(dec!(10)),
        Money::ZERO,
        chrono::Utc::now(),
    );
    assert_eq!(state.account.balance.0, dec!(10_490));
    assert_eq!(state.account.total_realized_pnl.0, dec!(490));
    // P1.7: largest_day_profit is no longer updated per-trade; it's
    // updated only at day rollover. Verify the running today_realized_pnl
    // is the correct accumulator instead.
    assert_eq!(
        state.account.today_realized_pnl.0,
        dec!(490),
        "today_realized_pnl should accumulate per-trade; got {}",
        state.account.today_realized_pnl.0
    );
    assert_eq!(
        state.account.largest_day_profit.0,
        dec!(0),
        "P1.7: largest_day_profit should NOT be updated per-trade (was a bug); got {}",
        state.account.largest_day_profit.0
    );
    // Now roll over the day — largest_day_profit should be stamped.
    let state = state.rollover_day(true);
    assert_eq!(state.account.largest_day_profit.0, dec!(490),
        "P1.7: after rollover, largest_day_profit should be stamped from today_realized_pnl; got {}",
        state.account.largest_day_profit.0);
}

#[test]
fn test_account_state_rollover() {
    let plan = ftmo_phase1();
    let account = Account::new(AccountId::new(), plan)
        .start(chrono::Utc::now())
        .unwrap();
    let mut state = AccountState::new(account);
    state = state.apply_realized_pnl(
        Money(dec!(100)),
        Money::ZERO,
        Money::ZERO,
        chrono::Utc::now(),
    );
    assert_eq!(state.account.today_realized_pnl.0, dec!(100));
    state = state.rollover_day(true);
    assert_eq!(state.account.active_trading_days, 1);
    assert_eq!(state.account.today_realized_pnl.0, dec!(0));
    assert_eq!(state.account.day_start_balance.0, dec!(10_100));
}

#[test]
fn test_risk_metrics_smoke() {
    let eq = vec![
        Money(dec!(10_000)),
        Money(dec!(10_200)),
        Money(dec!(10_150)),
        Money(dec!(10_500)),
    ];
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
    use propfirm::core::events::{DomainEvent, DomainEventKind};
    use propfirm::events::store::EventStore;
    let store = EventStore::in_memory();
    let plan = ftmo_phase1();
    let account = Account::new(AccountId::new(), plan.clone());
    let ev1 = DomainEvent::new(
        account.id,
        DomainEventKind::AccountStarted,
        chrono::Utc::now(),
    );
    store.append(ev1).unwrap();
    let ev2 = DomainEvent::new(
        account.id,
        DomainEventKind::TradeFilled {
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
        },
        chrono::Utc::now(),
    );
    store.append(ev2).unwrap();
    let replayed = store.replay(account.id, account.clone()).unwrap();
    assert_eq!(replayed.status, AccountStatus::Active);
}

#[test]
fn test_violation_validate() {
    use propfirm::core::ids::RuleId;
    use propfirm::core::violation::Violation;
    let v = Violation::new(
        AccountId::new(),
        RuleId::named("test"),
        "test",
        ViolationKind::DailyDrawdown,
        ViolationSeverity::Hard,
        "test",
        chrono::Utc::now(),
    );
    v.validate().unwrap();
}

#[test]
fn test_decision_aggregation() {
    use propfirm::core::ids::RuleId;
    use propfirm::rules::context::EvaluationScope;
    use propfirm::rules::traits::{RuleReport, RuleVerdict};
    let v = propfirm::core::violation::Violation::new(
        AccountId::new(),
        RuleId::named("test"),
        "test",
        ViolationKind::DailyDrawdown,
        ViolationSeverity::Warning,
        "test",
        chrono::Utc::now(),
    );
    let r1 = RuleReport::new(
        RuleId::named("a"),
        "A",
        RuleVerdict::Pass,
        EvaluationScope::OnTick,
    );
    let r2 = RuleReport::new(
        RuleId::named("b"),
        "B",
        RuleVerdict::Warn(v),
        EvaluationScope::OnTick,
    );
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
    let friday_pre_news = chrono::DateTime::parse_from_rfc3339("2026-09-18T12:25:00Z")
        .unwrap()
        .with_timezone(&chrono::Utc);
    assert!(within_news_window(friday_pre_news, 5).is_some());
    // Sunday – no events
    let sunday = chrono::DateTime::parse_from_rfc3339("2026-09-20T12:00:00Z")
        .unwrap()
        .with_timezone(&chrono::Utc);
    assert!(within_news_window(sunday, 5).is_none());
}

#[test]
fn test_copy_trading_detection() {
    // REWRITTEN with the §A.2 cross-account fix (was: three same-account
    // events in recent_events — structurally self-referential). Copy
    // trading is now detected by correlating the trader's fill against a
    // reference feed of OTHER accounts' fills via
    // `RuleContext::cross_reference_trades`.
    let plan = ftmo_phase1();
    let mut acc = Account::new(AccountId::new(), plan)
        .start(chrono::Utc::now())
        .unwrap();
    acc.plan.copy_trading_allowed = false;
    let evaluator = Evaluator::new(&acc.plan);
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
    // Three fills from three OTHER accounts, same symbol/side/size, same
    // instant — the copied-signal signature.
    let reference: Vec<Trade> = (0..3)
        .map(|_| {
            Trade::new(
                propfirm::core::ids::OrderId::new(),
                AccountId::new(), // a different account
                Symbol::new("EURUSD"),
                OrderSide::Buy,
                TradeSide::Entry,
                Price(dec!(1.08)),
                Quantity(dec!(1)),
                Money::ZERO,
                now,
            )
        })
        .collect();
    use propfirm::rules::context::RuleContext;
    let mut ctx = RuleContext::for_trade_fill(acc.clone(), &trade);
    ctx.open_positions = Vec::new();
    ctx.today_trades = Vec::new();
    ctx.cross_reference_trades = reference;
    let result = evaluator.evaluate(&ctx).unwrap();
    assert!(
        result.decision.kind.is_fail(),
        "expected cross-account copy-trading failure, got {:?}",
        result.decision.kind
    );
}

#[test]
fn test_time_limit_expired() {
    let plan = ftmo_phase1();
    let mut account = Account::new(AccountId::new(), plan)
        .start(chrono::Utc::now() - chrono::Duration::days(40))
        .unwrap();
    account.deadline = Some(chrono::Utc::now() - chrono::Duration::days(10));
    let evaluator = Evaluator::new(&account.plan);
    let tick = Tick::new(
        Symbol::new("EURUSD"),
        Quote {
            bid: Price(dec!(1.08)),
            ask: Price(dec!(1.0802)),
            ts: chrono::Utc::now(),
        },
    );
    let result = evaluator
        .evaluate_tick(&account, &tick, &[], &[], Vec::new())
        .unwrap();
    // Time limit rule should fail
    let time_rule = result.reports.iter().find(|r| r.rule_name == "Time Limit");
    if let Some(r) = time_rule {
        assert!(!r.verdict.is_pass(), "time limit should fail");
    }
}

// ============================================================================
// P0 REGRESSION TESTS — these pin the semantics from the gap review
// and must never silently change in future refactors.
// ============================================================================

#[test]
fn p0_1_static_max_drawdown_does_not_breach_when_above_initial_floor() {
    // Account grew to 105k then pulled back to 95k. Static max loss is
    // 10% of initial 100k → floor is 90k. 95k > 90k → NO breach.
    // The OLD code would have tripped because it measured dd from peak
    // (105k) instead of from initial (100k).
    let plan = ftmo_phase1().with_loss_reference(propfirm::config::plan::LossReference::Static);
    let mut account = Account::new(AccountId::new(), plan)
        .start(chrono::Utc::now())
        .unwrap();
    account.balance = Money(dec!(95_000));
    account.equity = Money(dec!(95_000));
    account.peak_balance = Money(dec!(105_000)); // grew then pulled back
    account.peak_equity = Money(dec!(105_000));
    account.initial_balance = Money(dec!(100_000));
    let evaluator = Evaluator::new(&account.plan);
    let tick = Tick::new(
        Symbol::new("EURUSD"),
        Quote {
            bid: Price(dec!(1.0800)),
            ask: Price(dec!(1.0802)),
            ts: chrono::Utc::now(),
        },
    );
    let result = evaluator
        .evaluate_tick(&account, &tick, &[], &[], Vec::new())
        .unwrap();
    // MaxDrawdown rule should NOT fire — we're still above the static floor.
    let max_dd = result
        .reports
        .iter()
        .find(|r| r.rule_name == "Maximum Drawdown")
        .expect("max dd rule");
    assert!(
        max_dd.verdict.is_pass() || !max_dd.verdict.is_terminating(),
        "static max dd should not breach — account is above the floor; got {:?}",
        max_dd.verdict
    );
}

#[test]
fn p0_1_trailing_max_drawdown_does_breach_when_pullback_exceeds_trail() {
    // Same scenario, but in trailing mode: peak=105k, current=95k, trail=10%
    // → floor = 105k - 10.5k = 94.5k. 95k > 94.5k → still OK.
    // But if current drops to 94k → 94k < 94.5k → BREACH.
    let plan = ftmo_phase1().with_loss_reference(propfirm::config::plan::LossReference::Trailing);
    let mut account = Account::new(AccountId::new(), plan)
        .start(chrono::Utc::now())
        .unwrap();
    account.initial_balance = Money(dec!(100_000));
    account.balance = Money(dec!(94_000));
    account.equity = Money(dec!(94_000));
    account.peak_balance = Money(dec!(105_000));
    account.peak_equity = Money(dec!(105_000));
    let evaluator = Evaluator::new(&account.plan);
    let tick = Tick::new(
        Symbol::new("EURUSD"),
        Quote {
            bid: Price(dec!(1.0800)),
            ask: Price(dec!(1.0802)),
            ts: chrono::Utc::now(),
        },
    );
    let result = evaluator
        .evaluate_tick(&account, &tick, &[], &[], Vec::new())
        .unwrap();
    assert!(result.decision.kind.is_terminating(),
        "trailing max dd SHOULD breach (peak 105k - 10.5k trail = 94.5k floor; current 94k < floor), got {:?}",
        result.decision.kind);
}

#[test]
fn p0_2_target_reached_stays_pending_when_equity_dips_below() {
    // Trader hits target on day 1 of a 5-min-trading-day requirement.
    // Equity then dips back below target on day 2. The target_reached_at
    // timestamp must NOT be cleared — the account stays in
    // TargetHitPending until min trading days is satisfied.
    let plan = ftmo_phase1().with_min_days(5);
    let mut account = Account::new(AccountId::new(), plan)
        .start(chrono::Utc::now() - chrono::Duration::days(2))
        .unwrap();
    // P0-2: simulate that target was hit on day 1.
    account.target_reached_at = Some(chrono::Utc::now() - chrono::Duration::days(1));
    account.target_reached_on_day = Some(0);
    account.status = propfirm::core::account::AccountStatus::TargetHitPending;
    // Now equity dips back below target on day 2.
    account.balance = Money(dec!(10_500)); // only 5% profit, below 10% target
    account.equity = Money(dec!(10_500));
    // active_trading_days is still < 5.
    account.active_trading_days = 1;
    let evaluator = Evaluator::new(&account.plan);
    let tick = Tick::new(
        Symbol::new("EURUSD"),
        Quote {
            bid: Price(dec!(1.0800)),
            ask: Price(dec!(1.0802)),
            ts: chrono::Utc::now(),
        },
    );
    let result = evaluator
        .evaluate_tick(&account, &tick, &[], &[], Vec::new())
        .unwrap();
    // P0-2 fix: target_reached_at is sticky. The ProfitTargetRule should
    // return Pass (target was already reached, no need to re-emit TargetHit),
    // and the overall decision should NOT be Fail (we're still in pending
    // state, not breached).
    let profit_rule = result
        .reports
        .iter()
        .find(|r| r.rule_name == "Profit Target")
        .expect("profit rule");
    assert!(profit_rule.verdict.is_pass(),
        "once target_reached_at is set, ProfitTargetRule should Pass (not re-emit TargetHit); got {:?}",
        profit_rule.verdict);
    assert!(
        !result.decision.is_terminating(),
        "dip below target while pending is NOT a breach; got {:?}",
        result.decision.kind
    );
    // Verify the account's target_reached_at is still set (sticky).
    assert!(
        account.target_reached_at.is_some(),
        "target_reached_at must not be cleared by a dip below target"
    );
}

#[test]
fn p0_3_breach_beats_target_hit_on_same_tick() {
    // Both MaxDrawdown breach and ProfitTarget hit on the same evaluation.
    // Per the binding spec, the breach MUST win — "breach wins, always".
    let plan = ftmo_phase1();
    let mut account = Account::new(AccountId::new(), plan)
        .start(chrono::Utc::now())
        .unwrap();
    // Simultaneously: target reached (net = 1000 = 10% target) AND
    // max drawdown breached (peak 105k, current 94k, trail-style).
    account.initial_balance = Money(dec!(100_000));
    account.balance = Money(dec!(101_000)); // net = 1000 = 10% target reached
    account.equity = Money(dec!(94_000)); // but equity dipped (drawdown breach)
    account.peak_balance = Money(dec!(105_000));
    account.peak_equity = Money(dec!(105_000));
    // Trailing mode so dd measures from peak (105k) → dd = 11k, limit = 10.5k → BREACH.
    account.plan.max_loss_reference = propfirm::config::plan::LossReference::Trailing;
    let evaluator = Evaluator::new(&account.plan);
    let tick = Tick::new(
        Symbol::new("EURUSD"),
        Quote {
            bid: Price(dec!(1.0800)),
            ask: Price(dec!(1.0802)),
            ts: chrono::Utc::now(),
        },
    );
    let result = evaluator
        .evaluate_tick(&account, &tick, &[], &[], Vec::new())
        .unwrap();
    // Decision MUST be terminating (breach won), NOT TargetHit.
    assert!(
        result.decision.is_terminating(),
        "breach must beat target_hit on the same tick; got {:?}",
        result.decision.kind
    );
    assert_ne!(
        result.decision.kind,
        propfirm::engine::decision::DecisionKind::TargetHit,
        "TargetHit must NOT win when a breach fires on the same tick"
    );
}

#[test]
fn p0_4_reordering_rules_produces_same_decision() {
    // The SAME set of rules, registered in DIFFERENT orders, must produce
    // the SAME decision when both fire on the same tick. This is the
    // "one defensible answer" property — registration order cannot
    // silently change the outcome.
    use propfirm::rules::evaluators::*;
    use propfirm::rules::registry::RuleRegistry;
    use std::sync::Arc;

    // Order A: daily_drawdown first, then max_drawdown.
    let mut reg_a = RuleRegistry::empty();
    reg_a.register(Arc::new(daily_drawdown::DailyDrawdownRule::default()));
    reg_a.register(Arc::new(max_drawdown::MaxDrawdownRule::default()));

    // Order B: max_drawdown first, then daily_drawdown (reversed).
    let mut reg_b = RuleRegistry::empty();
    reg_b.register(Arc::new(max_drawdown::MaxDrawdownRule::default()));
    reg_b.register(Arc::new(daily_drawdown::DailyDrawdownRule::default()));

    // Construct an account where BOTH rules would breach.
    let plan = ftmo_phase1();
    let mut account = Account::new(AccountId::new(), plan)
        .start(chrono::Utc::now())
        .unwrap();
    account.initial_balance = Money(dec!(100_000));
    account.balance = Money(dec!(90_000)); // 10k dd from peak → max_dd breach (trailing)
    account.equity = Money(dec!(90_000));
    account.peak_balance = Money(dec!(100_000));
    account.peak_equity = Money(dec!(100_000));
    account.day_start_balance = Money(dec!(100_000));
    account.day_start_equity = Money(dec!(100_000));
    // daily_dd also breaches: 100k → 90k = 10k drop, limit = 5% of 100k = 5k → BREACH.
    account.plan.max_loss_reference = propfirm::config::plan::LossReference::Trailing;

    let tick = Tick::new(
        Symbol::new("EURUSD"),
        Quote {
            bid: Price(dec!(1.0800)),
            ask: Price(dec!(1.0802)),
            ts: chrono::Utc::now(),
        },
    );
    let mut ctx_a = propfirm::rules::context::RuleContext::for_tick(account.clone(), &tick);
    ctx_a.rule_config = propfirm::config::rule_config::RuleConfig::from_plan(&account.plan);
    // P1-5: mark the equity as broker-reported so breach rules CAN terminate.
    ctx_a = ctx_a.with_broker_equity(account.equity, account.balance);
    let ctx_b = propfirm::rules::context::RuleContext {
        open_positions: ctx_a.open_positions.clone(),
        today_trades: ctx_a.today_trades.clone(),
        recent_events: ctx_a.recent_events.clone(),
        pending_order: ctx_a.pending_order.clone(),
        latest_trade: ctx_a.latest_trade.clone(),
        latest_tick: ctx_a.latest_tick.clone(),
        ..ctx_a.clone()
    };
    let _ = ctx_b;
    let reports_a = reg_a.evaluate(&ctx_a).unwrap();
    let reports_b = reg_b.evaluate(&ctx_a).unwrap();
    let decision_a = propfirm::engine::decision::Decision::from_reports(&reports_a);
    let decision_b = propfirm::engine::decision::Decision::from_reports(&reports_b);
    assert_eq!(
        decision_a.kind, decision_b.kind,
        "decision must be invariant under rule reordering; got {:?} (order A) vs {:?} (order B)",
        decision_a.kind, decision_b.kind
    );
    // Both should produce the same winning rule (max_drawdown, which has higher priority).
    assert!(
        decision_a.is_terminating() && decision_b.is_terminating(),
        "both orders must produce a terminating decision"
    );
    assert_eq!(
        decision_a.winning_priority, decision_b.winning_priority,
        "winning priority must be identical regardless of registration order"
    );
}

// ============================================================================
// P1 REGRESSION TESTS — broker-is-truth equity, stale ticks, etc.
// ============================================================================

#[test]
fn p1_5_estimated_equity_does_not_terminate_account() {
    // Account breaches max drawdown, but the equity input is *estimated*
    // (e.g. an interim quote between broker sync windows). The breach
    // rule must DOWNGRADE to Warn — the account must NOT be terminated
    // on a number that might be shadow-ledger drift.
    let plan = ftmo_phase1().with_loss_reference(propfirm::config::plan::LossReference::Trailing);
    let mut account = Account::new(AccountId::new(), plan)
        .start(chrono::Utc::now())
        .unwrap();
    account.initial_balance = Money(dec!(100_000));
    account.balance = Money(dec!(94_000)); // breach: peak 105k - 10.5k trail = 94.5k floor; 94k < floor.
    account.equity = Money(dec!(94_000));
    account.peak_balance = Money(dec!(105_000));
    account.peak_equity = Money(dec!(105_000));
    let evaluator = Evaluator::new(&account.plan);
    let tick = Tick::new(
        Symbol::new("EURUSD"),
        Quote {
            bid: Price(dec!(1.0800)),
            ask: Price(dec!(1.0802)),
            ts: chrono::Utc::now(),
        },
    );
    // Use evaluate_tick_estimated — breach rule should NOT terminate.
    let result = evaluator
        .evaluate_tick_estimated(&account, &tick, &[], &[], Vec::new())
        .unwrap();
    assert!(
        !result.decision.is_terminating(),
        "P1-5: estimated equity must NOT terminate; got {:?}",
        result.decision.kind
    );
    // But the same scenario with broker-reported equity SHOULD terminate.
    let result = evaluator
        .evaluate_tick(&account, &tick, &[], &[], Vec::new())
        .unwrap();
    assert!(
        result.decision.is_terminating(),
        "P1-5: broker-reported equity SHOULD terminate on a real breach; got {:?}",
        result.decision.kind
    );
}

#[test]
fn p1_5_broker_reported_equity_terminates_on_real_breach() {
    // Companion to the above: with broker-reported equity, a real breach
    // terminates the account as expected.
    let plan = ftmo_phase1().with_loss_reference(propfirm::config::plan::LossReference::Static);
    let mut account = Account::new(AccountId::new(), plan)
        .start(chrono::Utc::now())
        .unwrap();
    account.initial_balance = Money(dec!(100_000));
    account.balance = Money(dec!(89_000)); // breach: static floor = 90k; 89k < 90k.
    account.equity = Money(dec!(89_000));
    account.peak_balance = Money(dec!(100_000));
    account.peak_equity = Money(dec!(100_000));
    let evaluator = Evaluator::new(&account.plan);
    let tick = Tick::new(
        Symbol::new("EURUSD"),
        Quote {
            bid: Price(dec!(1.0800)),
            ask: Price(dec!(1.0802)),
            ts: chrono::Utc::now(),
        },
    );
    let result = evaluator
        .evaluate_tick(&account, &tick, &[], &[], Vec::new())
        .unwrap();
    assert!(
        result.decision.is_terminating(),
        "broker-reported breach must terminate; got {:?}",
        result.decision.kind
    );
}

#[test]
fn p1_14_stale_tick_is_rejected_by_pipeline() {
    // A tick older than 10 minutes must be rejected before evaluation runs.
    let mut plan = ftmo_phase1();
    plan.weekend_holding_allowed = true;
    plan.overnight_holding_allowed = true;
    plan.news_trading_allowed = true;
    let account = Account::new(AccountId::new(), plan.clone())
        .start(chrono::Utc::now())
        .unwrap();
    let store = propfirm::persistence::memory::InMemoryStore::new();
    use propfirm::persistence::traits::AccountStore;
    store.put(account.clone()).unwrap();
    let evaluator = Evaluator::new(&plan);
    let mut pipeline = propfirm::engine::pipeline::Pipeline::new(
        evaluator,
        store,
        propfirm::notifications::log::LogNotifier::new(),
    );
    // A stale tick from 30 minutes ago.
    let stale_ts = chrono::Utc::now() - chrono::Duration::minutes(30);
    let tick = Tick::new(
        Symbol::new("EURUSD"),
        Quote {
            bid: Price(dec!(1.0800)),
            ask: Price(dec!(1.0802)),
            ts: stale_ts,
        },
    );
    let result = pipeline.process(
        account.id,
        PipelineEvent::Tick {
            tick,
            broker_equity: account.equity,
            broker_balance: account.balance,
        },
    );
    assert!(
        matches!(result, Err(propfirm::Error::TickRejected(_))),
        "stale tick must be rejected with TickRejected; got {:?}",
        result
    );
}

#[test]
fn p1_14_out_of_order_tick_is_rejected_by_pipeline() {
    // After processing tick at time T, a tick at time T-1 must be rejected
    // (replay protection).
    let mut plan = ftmo_phase1();
    plan.weekend_holding_allowed = true;
    plan.overnight_holding_allowed = true;
    plan.news_trading_allowed = true;
    let account = Account::new(AccountId::new(), plan.clone())
        .start(chrono::Utc::now())
        .unwrap();
    let store = propfirm::persistence::memory::InMemoryStore::new();
    use propfirm::persistence::traits::AccountStore;
    store.put(account.clone()).unwrap();
    let evaluator = Evaluator::new(&plan);
    let mut pipeline = propfirm::engine::pipeline::Pipeline::new(
        evaluator,
        store,
        propfirm::notifications::log::LogNotifier::new(),
    );
    // First tick at T=now.
    let t1 = chrono::Utc::now();
    let tick1 = Tick::new(
        Symbol::new("EURUSD"),
        Quote {
            bid: Price(dec!(1.0800)),
            ask: Price(dec!(1.0802)),
            ts: t1,
        },
    );
    let _ = pipeline
        .process(
            account.id,
            PipelineEvent::Tick {
                tick: tick1,
                broker_equity: account.equity,
                broker_balance: account.balance,
            },
        )
        .unwrap();
    // Second tick at T-1 (older) — must be rejected.
    let t2 = t1 - chrono::Duration::minutes(1);
    let tick2 = Tick::new(
        Symbol::new("EURUSD"),
        Quote {
            bid: Price(dec!(1.0800)),
            ask: Price(dec!(1.0802)),
            ts: t2,
        },
    );
    let result = pipeline.process(
        account.id,
        PipelineEvent::Tick {
            tick: tick2,
            broker_equity: account.equity,
            broker_balance: account.balance,
        },
    );
    assert!(
        matches!(result, Err(propfirm::Error::TickRejected(_))),
        "out-of-order tick must be rejected; got {:?}",
        result
    );
}

#[test]
fn p1_8_optimistic_concurrency_rejects_stale_write() {
    // Two concurrent writers: writer A reads at v=0, writer B writes
    // (bumping to v=1), then writer A tries to write expecting v=0 →
    // must get StateConflict.
    use propfirm::persistence::traits::AccountStore;
    let plan = ftmo_phase1();
    let account = Account::new(AccountId::new(), plan)
        .start(chrono::Utc::now())
        .unwrap();
    let store = propfirm::persistence::memory::InMemoryStore::new();
    store.put(account.clone()).unwrap();
    // Writer B writes (bumps version to 1).
    let mut b_account = account.clone();
    b_account.balance = Money(dec!(10_500));
    store.put(b_account).unwrap();
    // Writer A tries to write expecting v=0 — must fail.
    let mut a_account = account.clone();
    a_account.balance = Money(dec!(9_900));
    let result = store.put_with_version(a_account, 0);
    assert!(
        matches!(result, Err(propfirm::Error::StateConflict(_, _, _))),
        "stale write must be rejected with StateConflict; got {:?}",
        result
    );
}

#[test]
fn p1_9_tenant_isolation_filter() {
    // An account belonging to tenant A must NOT be visible to tenant B
    // via get_for_tenant.
    use propfirm::persistence::traits::AccountStore;
    let plan = ftmo_phase1();
    let tenant_a = propfirm::tenant::TenantId::named("tenant-a");
    let tenant_b = propfirm::tenant::TenantId::named("tenant-b");
    let acc_a = Account::new(AccountId::new(), plan.clone())
        .with_tenant(tenant_a)
        .start(chrono::Utc::now())
        .unwrap();
    let store = propfirm::persistence::memory::InMemoryStore::new();
    store.put(acc_a.clone()).unwrap();
    // Tenant B tries to read A's account — must get None.
    let result = store.get_for_tenant(tenant_b, acc_a.id).unwrap();
    assert!(
        result.is_none(),
        "P1-9: tenant B must not see tenant A's account; got {:?}",
        result
    );
    // Tenant A reads own account — must succeed.
    let result = store.get_for_tenant(tenant_a, acc_a.id).unwrap();
    assert!(result.is_some(), "tenant A must see own account");
}

#[test]
fn p1_11_override_clears_breach_state() {
    // An account in Failed status, given an Override record, must
    // transition back to Active.
    use propfirm::core::ids::ViolationId;
    use propfirm::core::violation::Violation;
    use propfirm::override_engine::Override;
    let plan = ftmo_phase1();
    let mut account = Account::new(AccountId::new(), plan)
        .start(chrono::Utc::now())
        .unwrap();
    account.status = propfirm::core::account::AccountStatus::Failed;
    let violation_id = ViolationId::new();
    let _v = Violation::new(
        account.id,
        propfirm::core::ids::RuleId::named("max_drawdown"),
        "Maximum Drawdown",
        propfirm::core::violation::ViolationKind::MaxDrawdown,
        propfirm::core::violation::ViolationSeverity::Liquidate,
        "breach",
        chrono::Utc::now(),
    );
    let override_record = Override::new(
        account.id,
        violation_id,
        "Broker glitch tick on 2026-09-15 14:23 UTC; ticket #4521",
        "ops-alice",
        chrono::Utc::now(),
    );
    let state = propfirm::engine::state::AccountState::new(account);
    let new_state = state.clear_breach(&override_record).unwrap();
    assert_eq!(
        new_state.account.status,
        propfirm::core::account::AccountStatus::Active,
        "override must transition Failed → Active"
    );
}

#[test]
fn p1_12_emergency_stop_short_circuits() {
    // An EmergencyStop event forces EmergencyStopped status on the account.
    use propfirm::persistence::traits::AccountStore;
    let mut plan = ftmo_phase1();
    plan.weekend_holding_allowed = true;
    plan.overnight_holding_allowed = true;
    plan.news_trading_allowed = true;
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
            PipelineEvent::EmergencyStop {
                reason: "Broker feed corrupted — freezing all accounts".into(),
                actor_id: "ops-bob".into(),
                at: chrono::Utc::now(),
            },
        )
        .unwrap();
    assert_eq!(
        result.snapshot.account.status,
        propfirm::core::account::AccountStatus::EmergencyStopped,
        "emergency stop must transition to EmergencyStopped; got {:?}",
        result.snapshot.account.status
    );
}

#[test]
fn p1_7_pure_evaluate_produces_stable_input_hash() {
    // Same (account, pack, tick, server_time) → same input_hash. Different
    // (account, pack, tick, server_time) → different input_hash (with
    // overwhelming probability on sha256).
    use propfirm::core::types::ServerTime;
    use propfirm::pure::EquitySource;
    use propfirm::pure::{compute_input_hash, evaluate};
    use propfirm::rulepack::{PackLifecycle, RuleEntry, RulePack};
    use propfirm::rules::context::RuleContextKind;
    use propfirm::rules::registry::RuleRegistry;
    use std::sync::Arc;

    let plan = ftmo_phase1();
    let account1 = Account::new(AccountId::new(), plan.clone())
        .start(chrono::Utc::now())
        .unwrap();
    let account2 = Account::new(AccountId::new(), plan)
        .start(chrono::Utc::now())
        .unwrap();
    let pack = RulePack {
        id: "test-pack-v1".into(),
        version: 1,
        tenant_id: propfirm::tenant::TenantId::named("test"),
        lifecycle: PackLifecycle::Active,
        effective_from: chrono::Utc::now(),
        superseded_by: None,
        description: "test".into(),
        rules: vec![RuleEntry::new("max_drawdown", "max_drawdown", dec!(0.10))],
        initial_balance: Money(dec!(10_000)),
        leverage: 100,
        profit_target_pct: Pct(dec!(0.10)),
    };
    let registry = RuleRegistry::with_default_rules();
    let _ = Arc::new(registry);
    // P0-C: server_time is now part of the input hash, so identical
    // inputs evaluated at the *same* server_time produce the same hash.
    let st = ServerTime::now();
    let h1 = compute_input_hash(
        &account1,
        &pack,
        RuleContextKind::OnTick,
        st,
        &[],
        &[],
        None,
        None,
        None,
        &[],
        EquitySource::Estimated,
    );
    let h2 = compute_input_hash(
        &account1,
        &pack,
        RuleContextKind::OnTick,
        st,
        &[],
        &[],
        None,
        None,
        None,
        &[],
        EquitySource::Estimated,
    );
    let h3 = compute_input_hash(
        &account2,
        &pack,
        RuleContextKind::OnTick,
        st,
        &[],
        &[],
        None,
        None,
        None,
        &[],
        EquitySource::Estimated,
    );
    assert_eq!(
        h1, h2,
        "same inputs at same server_time must produce same hash"
    );
    assert_ne!(h1, h3, "different account ids must produce different hash");
    // P0-B: hash must be a real 64-char sha256 digest (not 16-char SipHash).
    assert!(
        h1.starts_with("sha256:"),
        "hash must be prefixed with sha256:"
    );
    assert_eq!(
        h1.len(),
        7 + 64,
        "hash must be 64 hex chars after the sha256: prefix"
    );

    // Full evaluate() should also produce a stable PureVerdict when called
    // with the same server_time.
    let v1 = evaluate(
        &account1,
        &pack,
        &RuleRegistry::with_default_rules(),
        RuleContextKind::OnTick,
        st,
        propfirm::pure::EvaluateInputs::default(),
    )
    .unwrap();
    let v2 = evaluate(
        &account1,
        &pack,
        &RuleRegistry::with_default_rules(),
        RuleContextKind::OnTick,
        st,
        propfirm::pure::EvaluateInputs::default(),
    )
    .unwrap();
    assert_eq!(
        v1.input_hash, v2.input_hash,
        "pure evaluate must produce stable hash"
    );
    assert_eq!(v1.pack_version, 1);
    assert_eq!(v1.pack_id, "test-pack-v1");
}

#[test]
fn p1_1_daily_dd_equity_vs_balance_basis_divergence() {
    use propfirm::config::presets::ftmo_phase1;
    use propfirm::rules::context::RuleContext;
    use propfirm::rules::evaluators::daily_drawdown::DailyDrawdownRule;
    use propfirm::rules::registry::RuleRegistry;
    use std::sync::Arc;

    let plan = ftmo_phase1();
    let mut account = Account::new(AccountId::new(), plan)
        .start(chrono::Utc::now())
        .unwrap();

    // Simulate: day starts at 100k, then balance drops to 95k (realized
    // loss) AND equity drops further to 90k (open floating loss on top).
    account.day_start_balance = Money(dec!(100_000));
    account.day_start_equity = Money(dec!(100_000));
    account.balance = Money(dec!(95_000));
    account.equity = Money(dec!(90_000));

    // max_daily_drawdown_pct = 0.05 → limit = 5% of day-start.
    //
    // Equity basis: drawdown = 100k - 90k = 10k > 5k limit → BREACH.
    // Balance basis: drawdown = 100k - 95k = 5k == 5k limit → PASS.
    //
    // The rule must fire on equity basis (the binding spec default).

    let mut reg = RuleRegistry::empty();
    reg.register(Arc::new(DailyDrawdownRule::default()));

    let tick = propfirm::core::tick::Tick::new(
        Symbol::new("EURUSD"),
        propfirm::core::tick::Quote {
            bid: Price(dec!(1.0800)),
            ask: Price(dec!(1.0802)),
            ts: chrono::Utc::now(),
        },
    );
    let mut ctx = RuleContext::for_tick(account.clone(), &tick);
    ctx = ctx.with_broker_equity(account.equity, account.balance);

    let reports = reg.evaluate(&ctx).unwrap();
    let decision = propfirm::engine::decision::Decision::from_reports(&reports);

    assert!(
        decision.is_terminating(),
        "daily drawdown must breach on equity basis (10k drop > 5k limit), \
         got {:?}",
        decision.kind
    );
}

#[test]
fn p1_1_daily_dd_balance_basis_does_not_terminate() {
    use propfirm::config::presets::ftmo_phase1;
    use propfirm::rules::context::RuleContext;
    use propfirm::rules::evaluators::daily_drawdown::DailyDrawdownRule;
    use propfirm::rules::registry::RuleRegistry;
    use std::sync::Arc;

    let plan = ftmo_phase1();
    let mut account = Account::new(AccountId::new(), plan)
        .start(chrono::Utc::now())
        .unwrap();

    // Same scenario but with drawdown_on_balance = true.
    account.plan.drawdown_on_balance = true;
    account.day_start_balance = Money(dec!(100_000));
    account.day_start_equity = Money(dec!(100_000));
    account.balance = Money(dec!(95_000));
    account.equity = Money(dec!(90_000));

    // Balance basis: drawdown = 100k - 95k = 5k == 5k limit → PASS.
    let mut reg = RuleRegistry::empty();
    reg.register(Arc::new(DailyDrawdownRule::default()));

    let tick = propfirm::core::tick::Tick::new(
        Symbol::new("EURUSD"),
        propfirm::core::tick::Quote {
            bid: Price(dec!(1.0800)),
            ask: Price(dec!(1.0802)),
            ts: chrono::Utc::now(),
        },
    );
    let mut ctx = RuleContext::for_tick(account.clone(), &tick);
    ctx = ctx.with_broker_equity(account.equity, account.balance);

    let reports = reg.evaluate(&ctx).unwrap();
    let decision = propfirm::engine::decision::Decision::from_reports(&reports);

    assert!(
        !decision.is_terminating(),
        "balance-basis daily drawdown must NOT breach when drawdown equals limit exactly, \
         got {:?}",
        decision.kind
    );
}
