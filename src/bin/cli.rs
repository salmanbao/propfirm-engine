//! CLI entry point: demonstrates a full evaluation cycle.

use propfirm::prelude::*;
use propfirm::config::presets::ftmo_phase1;
use propfirm::core::order::{Order, OrderKind, OrderSide, OrderType, TimeInForce};
use propfirm::core::tick::{Quote, Tick};
use propfirm::core::types::{Money, Price, Quantity, Symbol, dec};
use propfirm::engine::evaluator::Evaluator;
use propfirm::engine::pipeline::{Pipeline, PipelineEvent};
use propfirm::notifications::log::LogNotifier;
use propfirm::persistence::memory::InMemoryStore;
use propfirm::persistence::traits::AccountStore;

fn main() -> anyhow::Result<()> {
    println!("=== Prop Firm Engine – CLI Demo ===\n");

    // 1. Build the challenge plan and account.
    let plan = ftmo_phase1();
    let account = Account::new(AccountId::new(), plan.clone());
    println!("Account: id={} type={} phase={} initial={}", account.id, account.account_type, plan.phase, account.initial_balance);
    println!("Plan: profit_target={} daily_dd={} max_dd={} min_days={:?} time_limit_days={:?}",
        plan.profit_target_pct, plan.max_daily_drawdown_pct, plan.max_total_drawdown_pct,
        plan.min_trading_days, plan.time_limit_days);

    // 2. Build the pipeline.
    let evaluator = Evaluator::new(plan.clone());
    let store = InMemoryStore::new();
    store.put(account.clone())?;
    let notifier = LogNotifier::new();
    let mut pipeline = Pipeline::new(evaluator, store, notifier);

    // 3. Start the account.
    let now = chrono::Utc::now();
    let result = pipeline.process(account.id, PipelineEvent::AccountStarted { at: now })?;
    println!("\n[Started] decision={:?} events={}", result.snapshot.decision.kind, result.events.len());

    // 4. Open a long EURUSD position.
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
        comment: Some("demo".into()),
        submitted_at: now,
        status: propfirm::core::order::OrderStatus::Pending,
        filled_quantity: Quantity::ZERO,
        avg_fill_price: None,
    };
    let result = pipeline.process(account.id, PipelineEvent::OrderSubmitted { order })?;
    println!("[Order] decision={:?} passed={} violations={}", result.snapshot.decision.kind, result.result.passed(), result.result.violations().len());

    // 5. A tick comes in (positive move).
    let tick = Tick::new(
        Symbol::new("EURUSD"),
        Quote { bid: Price(dec!(1.0850)), ask: Price(dec!(1.0852)), ts: now },
    );
    let result = pipeline.process(account.id, PipelineEvent::Tick { tick })?;
    println!("[Tick] equity={} balance={} daily_dd={}/{}",
        result.snapshot.account.equity,
        result.snapshot.account.balance,
        result.snapshot.account.daily_drawdown,
        account.daily_dd_limit());

    // 6. Risk metrics computation.
    let equity_curve = vec![Money(dec!(10_000)), Money(dec!(10_200)), Money(dec!(10_150)), Money(dec!(10_300))];
    let risk = propfirm::risk::metrics::RiskMetrics::compute(&equity_curve, &[Money(dec!(100)), Money(dec!(-50)), Money(dec!(150))]);
    println!("\n[Risk] sharpe={:.4} sortino={:.4} max_dd={:.4} profit_factor={:.4} win_rate={:.2}%",
        risk.sharpe, risk.sortino, risk.max_drawdown, risk.profit_factor, risk.win_rate * dec!(100));

    println!("\nDone. Engine worked end-to-end. ✓");
    Ok(())
}
