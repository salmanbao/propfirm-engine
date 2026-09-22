# Integration guide

This guide shows how to embed the prop firm engine in a Rust service, run evaluations, persist state, and hook into the HTTP API.

## Prerequisites

- Rust 1.70+ (2021 edition)
- Optional: PostgreSQL 12+ if you use the `postgres` feature

## Adding the dependency

```toml
[dependencies]
propfirm-engine = { git = "https://github.com/salmanbao/propfirm-engine", optional = true }
```

Or from crates.io when published:

```toml
[dependencies]
propfirm-engine = "0.1"
```

## Feature flags

| Feature | Description |
|---------|-------------|
| `default` | Enables `serialization` and `in-memory-store`. |
| `serialization` | Enables `serde`/`serde_json`/`chrono/serde` for JSON config and persistence. |
| `in-memory-store` | Enables the in-memory `AccountStore` implementation. |
| `postgres` | Enables `sqlx`, `sqlx-postgres`, `tokio-postgres`, `deadpool-postgres`. |
| `server` | Enables the `axum` HTTP server and all REST endpoints. |
| `tracing` | Enables `tracing`/`tracing-subscriber` for structured audit logging. |

For a typical embedded use (no HTTP server, in-memory persistence):

```toml
propfirm-engine = { version = "0.1", default-features = false, features = ["serialization", "in-memory-store"] }
```

For production with Postgres:

```toml
propfirm-engine = { version = "0.1", default-features = false, features = ["serialization", "postgres", "tracing"] }
```

## Quick start

```rust
use propfirm::prelude::*;
use propfirm::config::presets::ftmo_phase1;
use propfirm::core::ids::AccountId;
use propfirm::engine::evaluator::Evaluator;
use propfirm::engine::pipeline::{Pipeline, PipelineEvent};
use propfirm::notifications::log::LogNotifier;
use propfirm::persistence::memory::InMemoryStore;
use propfirm::persistence::traits::AccountStore;

fn main() -> anyhow::Result<()> {
    // 1. Choose a preset plan.
    let plan = ftmo_phase1();

    // 2. Create an account.
    let mut account = Account::new(AccountId::new(), plan.clone())
        .with_tenant(TenantId::named("my-firm"));
    account.balance = Money(dec!(100_000));
    account.equity = account.balance;

    // 3. Build the evaluator and pipeline.
    let evaluator = Evaluator::new(&plan);
    let store = InMemoryStore::new();
    store.put(account.clone())?;
    let notifier = LogNotifier::new();
    let mut pipeline = Pipeline::new(evaluator, store, notifier);

    // 4. Start the account.
    pipeline.process(
        account.id,
        PipelineEvent::AccountStarted { at: chrono::Utc::now() },
    )?;

    // 5. Evaluate a tick (broker-reported equity).
    let tick = Tick::new(
        Symbol::new("EURUSD"),
        Quote {
            bid: Price(dec!(1.0850)),
            ask: Price(dec!(1.0852)),
            ts: chrono::Utc::now(),
        },
    );
    let result = pipeline.process(
        account.id,
        PipelineEvent::Tick {
            tick,
            broker_equity: Money(dec!(102_000)),
            broker_balance: Money(dec!(100_000)),
        },
    )?;
    println!("Tick decision: {:?}", result.snapshot.decision.kind);

    // 6. Evaluate an order (pre-trade).
    let order = Order::market_open(
        account.id,
        Symbol::new("EURUSD"),
        OrderSide::Buy,
        Quantity(dec!(1)),
        Some(Price(dec!(1.05))),
        Some(Price(dec!(1.10))),
        chrono::Utc::now(),
    );
    let result = pipeline.process(account.id, PipelineEvent::OrderSubmitted { order })?;
    println!("Order decision: {:?}", result.snapshot.decision.kind);

    Ok(())
}
```

## Stateless evaluate

If you want to evaluate without mutating any state, use `pure::evaluate()`. It takes all inputs as arguments and returns a `PureVerdict` with an `input_hash`.

```rust
use propfirm::pure::evaluate;
use propfirm::config::presets::ftmo_phase1;
use propfirm::core::ids::AccountId;

fn main() -> anyhow::Result<()> {
    let plan = ftmo_phase1();
    let account = Account::new(AccountId::new(), plan);
    let pack = RulePack::default_for_plan(&plan)?;

    let verdict = evaluate(
        &account,
        &pack,
        &tick,
        &open_positions,
        &today_trades,
        pending_order,
        latest_trade,
        latest_tick,
        cross_reference_trades,
        equity_source,
    )?;

    // verdict.input_hash can be persisted and verified on replay.
    println!("verdict: {:?}, hash: {}", verdict.decision.kind, verdict.input_hash);
    Ok(())
}
```

## Persistence

### In-memory (testing)

```rust
use propfirm::persistence::memory::InMemoryStore;
use propfirm::persistence::traits::AccountStore;

let store = InMemoryStore::new();
store.put(account)?;
let loaded = store.get(account.id)?.expect("account must exist");
```

### Postgres (production)

Enable the `postgres` feature and use `PostgresStore`:

