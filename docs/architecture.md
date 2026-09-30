# Architecture

This document describes the internal architecture of the prop firm
engine, how the major modules connect, and the design decisions behind
the evaluation flow.

## Layered structure

The crate is organized as a layered library. Each layer has a single
responsibility and depends only on layers beneath it.

```
core
config
rules
engine
persistence (server feature)
events
notifications
reporting
pure
api (server feature)
```

### Core

`core` defines immutable value types and domain aggregates:

- `Account`, `Position`, `Order`, `Trade`, `Tick`
- `Money`, `Price`, `Quantity`, `Lots`
- `Violation`, `ViolationKind`, `ViolationSeverity`
- `AccountStatus`, `ChallengePlan`, `PackLifecycle`

All monetary values use `rust_decimal::Decimal`. There is no
floating-point arithmetic on money anywhere in the crate.

`core::mod::Error` is the crate-level error type with variants:
`InvalidConfig`, `RuleNotApplicable`, `NumericConversion`, `NotFound`,
`Persistence`, `Serialization`, `InvalidState`, `RuleEval`,
`StateConflict(id, expected, actual)`, `TickRejected`, `MissingMetric`.

### Config

`config` contains `ChallengePlan` definitions and preset factories. A
plan declares thresholds, enabled rules, loss-reference mode, time
limits, optional `max_total_lots`, optional `trading_hours`, optional
`timezone`, and leverage. Presets cover FTMO, MyForexFunds, The Funded
Trader, SurgeTrader, and custom configurations.

### Rules

`rules` contains the rule trait, context, registry, and all concrete evaluators.

- `Rule` — the trait every rule implements. It exposes `id()`, `name()`, `kind()`, `scope()`, `severity()`, `priority()`, `tolerance_cents()`, and `evaluate()`.
- `RuleContext` — the read-only context passed into every rule evaluation. It carries the account, open positions, today's trades, pending order, latest tick, recent events, cross-reference trades, instruments, and server time.
- `RuleRegistry` — owns the ordered list of rules and evaluates them all against a context. It catches panics from buggy rules and degrades them to `Warn` so one bad rule cannot take down the process.
- Evaluators — 25 concrete rule implementations under `rules/evaluators/`. Each evaluator reads from `RuleContext` and returns a `RuleVerdict`. The three plan-cap rules (`max_total_lots`, `trading_hours`, `margin`) live in `rules/evaluators/plan_caps.rs`.

Rule verdicts:

- `Pass` — rule passed.
- `Warn` — soft warning; account continues.
- `Fail` — hard violation; account should be terminated.
- `Liquidate` — force-close all open positions.
- `TargetHit` — positive outcome distinct from "nothing happened".
- `Emergency` — ops/compliance short-circuit; highest priority.
- `EarlyWarning` — ops-paged signal at ~80% of breach threshold.
- `GapFlagged` — required inputs were absent; rule did not silently evaluate against defaults.
- `Skip` — rule not applicable to this context kind.

### Engine

`engine` is the orchestration layer. It turns domain events into account state transitions and decisions.

- `Evaluator` — wraps a `RuleRegistry` and provides convenience methods for each context kind: `evaluate()`, `evaluate_order()`, `evaluate_trade()`, `evaluate_tick()`, `evaluate_tick_estimated()`, `evaluate_day_rollover()`.
- `Pipeline` — processes `PipelineEvent` values against the caller-supplied account: applies the event to build a `RuleContext`, runs the evaluator, emits domain events, and notifies listeners. No state is persisted (ADR-11) — the updated account comes back on `PipelineResult`.
- `PipelineEvent` — the input enum. Variants: `AccountStarted`, `OrderSubmitted`, `TradeFilled`, `Tick`, `TickEstimated`, `DayRollover`, `EndOfDay`, `OnDemand`, `EmergencyStop`, `OverrideBreach`, `PayoutRequest`, `PayoutApprove`.
- `Decision` — aggregates rule reports into a single outcome using declared rule priorities. `DecisionKind` orders outcomes by severity: `Pass < EarlyWarning < Warn < GapFlagged < TargetHit < Fail < Liquidate < Emergency`.
- `Snapshot` — point-in-time account state plus the current decision.

#### Stateless pure evaluate

