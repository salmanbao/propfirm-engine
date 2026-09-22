# Features

This document lists the rules, architectural properties, and operational controls shipped with the engine.

## Rule library

The engine includes 22 rule evaluators organized by category. Each rule is data-driven: thresholds, units, and priorities come from `ChallengePlan` or `RulePack`, not hard-coded constants.

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

### Time

| Rule | Description |
|------|-------------|
| Time limit | Enforces the overall time limit for a challenge phase. |
| Stop-loss required | Requires every trade to have a stop-loss attached. |
| Take-profit required | Requires every trade to have a take-profit attached. |
| Inactivity termination | Terminates an account if it is inactive for too many days. |

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
- **Optimistic concurrency control** — `AccountStore::put_with_version(expected)` returns `Error::StateConflict` on mismatch. No silent last-write-wins clobbering.
- **Tenant isolation** — `TenantId` threaded through `Account`, `ChallengePlan`, `Violation`, `RulePack`. `AccountStore::get_for_tenant(tenant_id, account_id)` filters at the storage layer.
- **Stale & out-of-order tick guard** — ticks older than 10 minutes (configurable) or older than the last-evaluated tick are rejected with `Error::TickRejected` before evaluation runs.
- **Decimal precision** — all monetary values use `rust_decimal::Decimal`; no floating-point drift on money.
- **Panic safety** — the registry wraps each rule evaluation in `catch_unwind`. A panicking rule degrades to a `Warn` verdict; the process stays alive.

## Operational controls

- **Manual override** — `Override` record (`clears_violation_id`, `reason`, `actor_id`, `at`) clears a false-positive breach without deleting the original verdict. State machine: `Failed` / `EmergencyStopped` → `Active`.
- **Emergency stop** — `PipelineEvent::EmergencyStop { reason, actor_id, at }` short-circuits normal rule evaluation and forces `DecisionKind::Emergency`. Beats every other verdict, including `Liquidate`.
- **Early-warning threshold** — first-class `RuleVerdict::EarlyWarning` (ops-paged) distinct from trader-facing `Warn`. Emitted at 80% of breach threshold on every breach-capable rule.
- **Override + emergency audit trail** — both record full metadata (`actor_id`, `reason`, `at`) and are persisted to the event log alongside the original verdict.
- **Idempotency** — all mutating HTTP endpoints accept an `Idempotency-Key` header. The server tracks the last N keys per endpoint to deduplicate retries.

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

## Persistence backends

| Backend | Feature flag | Description |
|---------|-------------|-------------|
| In-memory | `in-memory-store` | Default. Fast, non-persistent. Used for tests and benchmarks. |
| Postgres | `postgres` | Production implementation. Enforces tenant isolation at the SQL layer. Supports optimistic concurrency. |

## HTTP API

Optional `axum`-based server exposing REST endpoints for account evaluation, rule pack management, and breach reporting. See the [integration guide](integration.md) for endpoint details.

## Performance

Benchmarks are in `benches/engine.rs` and run via `cargo bench --features serialization,in-memory-store`.

### Headline numbers

| Benchmark | Time | Throughput | Notes |
|-----------|------|------------|-------|
| `evaluate_tick_single` | ~4.2 µs | — | One tick evaluation against an account with all 22 rules registered. |
| `evaluate_order_single` | ~3.5 µs | — | Pre-trade order evaluation against an account with all 22 rules registered. |
| `pure_evaluate` | **19 µs** | ~52,600 evals/sec | The stateless pure-evaluate function. |
| `realistic_load/10` | 48.8 µs | **~205,000 evals/sec** | 10 accounts × 1 tick. |
| `realistic_load/100` | 469.6 µs | **~213,000 evals/sec** | 100 accounts × 1 tick. |
| `realistic_load/1000` | **4.87 ms** | **~205,000 evals/sec** | 1000 accounts × 1 tick. |

## Testing

136+ tests across unit, integration, property, spec edge case, API, auth, and batch suites. All passing with `cargo test --all-features`.

| Suite | Tests | Purpose |
|-------|-------|---------|
| Unit tests (`src/`) | 16 | Core logic: auth, instrument registry, payout engine, doctests. |
| Integration tests (`tests/integration.rs`) | 39 | End-to-end behavior: presets, account lifecycle, drawdown breaches, profit target, hedging, position limits, pipeline, override, emergency stop, optimistic concurrency, tenant isolation, pure-evaluate determinism. |
| P0 default rules & units (`tests/p0_default_rules_and_units.rs`) | 20 | Rule registry: all 22 rule kinds produce correct verdicts from plan defaults; unit/basis parsing; pack-driven parameterization. |
| P0 pack-driven (`tests/p0_pack_driven.rs`) | 4 | Pack overrides: pack basis overrides plan basis, tolerance override, tenant isolation, pack priority override. |
| Property tests (`tests/property_tests.rs`) | 12 | `proptest`-driven invariants: drawdown non-negativity, static-floor immutability, trailing-floor monotonicity, stateless determinism, decision invariance under rule reordering, breach severity ordering, CI guard for rule-level verdict producers, GapFlagged production proof. |
| Spec edge cases (`tests/spec_edge_cases.rs`) | 18 | Named, permanent regression tests pinning edge semantics. |
| API integration tests (`tests/api_integration.rs`) | 9 | Binding spec HTTP endpoints: evaluate, override, manual-run, breach-report, evaluate-order. |
| API P0 fixes (`tests/api_p0_fixes.rs`) | 10 | Contract enforcement: idempotency, error-shape, overrides, etc. |
| Auth A.1 tests (`tests/auth_a1.rs`) | 10 | HTTP authentication: missing/wrong credentials ⇒ 401, valid key ⇒ 200, tenant mismatch ⇒ 403, `/health` & `/ready` exempt, `/internal/*` service-token-only, env fail-closed. |
| Batch tests (`tests/ab_batch.rs`, `tests/c_batch.rs`, `tests/d1_martingale.rs`) | 25 | Cross-account copy-trading detection, pack-driven thresholds, instrument spec units→lots conversion, margin/max_total_lots/trading_hours rules, martingale/grid detection. |
