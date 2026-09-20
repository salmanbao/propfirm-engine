//! Benchmarks for the engine.
//!
//! **P3 fix**: in addition to single-evaluate micro-benchmarks, this file
//! now includes a *realistic-load* benchmark that simulates the per-tick
//! request volume the platform expects to serve — 60-second cadence across
//! ~1,000 accounts, i.e. ~17 evaluations/second sustained. Run with
//! `cargo bench` to verify the engine can keep up.

use criterion::{black_box, criterion_group, criterion_main, Criterion, BenchmarkId};
use propfirm::config::presets::ftmo_phase1;
use propfirm::config::plan::LossReference;
use propfirm::core::account::Account;
use propfirm::core::ids::AccountId;
use propfirm::core::order::{Order, OrderKind, OrderSide, OrderStatus, OrderType, TimeInForce};
use propfirm::core::tick::{Quote, Tick};
use propfirm::core::types::{Money, Price, Quantity, Symbol, dec};
use propfirm::engine::evaluator::Evaluator;
use propfirm::rules::context::RuleContextKind;

fn evaluate_tick_bench(c: &mut Criterion) {
    let plan = ftmo_phase1().with_loss_reference(LossReference::Static);
    let account = Account::new(AccountId::new(), plan.clone());
    let evaluator = Evaluator::new(plan);
    let tick = Tick::new(Symbol::new("EURUSD"), Quote { bid: Price(dec!(1.08)), ask: Price(dec!(1.0802)), ts: chrono::Utc::now() });
    c.bench_function("evaluate_tick_single", |b| {
        b.iter(|| {
            let _ = black_box(evaluator.evaluate_tick(&account, &tick, &[], &[], Vec::new()).unwrap());
        });
    });
}

fn evaluate_order_bench(c: &mut Criterion) {
    let plan = ftmo_phase1().with_loss_reference(LossReference::Static);
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
    c.bench_function("evaluate_order_single", |b| {
        b.iter(|| {
            let _ = black_box(evaluator.evaluate_order(&account, &order, &[], &[], Vec::new()).unwrap());
        });
    });
}

/// **P3 fix**: realistic-load benchmark. Simulates the per-tick request
/// volume the platform expects to serve — 60s cadence across N accounts.
/// At 1000 accounts × 1 tick/60s = ~17 evals/sec. The bench processes
/// 1000 ticks in batch to measure throughput.
fn realistic_load_bench(c: &mut Criterion) {
    let mut group = c.benchmark_group("realistic_load");
    for n_accounts in [10, 100, 1000].iter() {
        group.throughput(criterion::Throughput::Elements(*n_accounts as u64));
        group.bench_with_input(BenchmarkId::from_parameter(n_accounts), n_accounts, |b, &n| {
            let plan = ftmo_phase1().with_loss_reference(LossReference::Static);
            let evaluator = Evaluator::new(plan.clone());
            let accounts: Vec<Account> = (0..n).map(|_| {
                Account::new(AccountId::new(), plan.clone()).start(chrono::Utc::now()).unwrap()
            }).collect();
            let tick = Tick::new(Symbol::new("EURUSD"), Quote {
                bid: Price(dec!(1.08)), ask: Price(dec!(1.0802)), ts: chrono::Utc::now(),
            });
            b.iter(|| {
                let mut total_decisions = 0u64;
                for acc in &accounts {
                    let result = evaluator.evaluate_tick(acc, &tick, &[], &[], Vec::new()).unwrap();
                    total_decisions += match result.decision.kind {
                        propfirm::engine::decision::DecisionKind::Pass => 0,
                        _ => 1,
                    };
                }
                black_box(total_decisions);
            });
        });
    }
    group.finish();
}

/// **P3 fix**: pure stateless evaluate benchmark. Measures the cost of the
/// pure function (no storage access) so we can compare against the
/// stateful pipeline.
fn pure_evaluate_bench(c: &mut Criterion) {
    let plan = ftmo_phase1().with_loss_reference(LossReference::Static);
    let account = Account::new(AccountId::new(), plan.clone()).start(chrono::Utc::now()).unwrap();
    use propfirm::rulepack::{RulePack, RuleEntry, RuleBasis, RuleUnit, PackLifecycle};
    let pack = RulePack {
        id: "bench-pack-v1".into(), version: 1,
        tenant_id: propfirm::tenant::TenantId::named("bench"),
        lifecycle: PackLifecycle::Active,
        effective_from: chrono::Utc::now(),
        superseded_by: None,
        description: "bench".into(),
        rules: vec![RuleEntry::new("max_drawdown", "max_drawdown", dec!(0.10))],
        initial_balance: Money(dec!(100_000)),
        leverage: 100,
        profit_target_pct: dec!(0.10).into(),
    };
    let registry = propfirm::rules::registry::RuleRegistry::with_default_rules();
    let tick = Tick::new(Symbol::new("EURUSD"), Quote { bid: Price(dec!(1.08)), ask: Price(dec!(1.0802)), ts: chrono::Utc::now() });
    let server_time = propfirm::core::types::ServerTime::now();
    c.bench_function("pure_evaluate", |b| {
        b.iter(|| {
            let _ = black_box(propfirm::pure::evaluate(
                &account, &pack, &registry,
                RuleContextKind::OnTick,
                server_time,
                &[], &[], Vec::new(),
                None, None, Some(&tick),
            ).unwrap());
        });
    });
}

criterion_group!(benches, evaluate_tick_bench, evaluate_order_bench, realistic_load_bench, pure_evaluate_bench);
criterion_main!(benches);
