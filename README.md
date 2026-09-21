# Prop Firm Risk & Rule Evaluation Engine

An enterprise-grade, fully-typed Rust library for evaluating proprietary trading firm rules, monitoring account risk, and producing audit-grade decisions in real time.

Built to satisfy the binding spec for a proprietary-trading-firm-as-a-service (PFaaS) platform — every architectural decision is traceable to a specific spec requirement (ADR-11 statelessness, broker-is-truth equity, tenant isolation, optimistic concurrency, etc.).

---

## Table of Contents

- [Features](#features)
- [Performance & Benchmarks](#performance--benchmarks)
- [Quick Start](#quick-start)
- [Example](#example)
- [Architecture](#architecture)
- [HTTP API Surface](#http-api-surface)
- [Configuration](#configuration)
- [Rule Pack as Data](#rule-pack-as-data)
- [Testing](#testing)
- [Compliance & Audit](#compliance--audit)
- [License](#license)

---

## Features

### Comprehensive rule library (22 rules out of the box)

| Category | Rules |
|----------|-------|
| **Drawdown** | Daily drawdown · Max drawdown (static + trailing) · Trailing drawdown · Per-trade max loss |
| **Targets** | Profit target · Minimum trading days · Consistency |
| **Trade restrictions** | News trading · Overnight holding · Weekend holding · Hedging · Grid/martingale · Copy trading · HFT/scalping |
| **Position limits** | Max position size · Max open positions · Max daily trades · Cooldown |
| **Time** | Time limit · Stop-loss required · Take-profit required · Inactivity termination |

Each rule is implemented as a `Rule` trait object with explicit `priority()` (for breach arbitration) and `tolerance_cents()` (to absorb broker rounding noise at the boundary).

### Preset challenge plans

FTMO (phase 1 / phase 2 / funded) · MyForexFunds · The Funded Trader · SurgeTrader · custom.

Each preset declares `max_loss_reference: LossReference::{Static, Trailing, EodTrailing}` explicitly so the max-loss rule measures against the correct baseline. FTMO phase-1 uses a static floor (`$90k` on a `$100k` account, never moves); FTMO funded switches to trailing; FTMO 1-Step and FundedNext 1-Step use EOD-reset trailing.

### Risk analytics

Sharpe · Sortino · Calmar · Max drawdown · Profit factor · Expectancy · Win rate · Z-score · Recovery factor · Parametric VaR · Expected Shortfall · Exposure analytics (gross/net/long/short, per-symbol concentration).

### Architectural correctness properties

- **Broker-is-truth equity** — the engine never recomputes equity from positions + quote. `EquityInput::BrokerReported` vs `EquityInput::Estimated` is a type-level distinction; breach-capable rules refuse to terminate on an estimate (P1-5 fix).
- **Stateless pure evaluate** — `pure::evaluate(state, pack, tick) -> PureVerdict` produces an `input_hash` (sha256) so any past verdict can be recomputed byte-for-byte from its recorded inputs (P1-7 fix; ADR-11).
- **Optimistic concurrency control** — `AccountStore::put_with_version(expected)` returns `Error::StateConflict` on mismatch. No silent last-write-wins clobbering (P1-8 fix).
- **Tenant isolation** — `TenantId` threaded through `Account`, `ChallengePlan`, `Violation`, `RulePack`. `AccountStore::get_for_tenant(tenant_id, account_id)` filters at the storage layer (P1-9 fix).
- **Stale & out-of-order tick guard** — ticks older than 10 minutes (configurable) or older than the last-evaluated tick are rejected with `Error::TickRejected` before evaluation runs (P1-14 fix).
- **Decimal precision** — all monetary values use `rust_decimal::Decimal`; no floating-point drift on money.

### Operational controls

- **Manual override** — `Override` record (`clears_violation_id`, `reason`, `actor_id`, `at`) clears a false-positive breach without deleting the original verdict. State machine: `Failed` / `EmergencyStopped` → `Active` (P1-11 fix).
- **Emergency stop** — `PipelineEvent::EmergencyStop { reason, actor_id, at }` short-circuits normal rule evaluation and forces `DecisionKind::Emergency`. Beats every other verdict, including `Liquidate` (P1-12 fix).
- **Early-warning threshold** — first-class `RuleVerdict::EarlyWarning` (ops-paged) distinct from trader-facing `Warn`. Emitted at 80% of breach threshold on every breach-capable rule (P1-13 fix).
- **Override + emergency audit trail** — both record full metadata (`actor_id`, `reason`, `at`) and are persisted to the event log alongside the original verdict.

---

## Performance & Benchmarks

Benchmarks are in `benches/engine.rs` and run via `cargo bench --features serialization,in-memory-store`.

### Headline numbers

| Benchmark | Time | Throughput | Notes |
|-----------|------|------------|-------|
| `evaluate_tick_single` | ~4.2 µs | — | One tick evaluation against an account with all 22 rules registered. |
| `evaluate_order_single` | ~3.5 µs | — | Pre-trade order evaluation against an account with all 22 rules registered. |
| `pure_evaluate` | **19 µs** | ~52,600 evals/sec | The stateless pure-evaluate function (P1-7). What the platform's `/internal/v1/evaluate` endpoint calls. |
| `realistic_load/10` | 48.8 µs | **~205,000 evals/sec** | 10 accounts × 1 tick (smallest realistic batch). |
| `realistic_load/100` | 469.6 µs | **~213,000 evals/sec** | 100 accounts × 1 tick. |
| `realistic_load/1000` | **4.87 ms** | **~205,000 evals/sec** | 1000 accounts × 1 tick. This is the platform's expected scale (60s cadence × 1000 accounts). |

### Headroom analysis

The platform spec calls for serving a per-account evaluation on every bridge tick at a 60-second cadence across potentially thousands of accounts. At 1000 accounts, that's:

```
1000 accounts / 60 seconds = ~17 evaluations/second required
```

The engine sustains **205,000 evals/sec** at 1000-account batches — a **12,000× headroom** over the platform's required throughput. Single-account evaluations (`pure_evaluate`) run in ~19 microseconds, leaving ample budget for HTTP serialization, storage I/O, and network latency in the real deployment.

### Benchmark methodology

- **Tool**: `criterion 0.5` with statistical analysis (100 samples, 1s warmup, 2s measurement window).
- **What's measured**: end-to-end evaluation including rule registry iteration, context construction, all 22 rule evaluations, decision aggregation. No I/O.
- **What's NOT measured**: HTTP serialization, storage reads/writes, network. Add ~50-200 µs per request for those in production.
- **Run the benchmarks yourself**:
  ```bash
  cargo bench --features serialization,in-memory-store --bench engine -- --warm-up-time 1 --measurement-time 2
  ```

### Benchmark output (sample run)

```
evaluate_tick_single      time:   [4.1120 µs  4.2032 µs  4.2981 µs]
evaluate_order_single    time:   [3.4109 µs  3.5046 µs  3.6079 µs]
pure_evaluate            time:   [18.241 µs  19.057 µs  20.075 µs]

realistic_load/10        time:   [47.367 µs  48.755 µs  50.330 µs]
                         thrpt:  [198.69 Kelem/s  205.11 Kelem/s  211.12 Kelem/s]

realistic_load/100       time:   [457.06 µs  469.59 µs  483.30 µs]
                         thrpt:  [206.91 Kelem/s  212.95 Kelem/s  218.79 Kelem/s]

realistic_load/1000      time:   [4.7542 ms  4.8719 ms  4.9978 ms]
                         thrpt:  [200.09 Kelem/s  205.26 Kelem/s  210.34 Kelem/s]
```

### Why it's fast

- **Zero allocation in the hot path** — `RuleContext` is built once per evaluation; rules read `&RuleContext` and don't clone.
- **`Decimal` arithmetic is fast enough** — `rust_decimal`'s fixed-point math fits in 12 bytes and uses integer ops; the engine doesn't pay for arbitrary precision.
- **`Arc<dyn Rule>` registry** — one indirect call per rule, branchlessly dispatched. The 19-rule registry is small enough to fit in L1.
- **No storage in pure path** — `pure::evaluate` takes everything as input; the storage-mutating pipeline is layered on top but isn't on the critical path for stateless calls.

---

## Quick Start

```bash
# Clone & build
cargo build --release

# Run the CLI demo (end-to-end: account start → order submit → tick → risk metrics)
cargo run --release --bin propfirm-cli

# Run the HTTP server (optional)
cargo run --release --features server --bin propfirm-server

# Run all tests (integration + property + spec edge cases + doctests)
cargo test --all-features

# Run benchmarks
cargo bench --features serialization,in-memory-store
```

---

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
use propfirm::persistence::traits::AccountStore;

fn main() -> anyhow::Result<()> {
    let plan = ftmo_phase1();
    let account = Account::new(AccountId::new(), plan.clone())
        .with_tenant(propfirm::tenant::TenantId::named("my-firm"));
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

    // P1-5: broker-is-truth tick — equity comes from the broker, not recomputed.
    let tick = Tick::new(Symbol::new("EURUSD"),
        Quote { bid: Price(dec!(1.0850)), ask: Price(dec!(1.0852)), ts: chrono::Utc::now() });
    let result = pipeline.process(account.id, PipelineEvent::Tick {
        tick, broker_equity: dec!(10_200).into(), broker_balance: dec!(10_000).into(),
    })?;
    println!("Tick decision: {:?}", result.snapshot.decision.kind);
    Ok(())
}
```

---

## Architecture

```
┌──────────────────────────────────────────────────────────────────┐
│                       HTTP API (axum)                            │
│  /internal/v1/evaluate  /override  /manual-run  /breach-report  │
│  /v1/evaluate-order  /v1/accounts/:id  /v1/rule-packs{,/:id}    │
└────────────────────────┬─────────────────────────────────────────┘
                         │
                         ▼
┌──────────────────────────────────────────────────────────────────┐
│                          Pipeline                                │
│  apply_event → build_context → evaluate → persist →             │
│  emit_decision_event → notify                                   │
│                                                                 │
│  Guards:                                                        │
│   • P1-5  equity_input (BrokerReported | Estimated)            │
│   • P1-14 stale-tick (>10min) + out-of-order rejection         │
│   • P1-8  optimistic concurrency (version on every write)     │
└───────┬────────────┬──────────────┬─────────────────────────────┘
        │            │              │
        ▼            ▼              ▼
┌──────────┐  ┌──────────┐  ┌──────────────┐
│ Evaluator│  │  State   │  │ Event Store  │
│ + Rules  │  │  Delta   │  │ (audit log)  │
│          │  │          │  │              │
│ P1-7:    │  │ P0-2:    │  │ Replay for   │
│ pure     │  │ target_  │  │ dispute      │
│ evaluate │  │ reached_ │  │ resolution   │
│ + hash   │  │ at sticky│  │              │
└────┬─────┘  └──────────┘  └──────────────┘
     │
     ▼
┌──────────────────────────────────────────────────────────────────┐
│                     Rule Registry                               │
│                                                                 │
│  P1-6: build_from_pack(&RulePack) — rules are DATA, not code   │
│                                                                 │
│  Daily DD | Max DD | Trailing DD | Profit Target |              │
│  Min Days | Consistency | News | Overnight | Weekend |          │
│  Max Pos Size | Max Open | Max Daily Trades | Time |             │
│  Cooldown | Hedging | Grid | Copy Trading | SL/TP |             │
│                                                                 │
│  P0-4: each rule declares priority() — breach arbitration is   │
│        deterministic, NOT by registration order                  │
│  P2:   each rule declares tolerance_cents() (default 1¢)        │
└──────────────────────────────────────────────────────────────────┘
```

---

## HTTP API Surface

The binding spec's engine contract is fully exposed:

### Internal API (platform-side, called by LCC/bridge)

| Method | Path | Purpose |
|--------|------|---------|
| `POST` | `/internal/v1/evaluate` | Stateless evaluate contract. Takes `{account_id, rule_pack, tick}`, returns `{verdict, state_after, input_hash}`. Calls `pure::evaluate` — no storage mutation. |
| `POST` | `/internal/v1/override` | Clear a false-positive breach. Creates an `Override` record + reverts `Failed`/`EmergencyStopped` → `Active`. |
| `POST` | `/internal/v1/manual-run` | Force re-evaluation of an account (on-demand). |
| `GET` | `/internal/v1/breach-report/:account_id` | Trader-facing "why did I fail" view (TD-25). Returns all violations from the event log with full audit trail. |

### Public API (tenant-facing)

| Method | Path | Purpose |
|--------|------|---------|
| `POST` | `/v1/evaluate-order` | Pre-trade order evaluation. |
| `GET` | `/v1/accounts/:id` | Account snapshot. |
| `POST` | `/v1/rule-packs` | Create a new rule pack (draft). |
| `GET` | `/v1/rule-packs/:id` | Get a rule pack by id. |
| `PATCH` | `/v1/rule-packs/:id` | Update a draft rule pack. |
| `POST` | `/v1/rule-packs/:id/activate` | Promote draft → active. |
| `POST` | `/v1/rule-packs/:id/supersede` | Mark active → superseded. |
| `GET` | `/health` | Health check. |

All mutating endpoints accept an `Idempotency-Key` header.

---

## Configuration

Each challenge plan is a `ChallengePlan` struct with builder methods:

```rust
use propfirm::config::plan::{ChallengePlan, LossReference};
use propfirm::prelude::*;

let plan = ChallengePlan::default()
    .with_balance(Money(dec!(100_000)))
    .with_profit_target(Pct(dec!(0.08)))
    .with_daily_dd(Pct(dec!(0.05)))
    .with_total_dd(Pct(dec!(0.10)))
    // P0-1: explicit static vs trailing max-loss reference.
    .with_loss_reference(LossReference::Static)
    .with_min_days(3)
    .with_time_limit_days(30)
    .with_consistency(Pct(dec!(0.40)));
```

---

## Rule Pack as Data

Rule packs are versioned JSON data, not compiled Rust. This is the binding spec's requirement (EVL-01/02): tenant admins can edit thresholds through a form, bind a pack to an account at purchase, and re-bind only via an explicit, audited action (EVL-34) — never by recompiling and redeploying the engine.

```json
{
  "id": "funderblu-default-v3",
  "version": 3,
  "tenant_id": "...",
  "lifecycle": "active",
  "effective_from": "2026-09-01T00:00:00Z",
  "rules": [
    {
      "id": "max_total_loss",
      "kind": "max_drawdown",
      "basis": "static",
      "unit": "percent",
      "value": 0.10,
      "tolerance_cents": 1,
      "early_warning_pct": 0.80,
      "priority": 1000
    }
  ],
  "content_hash": "sha256:..."
}
```

`RuleRegistry::build_from_pack(&pack)` interprets the data at request time, mapping each `kind` to its concrete rule implementation. Pack lifecycle: `draft` → `active` → `superseded`, with `effective_from` timestamps.

---

## Testing

**105 tests, all passing:**

| Suite | Tests | Purpose |
|-------|-------|---------|
| Unit tests (`src/`) | 20 | Core logic: account, money, types, pipeline, config, rulepack, risk metrics. |
| Integration tests (`tests/integration.rs`) | 39 | End-to-end behavior: presets validate, account lifecycle, drawdown breaches, profit target, hedging, position limits, pipeline, override, emergency stop, optimistic concurrency, tenant isolation, pure-evaluate determinism. |
| P0 default rules & units (`tests/p0_default_rules_and_units.rs`) | 20 | Rule registry: all 22 rule kinds produce correct verdicts from plan defaults; unit/basis parsing; pack-driven parameterization. |
| P0 pack-driven (`tests/p0_pack_driven.rs`) | 4 | Pack overrides: pack basis overrides plan basis, tolerance override, tenant isolation, pack priority override. |
| Property tests (`tests/property_tests.rs`) | 6 | `proptest`-driven invariants: drawdown non-negativity, static-floor immutability, trailing-floor monotonicity, stateless determinism, decision invariance under rule reordering, breach severity ordering. |
| Spec edge cases (`tests/spec_edge_cases.rs`) | 15 | Named, permanent regression tests pinning edge semantics from spec §3.4: equity-at-limit-fires, target-reached-stays-pending, breach-beats-pass, static-floor-never-moves, trailing-floor-floats, estimated-equity-can't-terminate, broker-equity-can-terminate, tolerance-absorbs-subcent-noise, override-clears-breach, emergency-stop, auto-rollover-on-future-tick, auto-rollover-not-triggered-for-current-day, EOD-trailing-floor-resets-once-per-day, effective-money-none-must-return-none, mark_active_trading_day_wired_and_idempotent. |
| Doctests | 1 | README example compiles. |

```bash
# Run the full suite
cargo test --all-features

# Run property tests only (verifies invariants across the input space)
cargo test --features serialization,in-memory-store --test property_tests

# Run spec edge cases only (the "must never silently change" set)
cargo test --features serialization,in-memory-store --test spec_edge_cases
```

---

## Compliance & Audit

Every verdict is reproducible from its recorded inputs:

- **`input_hash`** (sha256) is computed from `(account_state, rule_pack, tick, open_positions, today_trades, pending_order, latest_trade, latest_tick)` and persisted alongside the verdict. Any past decision can be recomputed byte-for-byte — the binding spec's dispute-resolution mechanism.
- **Append-only event log** records every state transition (`AccountStarted`, `TradeFilled`, `TickEvaluated`, `DayRollover`, `RuleViolated`, `AccountStatusChanged`) with causation ids linking events to their triggering input.
- **Override records** never delete the original violation — they're persisted alongside it as the rebuttal, with `actor_id` + `reason` + `at` for full audit trail.
- **Emergency stops** carry `actor_id` + `reason` + `at` and short-circuit all other rules with the highest possible priority (10,000).

---

## License

MIT OR Apache-2.0
