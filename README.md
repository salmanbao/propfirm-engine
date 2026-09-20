# Prop Firm Risk & Rule Evaluation Engine

An enterprise-grade, fully-typed Rust library for evaluating proprietary trading firm rules, monitoring account risk, and producing audit-grade decisions in real time.

## Features

- **Comprehensive rule library** (19 rules out of the box):
  - Daily drawdown, max drawdown, trailing drawdown
  - Profit target, minimum trading days, consistency
  - News trading, overnight holding, weekend holding
  - Max position size, max open positions, max daily trades
  - Time limit, cooldown, hedging, grid trading, copy trading
  - Stop-loss / take-profit required

- **Challenge plan presets**: FTMO, MyForexFunds, The Funded Trader, SurgeTrader, custom.

- **Risk analytics**: Sharpe ratio, Sortino ratio, Calmar ratio, max drawdown, profit factor, expectancy, win rate, Z-score, recovery factor, parametric VaR, expected shortfall, exposure analytics.

- **Persistence**: in-memory store with a clean trait for SQL / KV extensions.

- **Event sourcing**: append-only audit log with replay support for full account state reconstruction.

- **Notifications**: pluggable notifier trait with a `LogNotifier` for tests; wire in webhook/email/push in production.

- **Reporting**: structured `PerformanceReport` combining snapshot, risk metrics, and decision.

- **Optional HTTP API** (`server` feature): axum-based REST endpoints for order evaluation and account snapshots.

- **Decimal precision**: all monetary values use `rust_decimal::Decimal` — no floating-point drift.

- **Concurrency-safe**: thread-safe via `Arc<RwLock<...>>` and `parking_lot`.

## Quick Start

```bash
# Build
cargo build --release --all-features

# Run the CLI demo
cargo run --release --bin propfirm-cli

# Run the HTTP server (optional)
cargo run --release --features server --bin propfirm-server

# Run tests
cargo test --all-features

# Run benchmarks
cargo bench
```

## Example

```rust
use propfirm::prelude::*;
use propfirm::config::presets::ftmo_phase1;
use propfirm::core::order::{Order, OrderKind, OrderSide, OrderType, TimeInForce};
use propfirm::core::tick::{Quote, Tick};
use propfirm::core::types::{Price, Quantity, Symbol, dec};
use propfirm::engine::evaluator::Evaluator;
use propfirm::engine::pipeline::{Pipeline, PipelineEvent};
use propfirm::notifications::log::LogNotifier;
use propfirm::persistence::memory::InMemoryStore;

fn main() -> anyhow::Result<()> {
    let plan = ftmo_phase1();
    let account = Account::new(AccountId::new(), plan.clone());
    let evaluator = Evaluator::new(plan);
    let store = InMemoryStore::new();
    store.put(account.clone())?;
    let mut pipeline = Pipeline::new(evaluator, store, LogNotifier::new());

    pipeline.process(account.id, PipelineEvent::AccountStarted { at: chrono::Utc::now() })?;

    let order = Order::market_open(
        account.id, Symbol::new("EURUSD"), OrderSide::Buy,
        Quantity(dec!(1)),
        Some(Price(dec!(1.05))), Some(Price(dec!(1.10))),
        chrono::Utc::now(),
    );
    let result = pipeline.process(account.id, PipelineEvent::OrderSubmitted { order })?;
    println!("Order decision: {:?} (passed={})", result.snapshot.decision.kind, result.passed());
    Ok(())
}
```

## Architecture

```
┌──────────────────────────────────────────────────────────┐
│                     HTTP API (axum)                      │
│              /v1/evaluate-order, /v1/accounts/:id         │
└────────────────────────┬─────────────────────────────────┘
                         │
                         ▼
┌──────────────────────────────────────────────────────────┐
│                     Pipeline                             │
│  apply_event → build_context → evaluate → persist →      │
│  emit_decision_event → notify                            │
└───────┬────────────┬──────────────┬──────────────────────┘
        │            │              │
        ▼            ▼              ▼
┌──────────┐  ┌──────────┐  ┌──────────────┐
│ Evaluator│  │  State   │  │ Event Store  │
│ +Rules   │  │  Delta   │  │ (audit log)  │
└────┬─────┘  └──────────┘  └──────────────┘
     │
     ▼
┌──────────────────────────────────────────────────────────┐
│                   Rule Registry                          │
│  Daily DD | Max DD | Trailing DD | Profit Target |      │
│  Min Days | Consistency | News | Overnight | Weekend |  │
│  Max Pos Size | Max Open | Max Daily Trades | Time |    │
│  Cooldown | Hedging | Grid | Copy Trading | SL/TP |     │
└──────────────────────────────────────────────────────────┘
```

## Configuration

Each challenge plan is a `ChallengePlan` struct with builder methods:

```rust
use propfirm::config::plan::ChallengePlan;
use propfirm::prelude::*;

let plan = ChallengePlan::default()
    .with_balance(Money(dec!(100_000)))
    .with_profit_target(Pct(dec!(0.08)))
    .with_daily_dd(Pct(dec!(0.05)))
    .with_total_dd(Pct(dec!(0.10)))
    .with_min_days(3)
    .with_time_limit_days(30)
    .with_consistency(Pct(dec!(0.40)));
```

## License

MIT OR Apache-2.0