`pure::evaluate()` is a stateless function that takes all inputs explicitly and returns a `PureVerdict` with an `input_hash`. The hash is a SHA-256 of every input field, so any past verdict can be recomputed byte-for-byte from its recorded inputs. This is the contract the platform's `/internal/v1/evaluate` endpoint calls.

### Persistence (server feature)

**ADR-11**: account persistence was removed from the engine. What remains is a layered persistence stack — all wired and production-ready in v0.2.0:

- **Event store** — async trait `events::store::EventStore` (`append` / `all` / `recent` / `replay`) with two implementations:
  - `events::store::InMemoryEventStore` (in `src/events/store.rs`) — the dev / in-memory default.
  - `persistence::postgres::PostgresEventStore` — durable, append-only, backs the `events` table. Used as the read-side seam behind breach-report and override replay.
- **Idempotency backends** — `api::idempotency::IdempotencyBackend` trait with three implementations:
  - `api::idempotency::IdempotencyStore` (memory) — in-process `HashMap` for dev.
  - `persistence::postgres::PostgresIdempotencyBackend` — durable, atomic `INSERT ... ON CONFLICT DO NOTHING`.
  - `persistence::redis_store::RedisIdempotencyBackend` — atomic Lua-script check-and-remember.
- **Audit log** — `api::audit_log` writes to the Postgres `audit_log` table from every sensitive handler (override, emergency-stop, manual-run, breach-report, evaluate-order, evaluate-internal non-Pass, worker evaluate/error). Writes are best-effort — if `pg_pool` is `None` (memory-only dev mode), the write is skipped silently.
- **Rule pack store** — Postgres `rule_packs` table (versioned Draft → Active → Superseded lifecycle).
- **`persistence/traits.rs`** — sync no-op placeholder documenting why the engine no longer defines account CRUD.

The `server` cargo feature pulls `sqlx` (Postgres), `redis`, `bb8`,
`bb8-redis` — durable backends are compiled in whenever the server
feature is on. There is no separate `postgres` cargo feature.

### Events

`events` defines the async `EventStore` trait (`append`, `all`, `recent`, `replay`) plus the `InMemoryEventStore` implementation in `events::store` (in `src/events/store.rs`). Domain events carry causation IDs linking them to their triggering inputs; the pipeline emits them on `PipelineResult.events` and persisting them is the caller's decision (ADR-11). `replay` is the dispute-resolution seam for rebuilding account state from stored events.

### Notifications

`notifications` defines the `Notifier` trait. Implementations can deliver rule violations via webhook, email, push, or any other channel. `LogNotifier` is the default for development.

### Reporting

`reporting` builds structured `PerformanceReport` summaries combining rule status and risk metrics.

### API (server feature)

`api` is the optional `axum`-based HTTP server. It exposes:

- **Internal endpoints** (called by the platform backend over the private compose network; **no authentication** — trust is established at the network boundary, optionally strengthened with mTLS): `POST /internal/v1/evaluate`, `POST /internal/v1/override`, `POST /internal/v1/manual-run`, `POST /internal/v1/emergency-stop`, `POST /internal/v1/breach-report` (JSON body `{account_id}`, not a path parameter).
- **Public endpoints** (called by tenant admin tooling): `POST /v1/evaluate-order`, `POST /v1/rule-packs/validate`.
- **Probes** (no `X-Tenant-Id` required): `GET /health`, `GET /ready`.
- **Observability**: `GET /metrics` (Prometheus), `GET /openapi.json` + `GET /swagger-ui/` (when `openapi` cargo feature enabled).

Every mutating endpoint accepts an `Idempotency-Key` header. Every
`/internal/v1/*` and `/v1/*` request must carry an `X-Tenant-Id`
header — without it, the engine returns 400
`missing X-Tenant-Id header`. The header is not cryptographic
identity (the engine has no auth layer); it's the typed tenant id
used for audit-log scoping and request validation.

## Evaluation flow

```
PipelineEvent + caller-supplied account
    │
    ▼
apply event → RuleContext
    │
    ▼
Evaluator::evaluate(ctx)
    │
    ▼
RuleRegistry::evaluate(ctx)
    │  for each enabled rule:
    │    rule.is_enabled(ctx)?
    │    rule.scope matches ctx.kind?
    │    rule.evaluate(ctx) → RuleVerdict
    │    catch panics → Warn
    │
    ▼
Vec<RuleReport> (each with priority + metadata)
    │
    ▼
Decision::from_reports(&reports)
    │  pick highest-priority verdict
    │
    ▼
PipelineResult { snapshot, events, result, account }
    │
    ├── return updated account to caller (caller persists)
    ├── hand domain events to caller (EventStore audit seam)
    └── notify listeners
```