```rust
use propfirm::persistence::postgres::PostgresStore;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let store = PostgresStore::connect("postgres://propfirm:password@localhost/propfirm").await?;
    // store implements AccountStore, PositionStore, TradeStore, RulePackStore, etc.
    Ok(())
}
```

`PostgresStore` enforces tenant isolation at the SQL layer. Use `get_for_tenant(tenant_id, account_id)` instead of `get(account_id)` in multi-tenant deployments.

Optimistic concurrency:

```rust
use propfirm::persistence::traits::AccountStore;

// Only succeeds if the account version is still 5.
store.put_with_version(account, 5)?;
```

## HTTP API

Enable the `server` feature and start the HTTP server:

```rust
use propfirm::api::server::run_server;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    run_server("0.0.0.0:8080", ftmo_phase1()).await?;
    Ok(())
}
```

Authentication is configured via environment variables:

```bash
export PROPFIRM_SERVICE_TOKENS='{"web":"active-digest","relay":"active-digest"}'
export PROPFIRM_ALLOW_INSECURE=0
```

If no tokens are configured and `PROPFIRM_ALLOW_INSECURE` is not set, the server fails closed and refuses to start.

### Endpoints

| Method | Path | Auth | Description |
|--------|------|------|-------------|
| `GET` | `/health` | none | Liveness probe. |
| `GET` | `/ready` | none | Readiness probe. |
| `POST` | `/internal/v1/evaluate` | service | Stateless evaluate contract. |
| `POST` | `/internal/v1/override` | service | Clear a false-positive breach. |
| `POST` | `/internal/v1/manual-run` | service | Force re-evaluation of an account. |
| `GET` | `/internal/v1/breach-report/:account_id` | service | Trader-facing "why did I fail" view. |
| `POST` | `/v1/evaluate-order` | tenant/service | Pre-trade order evaluation. |
| `GET` | `/v1/accounts/:id` | tenant/service | Account snapshot. |
| `POST` | `/v1/rule-packs` | tenant/service | Create a draft rule pack. |
| `GET` | `/v1/rule-packs/:id` | tenant/service | Get a rule pack. |
| `PATCH` | `/v1/rule-packs/:id` | tenant/service | Update a draft rule pack. |
| `POST` | `/v1/rule-packs/:id/activate` | tenant/service | Promote draft → active. |
| `POST` | `/v1/rule-packs/:id/supersede` | tenant/service | Mark active → superseded. |

All mutating endpoints accept an `Idempotency-Key` header.

## Rule packs

Rule packs are versioned JSON data, not compiled Rust. Tenant admins can edit thresholds through a form, bind a pack to an account at purchase, and re-bind only via an explicit, audited action.

```rust
use propfirm::rulepack::{RulePack, RuleEntry, PackLifecycle};
use propfirm::core::types::Money;

let pack = RulePack {
    id: "funderblu-default-v3".into(),
    version: 3,
    tenant_id: TenantId::named("my-firm"),
    lifecycle: PackLifecycle::Active,
    effective_from: chrono::Utc::now(),
    superseded_by: None,
    description: "Default challenge plan".into(),
    rules: vec![
        RuleEntry {
            id: "max_total_loss".into(),
            kind: "max_drawdown".into(),
            basis: "static".into(),
            unit: "percent".into(),
            value: 0.10,
            tolerance_cents: 1,
            early_warning_pct: Some(0.80),
            priority: Some(1000),
            enabled: Some(true),
        },
        // ... more rules
    ],
    initial_balance: Money(dec!(100_000)),
    leverage: 100,
    profit_target_pct: Pct(dec!(0.08)),
};
```

Build a registry from a pack:

```rust
use propfirm::rules::registry::RuleRegistry;

let registry = RuleRegistry::build_from_pack(&pack)?;
let evaluator = Evaluator::with_registry(registry);
```

## Notifications

Implement the `Notifier` trait to deliver violations:

```rust
use propfirm::notifications::Notifier;
use propfirm::engine::evaluator::EvaluationResult;

struct WebhookNotifier {
    client: reqwest::Client,
    url: String,
}

impl Notifier for WebhookNotifier {
    fn notify(&self, result: &EvaluationResult) -> anyhow::Result<()> {
        for violation in result.violations() {
            self.client.post(&self.url).json(violation).send()?;
        }
        Ok(())
    }
}
```

## Testing

Run the full test suite:

```bash
cargo test --all-features
```

Run specific test files:

```bash
cargo test --features serialization,in-memory-store --test property_tests
cargo test --features server --test auth_a1
```

Run benchmarks:

```bash
cargo bench --features serialization,in-memory-store
```

## Error handling

All fallible operations return `propfirm::Result<T>`. The error type is `propfirm::Error`:

- `Error::Persistence(msg)` — storage failure.
- `Error::StateConflict(id, expected, actual)` — optimistic concurrency violation.
- `Error::TickRejected(reason)` — stale or out-of-order tick.
- `Error::RuleEval(msg)` — rule evaluation error.
- `Error::InvalidConfig(msg)` — configuration validation failure.
- `Error::NotFound(msg)` — resource not found.
