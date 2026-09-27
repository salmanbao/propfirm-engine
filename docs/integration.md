# Integration guide

This guide shows how to embed the prop firm engine in a Rust service, run evaluations, carry account state in and out (ADR-11), and hook into the HTTP API.

## Prerequisites

- Rust 1.70+ (2021 edition)

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
| `serialization` | Enables `serde`/`serde_json`/`chrono/serde` for JSON config and request/response payloads. |
| `in-memory-store` | Vestigial (kept so existing build commands keep working); gates nothing — there is no account store (ADR-11). |
| `server` | Enables the `axum` HTTP server and all REST endpoints. |
| `tracing` | Enables `tracing`/`tracing-subscriber` for structured audit logging. |

For a typical embedded use (no HTTP server):

```toml
propfirm-engine = { version = "0.1", default-features = false, features = ["serialization"] }
```

For the HTTP server:

```toml
propfirm-engine = { version = "0.1", default-features = false, features = ["serialization", "server", "tracing"] }
```

## Quick start

```rust
use propfirm::prelude::*;
use propfirm::config::presets::ftmo_phase1;
use propfirm::engine::evaluator::Evaluator;
use propfirm::engine::pipeline::{Pipeline, PipelineEvent};
use propfirm::notifications::log::LogNotifier;
use propfirm::tenant::TenantId;

// Pipeline::process is async; requires a tokio runtime (e.g. #[tokio::main]).
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // 1. Choose a preset plan and create an account (ADR-11: state is
    //    caller-owned; the engine never persists it).
    let plan = ftmo_phase1();
    let account = Account::new(AccountId::new(), plan.clone())
        .with_tenant(TenantId::named("my-firm"));

    // 2. Build the evaluator and pipeline (no store — domain events come
    //    back on PipelineResult.events for you to persist if you wish).
    let evaluator = Evaluator::new(&plan);
    let mut pipeline = Pipeline::new(evaluator, LogNotifier::new());
    let now = chrono::Utc::now();

    // 3. Start the account: Pending → Active.
    let result = pipeline
        .process(account.clone(), PipelineEvent::AccountStarted { at: now })
        .await?;
    println!("Started: {:?}", result.snapshot.account.status);

    // 4. Evaluate a tick (broker-reported equity).
    let tick = Tick::new(
        Symbol::new("EURUSD"),
        Quote {
            bid: Price(dec!(1.0850)),
            ask: Price(dec!(1.0852)),
            ts: now,
        },
    );
    let result = pipeline
        .process(
            result.account.clone(),
            PipelineEvent::Tick {
                tick,
                broker_equity: Money(dec!(102_000)),
                broker_balance: Money(dec!(100_000)),
            },
        )
        .await?;
    println!("Tick decision: {:?}", result.snapshot.decision.kind);

    // 5. Evaluate an order (pre-trade, SL/TP set).
    let order = Order::market_open(
        account.id,
        Symbol::new("EURUSD"),
        OrderSide::Buy,
        Quantity(dec!(1)),
        Some(Price(dec!(1.05))),
        Some(Price(dec!(1.10))),
        now,
    );
    let result = pipeline
        .process(result.account.clone(), PipelineEvent::OrderSubmitted { order })
        .await?;
    println!("Order decision: {:?}", result.snapshot.decision.kind);

    Ok(())
}
```

## Stateless evaluate

If you want to evaluate without mutating any state, use `pure::evaluate()`. It takes all inputs explicitly and returns a `PureVerdict` with an `input_hash`.

```rust
use propfirm::prelude::*;
use propfirm::config::presets::ftmo_phase1;
use propfirm::pure::{evaluate, EvaluateInputs};
use propfirm::rulepack::RulePack;
use propfirm::rules::context::RuleContextKind;
use propfirm::rules::registry::RuleRegistry;

fn main() -> anyhow::Result<()> {
    let plan = ftmo_phase1();
    let account = Account::new(AccountId::new(), plan.clone());

    // Rule packs are data: production callers deserialize their stored pack
    // JSON; here we synthesize one from the plan the account is bound to.
    let pack = RulePack::synthetic_from_plan(account.id, account.tenant_id.clone(), &plan);
    let registry = RuleRegistry::with_default_rules();
    let tick = Tick::new(
        Symbol::new("EURUSD"),
        Quote {
            bid: Price(dec!(1.08)),
            ask: Price(dec!(1.0802)),
            ts: chrono::Utc::now(),
        },
    );

    let verdict = evaluate(
        &account,
        &pack,
        &registry,
        RuleContextKind::OnTick,
        propfirm::core::types::ServerTime::now(),
        EvaluateInputs::for_tick(&[], &[], &tick),
    )?;

    // verdict.input_hash can be persisted by the caller and verified on replay.
    println!("verdict: {:?}, hash: {}", verdict.decision.kind, verdict.input_hash);
    Ok(())
}
```

## Persistence

Account persistence lives with the caller (ADR-11): `/internal/v1/evaluate` receives `account_state` and returns the updated state; storing it between calls is your responsibility. What the engine keeps:

- **Event store** — `events::store::EventStore` (async trait: `append`, `all`, `recent`, `replay`) with an in-memory implementation in `propfirm::persistence::memory`. This is the read-side seam behind breach-report and override replay.
- **Idempotency store** — in-memory `IdempotencyStore` in `propfirm::api::idempotency`, behind the `Idempotency-Key` header on `POST /internal/v1/evaluate`.

There is no `postgres` feature, no `AccountStore`, and no `put_with_version`. For your own optimistic-concurrency layer, carry `Account.version` inside `account_state`: it is hashed into every verdict's `input_hash`, so replaying a stale state is detectable (`Error::StateConflict` is retained as the typed error for that purpose).

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
| `POST` | `/internal/v1/evaluate` | service | Stateless evaluate contract (`account_state` required; a `rule_pack` field is rejected with 400). |
| `POST` | `/internal/v1/override` | service | Clear a false-positive breach. |
| `POST` | `/internal/v1/manual-run` | service | Force re-evaluation of an account. |
| `POST` | `/internal/v1/emergency-stop` | service | Force `DecisionKind::Emergency`, short-circuiting rule evaluation. |
| `GET` | `/internal/v1/breach-report/:account_id` | service | Trader-facing "why did I fail" view. |
| `POST` | `/v1/evaluate-order` | tenant/service | Pre-trade order evaluation. |
| `POST` | `/v1/rule-packs/validate` | tenant/service | Validate a rule pack (stateless — nothing is persisted). |

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
use propfirm::core::violation::Violation;
use propfirm::core::Error;
use propfirm::notifications::traits::Notifier;

struct WebhookNotifier {
    client: reqwest::Client,
    url: String,
}

impl Notifier for WebhookNotifier {
    fn notify_violation(&self, v: &Violation) -> Result<(), Error> {
        self.client
            .post(&self.url)
            .json(v)
            .send()
            .map_err(|e| Error::Persistence(e.to_string()))?;
        Ok(())
    }

    fn notify_account_event(
        &self,
        _account_id: propfirm::core::ids::AccountId,
        _kind: &str,
        _msg: &str,
    ) -> Result<(), Error> {
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

- `Error::Persistence(msg)` — storage failure (event store, idempotency backend).
- `Error::StateConflict(id, expected, actual)` — optimistic concurrency violation; retained for caller-side use (ADR-11: the stateless contract never produces it).
- `Error::TickRejected(reason)` — stale or out-of-order tick.
- `Error::RuleEval(msg)` — rule evaluation error.
- `Error::InvalidConfig(msg)` — configuration validation failure.
- `Error::NotFound(msg)` — resource not found.