On the HTTP path, `evaluate_internal_impl` runs this pipeline, then:
- Records `propfirm_evaluate_decisions_total{kind=...}` via `record_decision`.
- Records `propfirm_idempotency_outcomes_total{outcome=...}` via `record_idempotency_outcome`.
- Records `propfirm_request_duration_seconds` via `LatencyScope` on drop.
- If the decision is non-Pass, writes an `audit_log` entry via `audit_log::evaluate(...).finish(pg_pool, ...)`.

On the worker path, the consumer task does the same plus:
- Records `propfirm_event_bus_messages_consumed_total{consumer=...}`.
- Records `propfirm_event_bus_messages_produced_total` after the XADD response.
- Records `propfirm_event_bus_messages_acked_total` after the XACK.
- Writes `audit_log::worker_evaluate(...)` (or `worker_error` on a decode/panic failure).

## Key design decisions

- **Broker-is-truth equity**: The engine never recomputes equity from positions + quote. `EquityInput::BrokerReported` vs `EquityInput::Estimated` is a type-level distinction. Breach-capable rules refuse to terminate on an estimate.
- **Stateless pure evaluate**: `pure::evaluate()` produces an `input_hash` so any past verdict can be recomputed byte-for-byte from its recorded inputs.
- **Caller-owned concurrency**: The server keeps no account state (ADR-11). `Account.version` travels in the request's `account_state` and is hashed into `input_hash`; `Error::StateConflict` remains the typed error for caller-side optimistic concurrency.
- **Tenant isolation**: `TenantId` is threaded through `Account`, `ChallengePlan`, `Violation`, `RulePack`. The `X-Tenant-Id` header is required on every request; mismatches between the header and `account_state.tenant_id` return 403.
- **Stale tick guard**: Ticks older than 10 minutes (configurable) or older than the last-evaluated tick are rejected before evaluation runs.
- **Panic safety**: The registry wraps each rule evaluation in `catch_unwind`. A panicking rule degrades to a `Warn` verdict; the process stays alive. The panic hook routes the message to `tracing::error`; the worker writes an `audit_log` entry on panic.
- **Rule priority over registration order**: Each rule declares a numeric `priority()`. `Decision::from_reports` picks the highest-priority verdict. Reordering rules in a pack does not change outcomes.
- **No authentication**: Removed in v0.2.0. The engine is internal-only; trust is established at the network boundary (private compose network, k8s NetworkPolicy, optional mTLS via `rustls::server::WebPkiClientVerifier`).

## Cargo features

The architecture is shaped by the cargo feature set (see `Cargo.toml`):

| Feature | Pulls in |
|---|---|
| `default` | `serialization` + `in-memory-store` |
| `serialization` | `serde`, `serde_json`, `chrono/serde` |
| `server` | `axum`, `axum-server`, `tower`, `tower-http`, `tokio`, `tracing`, `tracing-subscriber`, `sqlx`, `redis`, `bb8`, `bb8-redis`, `rustls`, `rustls-pemfile`, `metrics`, `metrics-exporter-prometheus`, `prometheus`, `figment`, `dotenvy`, `tokio-util` |
| `otel` | `tracing-opentelemetry`, `opentelemetry`, `opentelemetry-otlp`, `opentelemetry_sdk`, `opentelemetry-stdout` |
| `flame` | `tracing-flame` |
| `openapi` | `utoipa`, `utoipa-swagger-ui` |
| `tracing` | `tracing`, `tracing-subscriber` |
| `tokio-cli` | `tokio` (enables the `propfirm-cli` binary) |
| `in-memory-store` | (vestigial — gates nothing in v0.2.0) |

`otel` enables the OTLP exporter layer (gRPC/HTTP transport to a
collector). `flame` enables a `tracing-flame::FlameLayer` for
flame-graph profiling output (set
`observability.flame_output_path` to a file path). `openapi` enables
the `utoipa` OpenAPI spec + `utoipa-swagger-ui` routes
(`/openapi.json` + `/swagger-ui/`).
