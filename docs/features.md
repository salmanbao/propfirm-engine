# Features

This document lists the rules, architectural properties, and operational
controls shipped with the engine (v0.2.0).

## Rule library

The engine includes **25 rule evaluators** organized by category. Each
rule is data-driven: thresholds, units, and priorities come from
`ChallengePlan` or `RulePack`, not hard-coded constants.

### Drawdown

| Rule | Description |
|------|-------------|
| Daily drawdown | Enforces the maximum daily loss threshold. Resets at the start of each trading day. |
| Max drawdown (static) | Enforces a fixed floor based on the account's initial balance. Never moves. |
| Max drawdown (trailing) | Enforces a floating floor based on the account's peak balance. Moves up as equity grows. |
| Max drawdown (EOD trailing) | Like trailing, but resets to the day-start balance once per day. |
| Per-trade max loss | Enforces a maximum loss on any single trade. Only active when the plan configures a per-trade threshold. |

### Targets

| Rule | Description |
|------|-------------|
| Profit target | Enforces the profit target. Emits `TargetHit` when the target is reached, allowing the account to proceed. |
| Minimum trading days | Requires a minimum number of distinct active trading days before a phase can be passed. |
| Consistency | Enforces the consistency ratio (largest single-day profit vs total profit). |

### Trade restrictions

| Rule | Description |
|------|-------------|
| News trading | Blocks trading during configured news events. |
| Overnight holding | Blocks holding positions overnight. |
| Weekend holding | Blocks holding positions over the weekend. |
| Hedging | Blocks hedging (opposite-direction positions on the same symbol). |
| Grid/martingale | Detects lot-escalation patterns characteristic of grid or martingale strategies. |
| Copy trading | Detects copying fills from other accounts (cross-account reference trades). |
| HFT/scalping | Detects high-frequency trading patterns when the plan bans them. |

### Position limits

