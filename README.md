# Prop Firm Risk & Rule Evaluation Engine

A **stateless compute service** (D81, docs/64) that evaluates proprietary
trading firm rules, monitors account risk, and produces audit-grade
decisions. Designed as an internal component of the Alpha One PFaaS
Platform — the platform's `workers` consumer owns all state; the engine
is a pure function called via HTTP.

## D81: Stateless — no database, no Redis, no worker

The engine holds **no state** and opens **no database connection**.
Everything it needs arrives in the request; everything it produces leaves
in the response. `workers` owns `evaluation_state`, ordering,
idempotency, retry and DLQ (docs/64 §4.1). The engine's sole job:

```
evaluate(state, rules, tick) → (verdict, new_state, hints)
```

No Postgres · No Redis · No worker binary · No migrations · No OCC.
Scales by `--scale engine=N`. ADR-11 (stateless Rust EVL) re-confirmed.

## Architecture

```
┌─────────────────────────────────────────────────────────────────┐
│                  Platform backend (caller)                      │
│  Either HTTP POST /internal/v1/evaluate                         │
│  Or XADD propfirm:evaluate:requests (Redis Stream)             │
└────────────┬───────────────────────────────────┬───────────────┘
            │                                    │
            ▼                                    ▼
┌───────────────────────┐            ┌────────────────────────┐
│   propfirm-server     │            │   propfirm-worker       │
│   (HTTP, axum)        │            │   (Redis Streams        │
│   - TLS + mTLS        │            │    consumer group)      │
│   - Tower middleware  │            │   - N concurrent tasks  │
│   - /metrics          │            │   - XAUTOCLAIM recovery│
│   - /openapi.json     │            │   - produces responses │
│   - /swagger-ui/      │            │   Subcommands:          │
│   - Graceful shutdown │            │    healthcheck, metrics,│
│   - Panic hook        │            │    status, drain,       │
└───────────┬───────────┘            │    reset-group           │
            │                        └────────────┬───────────┘
            └────────────┬────────────────────────┘
                         ▼
             ┌──────────────────────┐
             │   Engine core (pure) │
             │   25 rule evaluators │
             │   Decimal money      │
             │   input_hash (sha256)│
             └──────────┬───────────┘
                        │
           ┌────────────┴────────────┐
           ▼                         ▼
  ┌───────────────┐         ┌──────────────────┐
  │  PostgreSQL  │         │      Redis       │
  │  (events,    │         │ (idempotency,    │
  │   idempotency,│        │  event bus       │
  │   audit_log,  │        │  streams via     │
  │   rule_packs) │        │  bb8 pool)       │
  └───────────────┘         └──────────────────┘
```

**No authentication** — the engine is reached only from the platform
backend over the private compose network. Trust is established at the
network boundary, not in-process. REST endpoints are for admin
verification only; production traffic flows through Redis Streams for
async throughput.

## Features

### Comprehensive rule library (25 rules out of the box)

| Category | Rules |
|----------|-------|
| **Drawdown** | Daily · Max (static + trailing + EOD trailing) · Per-trade max loss |
| **Targets** | Profit target · Min trading days · Consistency |
| **Trade restrictions** | News · Overnight · Weekend · Hedging · Grid/martingale · Copy trading · HFT/scalping |
| **Position limits** | Max position size · Max open · Max daily · Cooldown · Max total lots · Margin · Trading hours |
| **Time** | Time limit · SL/TP required · Inactivity termination |

### Architectural correctness properties

- **Broker-is-truth equity** — `EquityInput::BrokerReported` vs `Estimated` is type-level; breach rules refuse to terminate on estimates.
- **Stateless pure evaluate** — `pure::evaluate(state, pack, tick) -> PureVerdict` with `input_hash` (sha256) for byte-for-byte reproducibility.
- **Caller-owned concurrency** — ADR-11: no account state in-process; caller owns persistence.
- **Tenant isolation** — `TenantId` threaded through every record; cross-tenant access blocked.
- **Stale & out-of-order tick guard** — ticks older than 10 min or older than last-tick rejected.
- **Decimal precision** — `rust_decimal::Decimal` everywhere; no f64 on money.
- **Panic safety** — `RuleRegistry::evaluate` wraps each rule in `catch_unwind`; a buggy rule degrades to `Warn`.
- **Priority arbitration** — every rule declares numeric `priority()`; `Decision::from_reports` picks the winner deterministically.
- **Pack-driven rules** — rules carry `Option<RuleParams>` populated from versioned `RulePack` JSON. Tenant edits to the pack actually change verdicts.
- **Audit trail** — every sensitive action (override, emergency-stop, manual-run, breach-report, evaluate-order, worker evaluate/error) writes to the `audit_log` table with actor_id, action, tenant, account, resource, request_hash, response_status, latency_ms, metadata JSONB.

