//! Example: build a custom challenge plan and run an evaluation.

use propfirm::config::plan::{ChallengePhase, ChallengePlan};
use propfirm::config::rule_config::RuleConfig;
use propfirm::core::order::{Order, OrderKind, OrderSide, OrderStatus, OrderType, TimeInForce};
use propfirm::core::tick::{Quote, Tick};
use propfirm::core::types::{dec, Money, Price, Quantity, Symbol};
use propfirm::engine::evaluator::Evaluator;
use propfirm::prelude::*;
use propfirm::rules::context::RuleContext;

fn main() -> anyhow::Result<()> {
    // Custom plan: $25k balance, 8% target, 4% daily DD, 8% total DD, 5 day min.
    let plan = ChallengePlan::default()
        .with_balance(Money(dec!(25_000)))
        .with_phase(ChallengePhase::Phase1)
        .with_profit_target(Pct(dec!(0.08)))
        .with_daily_dd(Pct(dec!(0.04)))
        .with_total_dd(Pct(dec!(0.08)))
        .with_min_days(5)
        .with_time_limit_days(30)
        .with_trailing_dd(Pct(dec!(0.08)))
        .with_consistency(Pct(dec!(0.40)));
    plan.validate()?;
    println!("Custom plan validated: {plan:?}");

    let account = Account::new(AccountId::new(), plan.clone()).start(chrono::Utc::now())?;
    let evaluator = Evaluator::new(plan);

    // Submit an order with SL set.
    let order = Order {
        id: propfirm::core::ids::OrderId::new(),
        account_id: account.id,
        symbol: Symbol::new("EURUSD"),
        side: OrderSide::Buy,
        kind: OrderKind::Open,
        order_type: OrderType::Market,
        quantity: Quantity(dec!(2)),
        tif: TimeInForce::Ioc,
        stop_loss: Some(Price(dec!(1.05))),
        take_profit: Some(Price(dec!(1.10))),
        comment: Some("example".into()),
        submitted_at: chrono::Utc::now(),
        status: OrderStatus::Pending,
        filled_quantity: Quantity::ZERO,
        avg_fill_price: None,
    };
    let mut ctx = RuleContext::for_open_order(account.clone(), &order);
    ctx.rule_config = RuleConfig::from_plan(&account.plan);
    let result = evaluator.evaluate(&ctx)?;
    println!(
        "Order evaluation: decision={:?} passed={}",
        result.decision.kind,
        result.passed()
    );
    for r in &result.reports {
        println!(
            "  rule={} verdict-kind={}",
            r.rule_name,
            match &r.verdict {
                propfirm::rules::traits::RuleVerdict::Pass => "pass",
                propfirm::rules::traits::RuleVerdict::Warn(_) => "warn",
                propfirm::rules::traits::RuleVerdict::Fail(_) => "fail",
                propfirm::rules::traits::RuleVerdict::Liquidate(_) => "liquidate",
                propfirm::rules::traits::RuleVerdict::TargetHit(_) => "target_hit",
                propfirm::rules::traits::RuleVerdict::Emergency(_) => "emergency",
                propfirm::rules::traits::RuleVerdict::EarlyWarning(_) => "early_warning",
                propfirm::rules::traits::RuleVerdict::Skip => "skip",
            }
        );
    }

    // Evaluate a tick.
    let tick = Tick::new(
        Symbol::new("EURUSD"),
        Quote {
            bid: Price(dec!(1.0850)),
            ask: Price(dec!(1.0852)),
            ts: chrono::Utc::now(),
        },
    );
    let result = evaluator.evaluate_tick(&account, &tick, &[], &[], Vec::new())?;
    println!(
        "Tick evaluation: equity={} decision={:?}",
        account.equity, result.decision.kind
    );

    Ok(())
}