| Rule | Description |
|------|-------------|
| Max position size | Enforces a maximum notional position size per trade. |
| Max open positions | Enforces a maximum number of simultaneously open positions. |
| Max daily trades | Enforces a maximum number of trades per trading day. |
| Cooldown | Enforces a minimum time between trades. |
| Max total lots | Caps the account's aggregate open exposure (positions + pending order) at `plan.max_total_lots`, converted through each symbol's instrument spec. |
| Margin | Rejects orders whose prospective margin (units × price ÷ `plan.leverage`) exceeds free margin (equity − margin already committed by open positions). Always on — leverage defaults to 1:100; margin math is meaningful for any account. |
| Trading hours | Rejects orders submitted outside `plan.trading_hours` (a `(start_hour, end_hour)` window evaluated in the plan's timezone). Supports wrap-around midnight windows (e.g. 22:00–06:00). |

### Time

| Rule | Description |
|------|-------------|
| Time limit | Enforces the overall time limit for a challenge phase. |
| Stop-loss required | Requires every trade to have a stop-loss attached. |
| Take-profit required | Requires every trade to have a take-profit attached. |
| Inactivity termination | Terminates an account if it is inactive for too many days. |

The three new rules (`max_total_lots`, `trading_hours`, `margin`) live
in `src/rules/evaluators/plan_caps.rs` (§C.2 rules for plan fields that
existed but were never enforced in v0.1.0).

## Rule pack as data

Rules are configured through `RulePack` JSON, not compiled Rust. This means:

- Tenant admins can edit thresholds through a form.
- A pack can be bound to an account at purchase.
- Re-binding requires an explicit, audited action.
- No redeployment is needed to change rule parameters.

Pack lifecycle: `draft` → `active` → `superseded`, with `effective_from` timestamps.

Each `RuleEntry` in a pack can override:

- `value` — the threshold value.
- `basis` — static, trailing, or EOD trailing for drawdown rules.
- `unit` — percent or money.
- `tolerance_cents` — broker-rounding tolerance.
- `priority` — arbitration priority.
- `enabled` — whether the rule is active in this pack.

## Architectural correctness properties

- **Broker-is-truth equity** — the engine never recomputes equity from positions + quote. `EquityInput::BrokerReported` vs `EquityInput::Estimated` is a type-level distinction; breach-capable rules refuse to terminate on an estimate.
- **Stateless pure evaluate** — `pure::evaluate(state, pack, tick) -> PureVerdict` produces an `input_hash` (sha256) so any past verdict can be recomputed byte-for-byte from its recorded inputs.
- **Caller-owned concurrency** — the server keeps no account state (ADR-11). Callers carry `Account.version` in the request's `account_state`; it is hashed into `input_hash` so a stale replay is detectable. `Error::StateConflict` remains the typed error for caller-side optimistic concurrency.
- **Tenant isolation** — `TenantId` threaded through `Account`, `ChallengePlan`, `Violation`, `RulePack`. The `X-Tenant-Id` header is required on every request (`/internal/v1/*` and `/v1/*`); a mismatch between the header and the `account_state.tenant_id` returns 403. Cross-tenant reads are never a storage concern.
- **Stale & out-of-order tick guard** — ticks older than 10 minutes (configurable) or older than the last-evaluated tick are rejected with `Error::TickRejected` before evaluation runs.
- **Decimal precision** — all monetary values use `rust_decimal::Decimal`; no floating-point drift on money.
- **Panic safety** — the registry wraps each rule evaluation in `catch_unwind`. A panicking rule degrades to a `Warn` verdict; the process stays alive. The panic hook routes the message to `tracing::error` (and an `audit_log` write on the worker).

## Operational controls

- **Manual override** — `Override` record (`clears_violation_id`, `reason`, `actor_id`, `at`) clears a false-positive breach without deleting the original verdict. State machine: `Failed` / `EmergencyStopped` → `Active`. Writes to `audit_log` with `action='override_breach'`.
- **Emergency stop** — `PipelineEvent::EmergencyStop { reason, actor_id, at }` short-circuits normal rule evaluation and forces `DecisionKind::Emergency`. Beats every other verdict, including `Liquidate`. Writes to `audit_log` with `action='emergency_stop'`.
- **Early-warning threshold** — first-class `RuleVerdict::EarlyWarning` (ops-paged) distinct from trader-facing `Warn`. Emitted at 80% of breach threshold on every breach-capable rule.
- **Override + emergency audit trail** — both record full metadata (`actor_id`, `reason`, `at`) and are emitted as domain events alongside the original verdict, returned to the caller on `PipelineResult.events` for persistence.
- **Idempotency** — all mutating HTTP endpoints accept an `Idempotency-Key` header. The server tracks the last N keys per endpoint to deduplicate retries. Outcomes (`fresh` / `replay` / `conflict` / `error`) are emitted as the `propfirm_idempotency_outcomes_total` Prometheus counter.

## Risk analytics

The engine includes a risk module that computes quantitative metrics from account history:

- Sharpe ratio
- Sortino ratio
- Calmar ratio
- Max drawdown
- Profit factor
- Expectancy
- Win rate
- Z-score
- Recovery factor
- Parametric Value-at-Risk
- Expected Shortfall
- Exposure analytics (gross/net/long/short, per-symbol concentration)

## Persistence

Account persistence was removed by ADR-11: `/internal/v1/evaluate` receives `account_state` in the request and returns the updated state in the response; persisting it is the caller's responsibility. What remains is a layered persistence stack:

| Layer | Implementations | Use |
|---|---|---|
| **Event store** | `events::store::InMemoryEventStore` (dev), `persistence::postgres::PostgresEventStore` (prod) | Append-only domain-event log; `replay(account_id)` rebuilds account state. |
| **Idempotency backend** | `api::idempotency::IdempotencyStore` (memory), `persistence::postgres::PostgresIdempotencyBackend`, `persistence::redis_store::RedisIdempotencyBackend` | Dedupes mutating HTTP retries. |
| **Audit log** | `api::audit_log` → Postgres `audit_log` table | Who-did-what-when for sensitive operations. |
| **Event bus** | `persistence::redis_store::event_bus::RedisEventBus` | Redis Streams at-least-once delivery for the worker. |
| **Rule pack store** | Postgres `rule_packs` table | Versioned pack lifecycle (Draft → Active → Superseded). |

The `server` cargo feature pulls `sqlx` (Postgres), `redis`, `bb8`,
`bb8-redis`, `rustls`, `metrics-exporter-prometheus`, `figment`, and
`dotenvy` — the full production stack is compiled in whenever the
server feature is on.

The `in-memory-store` cargo feature is vestigial (kept so existing
build commands keep working); it gates nothing in v0.2.0.

See `docs/persistence.md` for the schema and per-backend semantics.

## HTTP API

Optional `axum`-based server exposing REST endpoints for account
evaluation, rule pack validation, breach reporting, override /
emergency-stop / manual-run operations, Prometheus metrics, and
Swagger UI. See the [integration guide](integration.md) for endpoint
details.

## Cargo features at a glance

| Feature | Pulls in | Use |
|---|---|---|
| `default` | `serialization` + `in-memory-store` | Library-only embedding (no server). |
| `serialization` | `serde`, `serde_json`, `chrono/serde` | JSON config + request/response payloads. |
| `server` | `axum`, `axum-server`, `tower`, `tower-http`, `tokio`, `tracing`, `tracing-subscriber`, `sqlx`, `redis`, `bb8`, `bb8-redis`, `rustls`, `rustls-pemfile`, `metrics`, `metrics-exporter-prometheus`, `prometheus`, `figment`, `dotenvy`, `tokio-util` | HTTP server + worker + durable persistence + TLS + metrics. |
| `otel` | `tracing-opentelemetry`, `opentelemetry`, `opentelemetry-otlp`, `opentelemetry_sdk`, `opentelemetry-stdout` | OpenTelemetry OTLP exporter (gRPC/HTTP). Enable in production builds. |
| `flame` | `tracing-flame` | Flame-graph profiling output to `observability.flame_output_path`. Enable for one-off perf investigations. |
| `openapi` | `utoipa`, `utoipa-swagger-ui` | Serves `GET /openapi.json` + `GET /swagger-ui/`. |
| `tracing` | `tracing`, `tracing-subscriber` | Structured logging (also pulled by `server`). |
| `tokio-cli` | `tokio` | Enables the `propfirm-cli` binary. |
| `in-memory-store` | (nothing — vestigial) | Kept for backwards compatibility with v0.1 build commands. |

### Production feature set

```toml
[dependencies]
propfirm-engine = { features = ["server", "otel", "openapi"] }
```

This pulls in: HTTP server + worker + durable persistence + TLS + mTLS
+ Prometheus metrics + OpenTelemetry OTLP + Swagger UI. Skip `otel`
when you don't need distributed tracing; skip `openapi` when you
don't need the spec served at runtime.

### Profiling feature set

```toml
[dependencies]
propfirm-engine = { features = ["server", "flame"] }
```

Enables the `tracing-flame` layer; set
`observability.flame_output_path = "/tmp/propfirm-flame.trace"` in
`config/propfirm.toml` and convert with
`flamegraph /tmp/propfirm-flame.trace > flamegraph.svg`.

## Observability

All four observability pillars are wired and production-ready (see
`docs/observability.md` for full details):

1. **Structured logging** — `tracing` + `tracing-subscriber` (JSON or pretty).
2. **Prometheus metrics** — every handler emits
   `propfirm_http_requests_total`, `propfirm_evaluate_decisions_total`,
   `propfirm_idempotency_outcomes_total`, `propfirm_errors_total`, and
   `propfirm_request_duration_seconds` (via `LatencyScope`); the worker
   emits `propfirm_event_bus_messages_consumed_total`, `_produced_total`,
   `_acked_total`, `_claimed_total`, and `_errors_total`.
3. **Panic hook** — routes panics through `tracing::error`.
4. **OpenTelemetry OTLP exporter** (when `otel` feature is enabled) —
   gRPC/HTTP transport to Tempo / Jaeger / Honeycomb / etc.

A prebuilt Grafana dashboard ships at
`deploy/helm/dashboards/propfirm-overview.json` and 9 alerting rules
across 4 groups ship at `deploy/helm/alertrules/propfirm-engine.yaml`.

## Worker subcommands

The `propfirm-worker` binary supports five subcommands in addition to
the default consumer loop:

| Subcommand | Description |
|---|---|
| `healthcheck` | Connect to Redis + Postgres, verify stream group exists, exit 0/1. |
| `metrics` | Dump accumulated worker metrics + exit. |
| `status` | Print PEL stats + consumer list for the request stream. |
| `drain [idle_secs]` | XACK all PEL entries idle > `idle_secs` (default 60s). |
| `reset-group` | Delete + recreate the consumer group. |

```bash
# Default: run the consumer loop
propfirm-worker

# Healthcheck (useful for k8s liveness probes)
propfirm-worker healthcheck

# Drain a stuck PEL
propfirm-worker drain 120

# Recreate the consumer group (after a Redis flush)
propfirm-worker reset-group
```

## CLI subcommands

The `propfirm-cli` binary supports:

- Default (no args): built-in end-to-end demo (start → order → tick → risk metrics).
- `repl`: interactive REPL — paste JSON request bodies, get verdicts.
  - `-j` / `--json`: machine-readable JSON output (for `jq` piping).
  - `-f FILE`: read JSON requests from FILE (one per line) in batch mode.

```bash
propfirm-cli                              # default demo
propfirm-cli repl                         # interactive REPL, human output
propfirm-cli repl -j                      # interactive REPL, JSON output
propfirm-cli repl -f /tmp/requests.jsonl  # batch mode
```

## Performance

Benchmarks are in `benches/engine.rs` and run via
`cargo bench --features server`.

### Headline numbers

| Benchmark | Time | Throughput | Notes |
|-----------|------|------------|-------|
| `evaluate_tick_single` | ~4.2 µs | — | One tick evaluation against an account with all 25 rules registered. |
| `evaluate_order_single` | ~3.5 µs | — | Pre-trade order evaluation against an account with all 25 rules registered. |
| `pure_evaluate` | **19 µs** | ~52,600 evals/sec | The stateless pure-evaluate function. |
| `realistic_load/10` | 48.8 µs | **~205,000 evals/sec** | 10 accounts × 1 tick. |
| `realistic_load/100` | 469.6 µs | **~213,000 evals/sec** | 100 accounts × 1 tick. |
| `realistic_load/1000` | **4.87 ms** | **~205,000 evals/sec** | 1000 accounts × 1 tick. |

For load testing the HTTP server end-to-end, a k6 script ships at
`bench/load/evaluate.js` (see the README in the same folder for the
run command and thresholds).

## Testing

~200 tests across unit, integration, property, spec edge case, API,
batch, chaos, fuzz, and OTLP end-to-end suites. All passing with
`cargo test --all-features`.

| Suite | Purpose |
|-------|---------|
| Unit tests (`src/`) | Core logic: instrument registry, payout engine, settings, metrics helpers, audit-log builders, OTLP init. |
| Integration tests (`tests/integration.rs`) | End-to-end behavior: presets validate, account lifecycle, drawdown breaches, profit target, hedging, position limits, pipeline, override, emergency stop, pure-evaluate determinism. |
| P0 default rules & units (`tests/p0_default_rules_and_units.rs`) | Rule registry: all 25 rule kinds produce correct verdicts from plan defaults; unit/basis parsing; pack-driven parameterization. |
| P0 pack-driven (`tests/p0_pack_driven.rs`) | Pack overrides: pack basis overrides plan basis, tolerance override, tenant isolation, pack priority override. |
| Property tests (`tests/property_tests.rs`) | `proptest`-driven invariants: drawdown non-negativity, static-floor immutability, trailing-floor monotonicity, stateless determinism, decision invariance under rule reordering, breach severity ordering, replay determinism, disabled rules contribute nothing, money fields non-negative, lots/units no cross-type comparison, rule-level verdict variants have a producer, gap flagged on unstarted account. |
| Spec edge cases (`tests/spec_edge_cases.rs`) | Named, permanent regression tests pinning edge semantics (spec §3.4, P1.1 rollover/DST, D.3 phase progression, D.4 liquidation, gap-flag surfacing). |
| Doctests | `src/lib.rs` example compiles; `src/equity_input.rs` example. README example is not doctested. |
| API integration tests (`tests/api_integration.rs`) | Binding spec HTTP endpoints: health, evaluate-order, manual-run, breach-report, override 404, input_hash sha256, ServerState clone. |
| API P0 fixes (`tests/api_p0_fixes.rs`) | Contract enforcement: equity source defaults, overnight position, idempotency replay/conflict/no-double-apply, bridge tick v1, concurrency no-deadlock, two-call trailing chain, gap-flagged distinct. |
| Batch tests (`tests/ab_batch.rs`, `tests/c_batch.rs`, `tests/d1_martingale.rs`) | Cross-account copy-trading detection, pack-driven thresholds, instrument spec units→lots conversion, margin/max_total_lots/trading_hours rules, martingale/grid detection. |
| Event-bus integration (`tests/event_bus_integration.rs`) | Redis Streams end-to-end: ensure_group, XREADGROUP round-trip, XACK, XAUTOCLAIM recovery. |
| Chaos tests (`tests/chaos_redis.rs`, `#[ignore]`) | Redis-node-restart, network-partition, and PEL-recovery stress. Run with `--ignored`. |
| OTLP e2e (`tests/otlp_e2e.rs`, `#[ignore]`) | Real OTLP collector round-trip; asserts spans with expected service name + per-handler span names arrive. |
| OTLP integration (`tests/otlp_integration.rs`) | OTLP layer construction without a live collector (uses `opentelemetry_stdout`). |
| Fuzz (`fuzz/fuzz_targets/`) | `pure_evaluate_input_hash` (input-hash stability across random inputs) + `rule_registry_panic_safety` (registry survives arbitrary rule outputs). Nightly only via `cargo +nightly fuzz`. |
| `tracing-test`-based unit tests | Span assertions on the metrics + audit-log helper functions (see `src/api/metrics.rs::tests`). |

CI runs 13 parallel jobs: `check`, `nextest`, `coverage`, `outdated`,
`chaos`, `fuzz`, `security-audit`, `cargo-deny`, `machete`, `careful`,
`sbom`, `otel-e2e`, and `proof-of-build`. See `.github/workflows/ci.yml`.

## Supply-chain & policy documents

The repo root ships with:

- **`CHANGELOG.md`** — Keep-a-Changelog-format release notes.
- **`SECURITY.md`** — vulnerability disclosure process.
- **`CARGO_LOCK_POLICY.md`** — when to run `cargo update` + how to review Dependabot PRs.
- **`deny.toml`** — cargo-deny config (advisories, licenses, bans, sources).
- **`.github/dependabot.yml`** — automated dep upgrade PRs for `cargo`, `github-actions`, and `docker` ecosystems (weekly cadence).

A Go client library ships at **`clients/go/`** (one file each for
client + models + go.mod). A k6 load-test script ships at
**`bench/load/evaluate.js`**.