### Production-ready subsystems

- **Durable persistence** — PostgreSQL (events, idempotency, audit_log, rule_packs) + Redis (idempotency + event bus via bb8 pool).
- **Redis Streams event bus** — at-least-once delivery via consumer groups; XAUTOCLAIM for PEL recovery.
- **TLS termination** — in-process rustls with optional mTLS (`WebPkiClientVerifier` with configurable client CA).
- **Graceful shutdown** — SIGINT/SIGTERM handled; `CancellationToken` propagates to all worker tasks.
- **Request timeout enforcement** — `tower-http` `TimeoutLayer` + `RequestBodyLimitLayer` + `CompressionLayer`.
- **Idempotency** — `memory` / `postgres` / `redis` backends; atomic Lua script for Redis.
- **Observability** — tracing + tracing-subscriber (JSON/pretty), `/metrics` Prometheus endpoint, panic hook, `#[tracing::instrument]` on all HTTP handlers + worker functions.
- **OpenTelemetry OTLP** — feature-gated (`otel` cargo feature); gRPC/HTTP transport to Tempo/Jaeger/Honeycomb/etc.
- **Flame profiling** — feature-gated (`flame` cargo feature); writes flame-graph trace for performance analysis.
- **OpenAPI + Swagger UI** — feature-gated (`openapi` cargo feature); serves `/openapi.json` + `/swagger-ui/`.
- **No authentication** — intentionally removed; the platform is the sole caller.

## Quick Start

```bash
git clone https://github.com/salmanbao/propfirm-engine.git
cd propfirm-engine
cp .env.example .env
docker compose up -d --build

# Verify
curl http://localhost:8080/health    # → ok
curl http://localhost:8080/ready      # → ready
curl http://localhost:8080/metrics   # → Prometheus format
```

See [`docs/local-dev.md`](docs/local-dev.md) for the full
verification checklist.

## Binaries

| Binary | Purpose |
|---|---|
| `propfirm-server` | HTTP API server (axum + rustls + tower-http). TLS + mTLS optional. |
| `propfirm-worker` | Redis Streams consumer; runs `pure::evaluate` on each request, publishes response. |
| `propfirm-cli` | Local demo + interactive REPL for evaluation. |

### `propfirm-worker` subcommands

```bash
# Run the worker loop (default — no subcommand):
cargo run --release --features server --bin propfirm-worker

# Subcommands:
propfirm-worker healthcheck        # K8s liveness/readiness probe (PING Redis + verify group)
propfirm-worker metrics             # Dump accumulated Prometheus metrics to stdout
propfirm-worker status              # Print PEL stats + per-consumer pending/idle
propfirm-worker drain [idle_secs]   # XACK all PEL entries idle > N secs (destructive)
propfirm-worker reset-group         # Delete + recreate consumer group (destructive)
```

### `propfirm-cli` subcommands

```bash
# Default demo (FTMO Phase 1 lifecycle):
cargo run --release --features tokio-cli --bin propfirm-cli

# Interactive REPL (paste JSON request bodies, get verdicts):
cargo run --release --features server,tokio-cli --bin propfirm-cli repl

# REPL with JSON output (pipe to jq):
cargo run --release --features server,tokio-cli --bin propfirm-cli repl --json

# Batch mode (read JSON lines from file):
cargo run --release --features server,tokio-cli --bin propfirm-cli repl -f requests.jsonl
```

## Cargo features

| Feature | Default | Description |
|---------|---------|-------------|
| `default` | ✅ | `serialization` + `in-memory-store` |
| `serialization` | — | `serde` + `serde_json` + `chrono/serde` |
| `server` | — | HTTP server, worker, durable persistence (Postgres + Redis + bb8), TLS, observability, config |
| `otel` | — | OpenTelemetry OTLP exporter (gRPC/HTTP to Tempo/Jaeger/etc.) |
| `flame` | — | Flame-graph profiling via `tracing-flame` |
| `openapi` | — | OpenAPI 3.0 spec (`/openapi.json`) + Swagger UI (`/swagger-ui/`) via `utoipa` |
| `in-memory-store` | — | Vestigial (gates nothing — ADR-11 removed account store) |
| `tracing` | — | `tracing` + `tracing-subscriber` |
| `tokio-cli` | — | `tokio` (for the CLI binary) |

