//! Benchmarks for the engine.

use criterion::{black_box, criterion_group, criterion_main, Criterion};
use propfirm::config::presets::ftmo_phase1;
use propfirm::core::account::Account;
use propfirm::core::ids::AccountId;
use propfirm::core::order::{Order, OrderKind, OrderSide, OrderStatus, OrderType, TimeInForce};
use propfirm::core::tick::{Quote, Tick};
use propfirm::core::types::{Price, Quantity, Symbol, dec};
use propfirm::engine::evaluator::Evaluator;

fn evaluate_tick_bench(c: &mut Criterion) {
    let plan = ftmo_phase1();
    let account = Account::new(AccountId::new(), plan.clone());
    let evaluator = Evaluator::new(plan);
    let tick = Tick::new(Symbol::new("EURUSD"), Quote { bid: Price(dec!(1.08)), ask: Price(dec!(1.0802)), ts: chrono::Utc::now() });
    c.bench_function("evaluate_tick", |b| {
        b.iter(|| {
            let _ = black_box(evaluator.evaluate_tick(&account, &tick, &[], &[], Vec::new()).unwrap());
        });
    });
}

fn evaluate_order_bench(c: &mut Criterion) {
    let plan = ftmo_phase1();
    let account = Account::new(AccountId::new(), plan.clone());
    let evaluator = Evaluator::new(plan);
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
        submitted_at: chrono::Utc::now(),
        status: OrderStatus::Pending,
        filled_quantity: Quantity::ZERO,
        avg_fill_price: None,
    };
    c.bench_function("evaluate_order", |b| {
        b.iter(|| {
            let _ = black_box(evaluator.evaluate_order(&account, &order, &[], &[], Vec::new()).unwrap());
        });
    });
}

criterion_group!(benches, evaluate_tick_bench, evaluate_order_bench);
criterion_main!(benches);
