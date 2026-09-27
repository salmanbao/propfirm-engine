# Architecture

This document describes the internal architecture of the prop firm engine, how the major modules connect, and the design decisions behind the evaluation flow.

## Layered structure

The crate is organized as a layered library. Each layer has a single responsibility and depends only on layers beneath it.

```
core
config
rules
engine
persistence
events
notifications
reporting
pure
api (optional)
```

### Core

`core` defines immutable value types and domain aggregates:

- `Account`, `Position`, `Order`, `Trade`, `Tick`
- `Money`, `Price`, `Quantity`, `Lots`
- `Violation`, `ViolationKind`, `ViolationSeverity`
- `AccountStatus`, `ChallengePlan`, `PackLifecycle`

All monetary values use `rust_decimal::Decimal`. There is no floating-point arithmetic on money anywhere in the crate.

### Config

`config` contains `ChallengePlan` definitions and preset factories. A plan declares thresholds, enabled rules, loss-reference mode, and time limits. Presets cover FTMO, MyForexFunds, The Funded Trader, SurgeTrader, and custom configurations.

### Rules

`rules` contains the rule trait, context, registry, and all concrete evaluators.

- `Rule` — the trait every rule implements. It exposes `id()`, `name()`, `kind()`, `scope()`, `severity()`, `priority()`, `tolerance_cents()`, and `evaluate()`.
- `RuleContext` — the read-only context passed into every rule evaluation. It carries the account, open positions, today's trades, pending order, latest tick, recent events, cross-reference trades, instruments, and server time.
- `RuleRegistry` — owns the ordered list of rules and evaluates them all against a context. It catches panics from buggy rules and degrades them to `Warn` so one bad rule cannot take down the process.
- Evaluators — concrete rule implementations under `rules/evaluators/`. Each evaluator reads from `RuleContext` and returns a `RuleVerdict`.

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

### Persistence

**ADR-11**: account persistence was removed from the engine. What remains:

- `events::store::EventStore` — async trait (`append` / `all` / `recent` / `replay`) with the in-memory implementation in `persistence/memory.rs`. Read-side seam used by breach-report and override replay; ready to be backed by durable storage.
- `persistence/traits.rs` — sync no-op placeholder documenting why the engine no longer defines account CRUD.
- `IdempotencyStore` — lives in `api::idempotency` (server feature); backs `Idempotency-Key` on `POST /internal/v1/evaluate`.

### Events

`events` defines the async `EventStore` trait (`append`, `all`, `recent`, `replay`) plus an in-memory implementation. Domain events carry causation IDs linking them to their triggering inputs; the pipeline emits them on `PipelineResult.events` and persisting them is the caller's decision (ADR-11). `replay` is the dispute-resolution seam for rebuilding account state from stored events.

### Notifications

`notifications` defines the `Notifier` trait. Implementations can deliver rule violations via webhook, email, push, or any other channel. `LogNotifier` is the default for development.

### Reporting

`reporting` builds structured `PerformanceReport` summaries combining rule status and risk metrics.

### API (optional)

`api` is an optional `axum`-based HTTP server. It exposes:

- Internal endpoints (service-token authenticated): `/internal/v1/evaluate`, `/internal/v1/override`, `/internal/v1/manual-run`, `/internal/v1/emergency-stop`, `/internal/v1/breach-report/:account_id`
- Public endpoints (tenant-key or service-token authenticated): `/v1/evaluate-order`, `/v1/rule-packs/validate`
- Unauthenticated probes: `/health`, `/ready`

All mutating endpoints accept an `Idempotency-Key` header.

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

## Key design decisions

- **Broker-is-truth equity**: The engine never recomputes equity from positions + quote. `EquityInput::BrokerReported` vs `EquityInput::Estimated` is a type-level distinction. Breach-capable rules refuse to terminate on an estimate.
- **Stateless pure evaluate**: `pure::evaluate()` produces an `input_hash` so any past verdict can be recomputed byte-for-byte from its recorded inputs.
- **Caller-owned concurrency**: The server keeps no account state (ADR-11). `Account.version` travels in the request's `account_state` and is hashed into `input_hash`; `Error::StateConflict` remains the typed error for caller-side optimistic concurrency.
- **Tenant isolation**: `TenantId` is threaded through `Account`, `ChallengePlan`, `Violation`, `RulePack` and enforced at the HTTP auth layer (cross-tenant request ⇒ 403).
- **Stale tick guard**: Ticks older than 10 minutes (configurable) or older than the last-evaluated tick are rejected before evaluation runs.
- **Panic safety**: The registry wraps each rule evaluation in `catch_unwind`. A panicking rule degrades to a `Warn` verdict; the process stays alive.
- **Rule priority over registration order**: Each rule declares a numeric `priority()`. `Decision::from_reports` picks the highest-priority verdict. Reordering rules in a pack does not change outcomes.