```bash
# HTTP server (plain HTTP, behind proxy):
cargo run --release --features server --bin propfirm-server

# HTTP server with OTLP + OpenAPI + Swagger UI:
cargo run --release --features server,otel,openapi --bin propfirm-server

# HTTP server with TLS + mTLS + OTLP:
PROPFIRM_SERVER__TLS__ENABLED=true \
PROPFIRM_SERVER__TLS__CERT_PATH=/path/cert.pem \
PROPFIRM_SERVER__TLS__KEY_PATH=/path/key.pem \
PROPFIRM_SERVER__TLS__CLIENT_CA_PATH=/path/ca.pem \
PROPFIRM_OBSERVABILITY__OTLP__ENDPOINT=http://otel-collector:4317 \
cargo run --release --features server,otel --bin propfirm-server

# Event bus worker:
cargo run --release --features server --bin propfirm-worker

# CLI demo:
cargo run --release --features tokio-cli --bin propfirm-cli

# CLI REPL (interactive evaluation):
cargo run --release --features server,tokio-cli --bin propfirm-cli repl
```

## Documentation

- [`docs/configuration.md`](docs/configuration.md) — full settings reference (server, postgres, redis, observability, idempotency, event_bus, TLS, mTLS, OTLP, flame)
- [`docs/local-dev.md`](docs/local-dev.md) — local verification checklist
- [`docs/persistence.md`](docs/persistence.md) — Postgres + Redis backends (event store, idempotency, event bus, audit_log)
- [`docs/observability.md`](docs/observability.md) — tracing, metrics, panic hook, OTLP, flame, audit_log
- [`docs/tls.md`](docs/tls.md) — in-process TLS + mTLS termination
- [`docs/event-bus.md`](docs/event-bus.md) — Redis Streams event bus (consumer groups, XAUTOCLAIM, bb8 pool)
- [`docs/architecture.md`](docs/architecture.md) — internal architecture (layered modules, evaluation flow)
- [`docs/features.md`](docs/features.md) — rule library + correctness properties
- [`docs/integration.md`](docs/integration.md) — embedding the engine as a library
- [`CHANGELOG.md`](CHANGELOG.md) — Keep-a-Changelog format
- [`SECURITY.md`](SECURITY.md) — vulnerability disclosure process
- [`CARGO_LOCK_POLICY.md`](CARGO_LOCK_POLICY.md) — when to run `cargo update`
- [`deploy/helm/README.md`](deploy/helm/README.md) — Helm chart (Deployment, HPA, PDB, NetworkPolicy, Grafana dashboards + alerting)
- [`clients/go/README.md`](clients/go/README.md) — typed Go client library
- [`bench/load/README.md`](bench/load/README.md) — k6 load-test script

## HTTP API

All endpoints are **unauthenticated** (internal-only). All mutating
endpoints accept an `Idempotency-Key` header.

| Method | Path | Purpose |
|---|---|---|
| `GET` | `/health` | Liveness probe |
| `GET` | `/ready` | Readiness probe |
| `GET` | `/metrics` | Prometheus metrics |
| `POST` | `/internal/v1/evaluate` | Stateless evaluate contract (used by platform backend) |
| `POST` | `/internal/v1/override` | Clear a false-positive breach (audited) |
| `POST` | `/internal/v1/manual-run` | Force re-evaluation (audited) |
| `POST` | `/internal/v1/emergency-stop` | Short-circuit evaluation (highest priority, audited) |
| `POST` | `/internal/v1/breach-report` | Trader-facing "why did I fail" view |
| `POST` | `/v1/evaluate-order` | Pre-trade order evaluation (audited) |
| `POST` | `/v1/rule-packs/validate` | Validate a rule pack (stateless) |
| `GET` | `/openapi.json` | OpenAPI 3.0 spec *(requires `openapi` feature)* |
| `GET` | `/swagger-ui/` | Interactive Swagger UI *(requires `openapi` feature)* |

