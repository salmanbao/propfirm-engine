# Integration guide

This guide shows how to embed the prop firm engine in a Rust service, run evaluations, carry account state in and out (ADR-11), and hook into the HTTP API.

**Version: 0.2.0** — the engine is released as an **internal component**
of the Prop Firm as a Service Platform. There is no in-process
authentication: the engine is reached only from the platform backend
over the private compose network (HTTP) or the centralized Redis event
bus (worker). Trust is established at the network boundary, optionally
strengthened with mTLS (see `docs/tls.md`).

## Prerequisites

- Rust 1.70+ (2021 edition)
- (For the server / worker) Postgres 14+ and Redis 6.2+

## Adding the dependency

```toml
[dependencies]
propfirm-engine = { git = "https://github.com/salmanbao/propfirm-engine", optional = true }
```

Or from crates.io when published:

```toml
[dependencies]
propfirm-engine = "0.2"
```

## Feature flags

| Feature | Description |
|---------|-------------|
| `default` | Enables `serialization` + `in-memory-store`. |
| `serialization` | Enables `serde`/`serde_json`/`chrono/serde` for JSON config and request/response payloads. |
| `server` | Enables the `axum` HTTP server, the `propfirm-worker` binary, durable persistence (`sqlx` + `redis` + `bb8` + `bb8-redis`), `rustls` TLS, `metrics-exporter-prometheus`, `figment` config loading, `dotenvy`. |
| `otel` | Enables `tracing-opentelemetry` + `opentelemetry-otlp` (gRPC/HTTP) for exporting spans to a collector. |
| `flame` | Enables `tracing-flame` for flame-graph profiling (writes a trace to `observability.flame_output_path`). |
| `openapi` | Enables `utoipa` + `utoipa-swagger-ui`; serves `GET /openapi.json` + `GET /swagger-ui/`. |
| `tracing` | Enables `tracing`/`tracing-subscriber` for structured logging. |
| `tokio-cli` | Enables the `propfirm-cli` binary (tokio runtime). |
| `in-memory-store` | Vestigial (kept so existing build commands keep working); gates nothing in v0.2.0. |

For a typical embedded use (no HTTP server):

```toml
propfirm-engine = { version = "0.2", default-features = false, features = ["serialization"] }
```

For the HTTP server + OTLP + Swagger UI:

```toml
propfirm-engine = { version = "0.2", default-features = false, features = ["serialization", "server", "otel", "openapi"] }
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

- **Event store** — `events::store::EventStore` (async trait: `append`, `all`, `recent`, `replay`). Two implementations:
  - `events::store::InMemoryEventStore` — the dev / in-memory default.
  - `persistence::postgres::PostgresEventStore` — durable, append-only, backs the `events` table.
  This is the read-side seam behind breach-report and override replay.
- **Idempotency store** — three implementations behind the `IdempotencyBackend` trait:
  - `api::idempotency::IdempotencyStore` — in-memory (dev).
  - `persistence::postgres::PostgresIdempotencyBackend` — durable, atomic `INSERT ... ON CONFLICT DO NOTHING`.
  - `persistence::redis_store::RedisIdempotencyBackend` — atomic Lua-script check-and-remember.
  Backs the `Idempotency-Key` header on `POST /internal/v1/evaluate`.
- **Audit log** — `api::audit_log` writes to the `audit_log` Postgres table from override / emergency-stop / manual-run / breach-report / evaluate-order / evaluate-internal non-Pass / worker evaluate+error paths.
- **Event bus** — `persistence::redis_store::event_bus::RedisEventBus` for the worker binary's Redis Streams consumer.

The `server` cargo feature pulls `sqlx` (Postgres), `redis`, `bb8`, and `bb8-redis` — the durable backends are compiled in whenever the server feature is on. There is no separate `postgres` cargo feature.

There is no `AccountStore` and no `put_with_version`. For your own optimistic-concurrency layer, carry `Account.version` inside `account_state`: it is hashed into every verdict's `input_hash`, so replaying a stale state is detectable (`Error::StateConflict` is retained as the typed error for that purpose).

## HTTP API

Enable the `server` feature and start the HTTP server:

```rust
use propfirm::api::server::run_server;
use propfirm::settings::Settings;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let settings = Settings::load()?;
    run_server(settings).await?;
    Ok(())
}
```

`run_server(settings: Settings)` takes a fully-built `Settings` struct
(loaded via `Settings::load()` from `config/propfirm.toml` + `PROPFIRM_*`
env vars + `.env`), wires up the idempotency backend, runs Postgres
migrations when `postgres.run_migrations = true`, builds the axum
router, optionally installs TLS / mTLS, and serves with graceful shutdown.

There is **no authentication**. The engine is internal-only; trust is
established at the network boundary (private compose network, k8s
NetworkPolicy, and optionally mTLS — see `docs/tls.md`). The `X-Tenant-Id`
header is **required** on every `/internal/v1/*` and `/v1/*` request —
without it, the engine returns 400 `missing X-Tenant-Id header`
(see `src/api/handlers.rs::extract_tenant_id`). The header is not
cryptographic identity — it's the typed tenant id the engine uses for
audit-log scoping and request validation.

### Endpoints

| Method | Path | Tenant header | Description |
|--------|------|---------------|-------------|
| `GET` | `/health` | none | Liveness probe. |
| `GET` | `/ready` | none | Readiness probe. |
| `GET` | `/metrics` | none | Prometheus metrics scrape. |
| `GET` | `/openapi.json` | none | OpenAPI 3.0 spec (when `openapi` feature enabled). |
| `GET` | `/swagger-ui/` | none | Interactive Swagger UI (when `openapi` feature enabled). |
| `POST` | `/internal/v1/evaluate` | required | Stateless evaluate contract (`account_state` required; a `rule_pack` field is rejected with 400). |
| `POST` | `/internal/v1/override` | required | Clear a false-positive breach. |
| `POST` | `/internal/v1/manual-run` | required | Force re-evaluation of an account. |
| `POST` | `/internal/v1/emergency-stop` | required | Force `DecisionKind::Emergency`, short-circuiting rule evaluation. |
| `POST` | `/internal/v1/breach-report` | required | Trader-facing "why did I fail" view. The JSON body contains `account_id` (not a path parameter). |
| `POST` | `/v1/evaluate-order` | required | Pre-trade order evaluation. |
| `POST` | `/v1/rule-packs/validate` | required | Validate a rule pack (stateless — nothing is persisted). |

All mutating endpoints accept an `Idempotency-Key` header.

The breach-report route is `POST /internal/v1/breach-report` with a
JSON body:

```json
{ "account_id": "uuid" }
```

(The caller's tenant id comes from the `X-Tenant-Id` header. The
engine cross-checks the account's tenant against the header value and
returns 403 if they mismatch.)

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
cargo test --features server --test api_integration
cargo test --features server --test event_bus_integration
```

Run benchmarks (the benchmark harness requires the `server` feature for
the `IdempotencyStore` + `RuleRegistry` types it exercises):

```bash
cargo bench --features server
```

Run chaos tests (require a live Redis — `#[ignore]` by default):

```bash
cargo test --features server --test chaos_redis -- --ignored
```

Run the OTLP end-to-end test (requires a live OTLP collector —
`#[ignore]` by default):

```bash
cargo test --features server,otel --test otlp_e2e -- --ignored
```

Run fuzz targets (nightly only, via `cargo +nightly fuzz`):

```bash
cd fuzz
cargo +nightly fuzz run pure_evaluate_input_hash
cargo +nightly fuzz run rule_registry_panic_safety
```

## Error handling

All fallible operations return `propfirm::Result<T>`. The error type is
`propfirm::Error` (see `src/core/mod.rs`):

- `Error::InvalidConfig(msg)` — configuration validation failure.
- `Error::RuleNotApplicable(rule_id, ctx_kind)` — a rule was attempted on an unsupported context kind.
- `Error::NumericConversion(msg)` — a numeric conversion could not be performed safely (e.g. `Decimal` → `i64` overflow when serializing cents).
- `Error::NotFound(msg)` — entity not found in storage.
- `Error::Persistence(msg)` — storage failure (event store, idempotency backend, audit-log write).
- `Error::Serialization(msg)` — JSON / serde error.
- `Error::InvalidState(msg)` — a logical precondition was violated (e.g. applying `TradeFilled` to a `Pending` account).
- `Error::RuleEval(msg)` — a user-supplied rule produced an error.
- `Error::StateConflict(id, expected, actual)` — optimistic concurrency violation; retained for caller-side use (ADR-11: the stateless contract never produces it).
- `Error::TickRejected(reason)` — stale or out-of-order tick.
- `Error::MissingMetric(name)` — a required metric was unavailable for evaluation (distinct from a clean Pass — the verdict should be "ok-with-data-gap" rather than "ok").
