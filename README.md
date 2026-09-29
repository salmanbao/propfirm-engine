# Prop Firm Risk & Rule Evaluation Engine

An enterprise-grade, fully-typed Rust engine for evaluating proprietary
trading firm rules, monitoring account risk, and producing audit-grade
decisions in real time. Designed as an **internal component** of the
Prop Firm as a Service Platform (PFaaS).

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
│   - TLS termination   │            │    consumer group)      │
│   - Tower middleware  │            │   - N concurrent tasks  │
│   - /metrics          │            │   - XAUTOCLAIM recovery │
│   - graceful shutdown │            │   - produces responses │
└───────────┬───────────┘            └────────────┬───────────┘
            │                                     │
            └──────────────┬──────────────────────┘
                          ▼
              ┌──────────────────────┐
              │   Engine core (pure) │
              │   22 rule evaluators  │
              │   Decimal money       │
              │   input_hash (sha256) │
              └──────────┬───────────┘
                         │
            ┌────────────┴────────────┐
            ▼                         ▼
   ┌───────────────┐         ┌──────────────────┐
   │  PostgreSQL   │         │      Redis       │
   │  (events,     │         │ (idempotency,    │
   │   idempotency,│         │  event bus       │
   │   audit_log,  │         │  streams)        │
   │   rule_packs) │         │                  │
   └───────────────┘         └──────────────────┘
```

**No authentication** — the engine is reached only from the platform
backend over the private compose network. Trust is established at the
network boundary, not in-process. REST endpoints are for manual admin
verification only; production traffic flows through Redis Streams for
async throughput.

## Features

### Comprehensive rule library (22 rules out of the box)

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

### Production-ready subsystems (v0.2.0)

- **Durable persistence** — PostgreSQL (events, idempotency, audit_log, rule_packs) + Redis (idempotency + event bus).
- **Redis Streams event bus** — at-least-once delivery via consumer groups; XAUTOCLAIM for PEL recovery.
- **TLS termination** — in-process rustls; plain-HTTP fallback for behind-proxy deployments.
- **Graceful shutdown** — SIGINT/SIGTERM handled; `CancellationToken` propagates to all worker tasks.
- **Request timeout enforcement** — `tower-http` `TimeoutLayer` (configurable).
- **Idempotency** — `memory` / `postgres` / `redis` backends; atomic Lua script for Redis.
- **Observability** — tracing + tracing-subscriber (JSON/pretty), `/metrics` Prometheus endpoint, panic hook.
- **No authentication** — intentionally removed; the platform is the sole caller.

## Quick Start

```bash
# Bring up the full stack (Postgres + Redis + server + worker)
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
| `propfirm-server` | HTTP API server (axum, internal-only). TLS optional. |
| `propfirm-worker` | Redis Streams consumer; runs `pure::evaluate` on each request, publishes response. |
| `propfirm-cli` | Local demo: builds an FTMO Phase 1 account, runs through the lifecycle. |

```bash
# HTTP server
cargo run --release --features server --bin propfirm-server

# Event bus worker
cargo run --release --features server --bin propfirm-worker

# Local CLI demo
cargo run --release --features tokio-cli --bin propfirm-cli
```

## Documentation

- [`docs/configuration.md`](docs/configuration.md) — full settings reference
- [`docs/local-dev.md`](docs/local-dev.md) — local verification checklist
- [`docs/persistence.md`](docs/persistence.md) — Postgres + Redis backends
- [`docs/observability.md`](docs/observability.md) — tracing, metrics, panic hook
- [`docs/tls.md`](docs/tls.md) — in-process TLS termination
- [`docs/event-bus.md`](docs/event-bus.md) — Redis Streams event bus
- [`docs/architecture.md`](docs/architecture.md) — internal architecture
- [`docs/features.md`](docs/features.md) — rule library + correctness properties
- [`docs/integration.md`](docs/integration.md) — embedding the engine as a library

## HTTP API

All endpoints are **unauthenticated** (internal-only). All mutating
endpoints accept an `Idempotency-Key` header.

| Method | Path | Purpose |
|---|---|---|
| `GET` | `/health` | Liveness probe |
| `GET` | `/ready` | Readiness probe |
| `GET` | `/metrics` | Prometheus metrics |
| `POST` | `/internal/v1/evaluate` | Stateless evaluate contract (used by platform backend) |
| `POST` | `/internal/v1/override` | Clear a false-positive breach |
| `POST` | `/internal/v1/manual-run` | Force re-evaluation |
| `POST` | `/internal/v1/emergency-stop` | Short-circuit evaluation (highest priority) |
| `POST` | `/internal/v1/breach-report` | Trader-facing "why did I fail" view |
| `POST` | `/v1/evaluate-order` | Pre-trade order evaluation |
| `POST` | `/v1/rule-packs/validate` | Validate a rule pack (stateless) |

For async high-throughput traffic, use the Redis Streams event bus
instead of HTTP. See [`docs/event-bus.md`](docs/event-bus.md).

## Configuration

Config is loaded from `config/propfirm.toml` → `.env` → `PROPFIRM_*`
env vars (highest precedence). Nested env keys use `__`:

```bash
PROPFIRM_SERVER__BIND_ADDR=0.0.0.0:8080
PROPFIRM_SERVER__TLS__ENABLED=true
PROPFIRM_POSTGRES__DSN=postgresql://propfirm:propfirm@postgres:5432/propfirm
PROPFIRM_REDIS__URL=redis://redis:6379
PROPFIRM_IDEMPOTENCY__BACKEND=redis
PROPFIRM_OBSERVABILITY__LOG_FORMAT=json
```

See [`docs/configuration.md`](docs/configuration.md) for the full
reference and [`config/propfirm.toml`](config/propfirm.toml) for the
shipped defaults.

## Testing

166 tests + 1 doctest, all passing with `cargo test --all-features`.

```bash
cargo test --all-features
```

Test suites include unit, integration, property tests (proptest),
spec edge cases (pinned-edge regression tests), API integration,
pack-driven, and batch tests (cross-account copy trading, instrument
spec conversions, martingale detection).

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