For async high-throughput traffic, use the Redis Streams event bus
instead of HTTP. See [`docs/event-bus.md`](docs/event-bus.md).

## Configuration

Config is loaded from `config/propfirm.toml` → `.env` → `PROPFIRM_*`
env vars (highest precedence). Nested env keys use `__`:

```bash
PROPFIRM_SERVER__BIND_ADDR=0.0.0.0:8080
PROPFIRM_SERVER__TLS__ENABLED=true
PROPFIRM_SERVER__TLS__CLIENT_CA_PATH=/etc/propfirm/tls/ca.pem
PROPFIRM_POSTGRES__DSN=postgresql://propfirm:propfirm@postgres:5432/propfirm
PROPFIRM_REDIS__URL=redis://redis:6379
PROPFIRM_IDEMPOTENCY__BACKEND=redis
PROPFIRM_OBSERVABILITY__OTLP__ENDPOINT=http://otel-collector:4317
PROPFIRM_OBSERVABILITY__FLAME_OUTPUT_PATH=/tmp/propfirm-flame.trace
```

See [`docs/configuration.md`](docs/configuration.md) for the full
reference and [`config/propfirm.toml`](config/propfirm.toml) for the
shipped defaults.

## Production Deployment

### Helm chart

```bash
helm install propfirm ./deploy/helm \
  --namespace propfirm \
  --create-namespace
```

The chart deploys:
- **propfirm-server** (3 replicas, HPA 3-20, PDB min 2, TLS + mTLS optional)
- **propfirm-worker** (3 replicas, HPA 3-30, PDB min 2, liveness+readiness via `healthcheck` subcommand)
- **PostgreSQL** (optional StatefulSet, or point at external managed Postgres)
- **Redis** (optional StatefulSet, or point at external managed Redis)
- **NetworkPolicy** (ingress from `platform-backend` namespace, egress to `infrastructure` namespace)
- **ServiceMonitor** (Prometheus Operator scrape config)
- **Grafana dashboard** (auto-imported via ConfigMap + `grafana_dashboard` label)
- **Grafana alerting rules** (9 rules across 4 groups: API, Decisions, Worker, Health)

See [`deploy/helm/README.md`](deploy/helm/README.md) for the full
production checklist.

### Client libraries

- **Go**: `clients/go/` — typed HTTP client with `Evaluate`, `EvaluateOrder`, `Override`, `EmergencyStop`, `ValidateRulePack`, `Health`, `Ready`, `Metrics` methods. See [`clients/go/README.md`](clients/go/README.md).

## Testing

~200 tests across unit, integration, property, spec-edge, API, chaos,
fuzz, OTLP e2e, and tracing-test suites.

```bash
# Run all tests (skip Redis-dependent event_bus tests):
cargo test --features server,otel -- --skip event_bus

# Run with nextest (faster, parallel):
cargo nextest run --all-features --profile ci

# Run with coverage:
cargo llvm-cov --all-features --workspace --lcov --output-path coverage.info

# Run doc tests:
cargo test --all-features --doc

# Run fuzz targets (nightly):
cd fuzz && cargo +nightly fuzz run rule_registry_panic_safety -- -max_total_time=60

# Run chaos test (needs Docker):
cargo test --features server --test chaos_redis -- --include-ignored

# Run OTLP e2e test (needs OTLP collector):
cargo test --features server,otel --test otlp_e2e -- --include-ignored

# Run k6 load test:
k6 run --env BASE_URL=http://localhost:8080 bench/load/evaluate.js
```

CI runs **13 parallel jobs**: check, nextest, coverage, outdated,
chaos, fuzz, security-audit, cargo-deny, machete, careful, sbom,
otel-e2e, proof-of-build. See [`.github/workflows/ci.yml`](.github/workflows/ci.yml).

## Performance

| Benchmark | Time | Throughput |
|---|---|---|
| `evaluate_tick_single` | ~4.2 µs | — |
| `evaluate_order_single` | ~3.5 µs | — |
| `pure_evaluate` | 19 µs | ~52,600 evals/sec |
| `realistic_load/1000` | 4.87 ms | ~205,000 evals/sec |

Spec requirement: ~17 evals/sec (1000 accounts × 60-sec cadence).
Engine sustains **12,000× the required throughput** on the pure
evaluation path.

## License

MIT OR Apache-2.0
