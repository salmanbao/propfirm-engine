# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added
- `tracing-flame` layer for flame-graph profiling (gated behind `flame` cargo feature)
- `utoipa` + `utoipa-swagger-ui` for OpenAPI spec + Swagger UI (gated behind `openapi` cargo feature)
- `tracing-test` for unit-test span assertions
- `cargo-machete` CI job to detect unused dependencies
- `cargo-careful` CI job to run tests with extra UB detection
- `worker reset-group` subcommand to recreate consumer groups
- `profirm-cli --json` output mode for jq piping
- Go client library under `clients/go/`
- `CARGO_LOCK_POLICY.md` documenting when to run `cargo update`
- `SECURITY.md` with vulnerability disclosure process
- `dependabot.yml` for automated dep upgrade PRs
- `event_bus.max_len` setting (default `100_000`): every `XADD` carries
  `MAXLEN ~ <max_len>` so streams are trimmed approximately to that
  length. `max_len = 0` disables trimming (back-compat).
- `redis.ioThreads` / `redis.ioThreadsDoReads` Helm knobs (default
  `4` / `"yes"`); also wired into all `docker-compose` / `podman-compose`
  redis launch commands. Offloads socket I/O to worker threads.
- `worker.metricsPort` Helm knob (default `8081`) + new
  `templates/service-worker.yaml` Service (port `metrics`) + extended
  `templates/servicemonitor.yaml` to also scrape the worker when
  `worker.serviceMonitor.enabled = true` (default false). Previously
  the worker started a `/metrics` HTTP server on `:8081` but the Helm
  chart didn't declare the containerPort and had no Service /
  ServiceMonitor for the worker — so `propfirm_event_bus_*` counters
  were un-scrapable in production.
- `record_http_response(method, status)` helper in `src/api/metrics.rs`
  emitting the `propfirm_http_requests_total` counter with `method`
  and `status` labels. Restores the metric removed in `e431ed1` and
  reactivates the critical `PropfirmHighErrorRate` Grafana alert that
  was silently querying a non-existent series.
- `GET /internal/v1/audit-log` endpoint for querying the audit_log
  table via the engine itself (no direct Postgres access needed).
  Supports `tenant_id`, `account_id`, `action`, `since` (RFC3339),
  `limit` filters. Tenant isolation is enforced: the query
  `tenant_id` must match the `X-Tenant-Id` header, or `account_id`
  must be set. Limit clamped to [1, 500]. Returns rows ordered by
  `occurred_at DESC, id DESC`. See `src/api/audit_log.rs::query_entries`
  and `src/api/handlers.rs::audit_log`.
- `0002_idempotency_active_idx.sql` migration adding a partial index
  `idempotency_active_idx ON idempotency (composite_key) WHERE
  expires_at > now()`. Smaller index than the full-table btree
  (only currently-active rows), better cache locality, smaller
  autovacuum footprint. Paired with a `lookup()` SQL change to use
  SQL's `now()` directly instead of a bind parameter so the planner
  recognizes the partial-index predicate is implied.
- `DailyLossType` enum (`None`, `PctInitial`, `PctPriorDay`,
  `TrailingIntradayHigh`) on `ChallengePlan` — maps 1:1 to the
  propfirm-rules-dataset's `daily_loss.type` field. Default
  `PctPriorDay` preserves the engine's historical behavior; every
  preset that needs a different variant sets it explicitly. Closes
  the silent mis-encoding of FTMO/FundedNext/FundingPips/The5%ers/
  Bitfunded/Apex EOD (all `pct_initial`) and HyroTrader Standard
  (`trailing_intraday_high`).
- `ConsistencyType` enum (`None`, `BestDayPctOfTotal`,
  `BestDayPctOfPositiveDays`) on `ChallengePlan` — maps 1:1 to the
  dataset's `consistency.type` field. Default
  `BestDayPctOfPositiveDays` preserves the engine's historical
  behavior; HyroTrader (all 4 plans) now correctly uses
  `BestDayPctOfTotal` (denominator = `total_realized_pnl`, not
  `sum_positive_days_profit`).
- `LossReference::IntradayTrail` variant — the dataset's harshest
  max-drawdown mechanism, where the floor follows the highest
  **unrealised equity** peak (`Account::peak_equity`), not the
  closed-balance peak. Used by Apex Intraday Trail, FundingPips Zero,
  Breakout 2-Step. Paired with the `RuleBasis::IntradayTrail` variant
  for pack entries.
- `eod_trail_locks_at_start: bool` flag on `ChallengePlan` — when
  `true` and `max_loss_reference = EodTrailing`, the trailing floor
  caps at `initial_balance` once it would otherwise trail past it.
  After the lock engages, the worst case is returning to breakeven
  rather than being breached. Used by TopStep (all 3 Combines),
  Breakout 2-Step, FundedNext Stellar Instant, FundingPips Zero.
- `daily_loss_soft: bool` flag on `ChallengePlan` — when `true`,
  a daily-loss breach is a soft warning (severity = `Warning`)
  rather than a hard breach (severity = `Liquidate`). Used by Apex
  EOD Trail (the only verified firm whose daily loss is `soft: true`).
- `min_profitable_days: Option<u32>` field on `ChallengePlan` +
  new `MinProfitableDaysRule` evaluator — counts only **profitable**
  trading days (days where `today_realized_pnl > 0` at rollover),
  distinct from `min_trading_days` which counts any trade-day.
  Used by FundingPips Zero (requires 7 profitable days).
- `Account::intraday_peak_equity` field — highest unrealised equity
  peak observed during the *current* trading day. Updated on every
  tick via `EvaluationState::update_equity`; reset at day rollover.
  Used by HyroTrader Standard's `trailing_intraday_high` daily-loss
  mechanism. Distinct from `peak_equity` (all-time peak, never resets).
- `Account::profitable_days_count` field — count of profitable
  trading days, incremented at `EvaluationState::rollover_day` when
  `today_realized_pnl > 0`. Used by the new `MinProfitableDaysRule`.
- `ViolationKind::MinProfitableDays` variant for the new rule's
  violation category.

### Changed
- **Worker hot path**: `produce_response().await` + `ack().await` (two
  sequential RTs) collapsed to one `produce_response_and_ack()` call.
  Single-node Redis uses a true `redis::pipe()` (one TCP packet, one
  batch read); cluster path falls back to `tokio::join!` (concurrent,
  cuts wall-time from `RT1 + RT2` to `max(RT1, RT2)`).
- **Redis idempotency**: `EVAL` switched to `EVALSHA` with lazy
  `SCRIPT LOAD` + cached SHA1 + `NOSCRIPT` fallback that invalidates
  the cache and reloads transparently. Saves ~300 bytes/call on the
  idempotency hot path.
- **Postgres idempotency**: `INSERT … ON CONFLICT DO NOTHING` (two RTs:
  INSERT then SELECT) collapsed to `INSERT … ON CONFLICT DO UPDATE
  SET expires_at = idempotency.expires_at RETURNING (xmax = 0) AS fresh,
  body_hash, response, expires_at` — one RT on every conflict (the
  common steady-state path). Rare expired-row case handled by a separate
  `revive_expired()` UPDATE.
- **Postgres idempotency `lookup()` SQL**: changed `WHERE expires_at > $2`
  (bind param) to `WHERE expires_at > now()` (SQL function). The bind
  form couldn't be matched by the new partial index
  `idempotency_active_idx WHERE expires_at > now()` because the planner
  can't statically know `$1 ≈ now()`. The SQL `now()` form is recognized
  as implied by the partial index predicate, so the index kicks in.
- **HTTP metrics middleware**: new `http_request_metrics` axum
  middleware (`from_fn`) added as the **outermost** layer in
  `src/api/routes.rs`. Increments `propfirm_http_requests_total`
  with `method` + `status` labels for EVERY response, including the
  framework-level rejections (408 timeout, 413 body-limit, 404
  malformed routing) that bypass the handler body entirely. The
  previous `record_request()` helper was removed in `e431ed1` because
  no Rust code called it — but the Helm alert rule + dashboard still
  queried the metric, silently breaking the `PropfirmHighErrorRate`
  alert (severity: page). This time the counter is incremented by a
  real middleware, not a dead function.
- **TraceLayer `on_response` + `on_failure` hooks**: the per-request
  span declared in `TraceLayer::make_span_with` now carries
  `status = field::Empty`, `latency_ms = field::Empty`,
  `error = field::Empty` placeholders. The `on_response` hook fills
  `status` + `latency_ms` after the response is built; the
  `on_failure` hook fills `error` for 5xx. Without this, OTLP
  consumers (Tempo / Jaeger / Honeycomb) could see "request hit
  /internal/v1/evaluate" but not "and returned 500" — a real
  attribution gap for an audit-grade system. The metric side is
  handled by the outermost `http_request_metrics` middleware; this
  fills the trace side.
- **Audit-log `request_hash`**: the 5 sensitive handlers that didn't
  hash their request body (`override_breach`, `manual_run`,
  `emergency_stop`, `breach_report`, `evaluate_order`) now compute
  `hash_body(serde_json::to_string(&req))` at the top of the handler
  (before any field move) and chain `.with_request_hash(body_hash)`
  on the audit entry. Matches the existing pattern in
  `evaluate_internal` / `worker_evaluate`. Disputes can now
  byte-for-byte verify the exact request that triggered any audited
  action, not just evaluations.

## [0.2.0] — 2026-09-30

### Added — Production-grade internal service

- **Durable persistence**: PostgreSQL event store (`PostgresEventStore`)
  + idempotency backend (`PostgresIdempotencyBackend`) with auto-migrations;
  Redis idempotency backend (`RedisIdempotencyBackend`) with atomic Lua
  script; Redis Streams event bus (`RedisEventBus`) with consumer groups +
  `XAUTOCLAIM` PEL recovery.
- **Observability**: `tracing` + `tracing-subscriber` (JSON/pretty +
  EnvFilter); Prometheus metrics via `metrics-exporter-prometheus`
  (`/metrics` endpoint); panic hook; per-handler metrics
  (`evaluate_decisions_total`, `idempotency_outcomes_total`,
  `errors_total`, `request_duration_seconds`); worker metrics
  (`event_bus_messages_consumed_total`, `_produced_total`,
  `_acked_total`, `_claimed_total`, `_errors_total`).
- **OpenTelemetry OTLP exporter** (gated behind `otel` cargo feature):
  gRPC/HTTP transport to Tempo/Jaeger/Honeycomb/etc.
- **TLS termination**: in-process `rustls` with optional mTLS
  (`WebPkiClientVerifier`).
- **Graceful shutdown**: SIGINT/SIGTERM with `CancellationToken` fan-out
  to all worker tasks.
- **Request timeout enforcement**: `tower-http` `TimeoutLayer` +
  `RequestBodyLimitLayer` + `CompressionLayer`.
- **Idempotency store**: `memory` / `postgres` / `redis` backends;
  TTL configurable.
- **bb8-redis pool**: replaces `tokio::sync::Mutex<ClusterConnection>`
  for true concurrent cluster ops.
- **Worker subcommands**: `healthcheck`, `metrics`, `status`, `drain`.
- **CLI REPL**: `propfirm-cli repl` for interactive evaluation.
- **Audit log**: `audit_log` table writes from all sensitive handlers
  (override, emergency-stop, manual-run, breach-report, evaluate-order,
  evaluate-internal non-Pass, worker evaluate/error).
- **`#[tracing::instrument]`** on all HTTP handlers + worker functions.
- **k6 load-test** script with custom metrics + thresholds.
- **Helm chart**: Deployment + Service + HPA + PDB + NetworkPolicy +
  ConfigMap + Secret + Ingress + ServiceMonitor + optional Postgres/
  Redis StatefulSets + Grafana dashboard auto-import + alerting rules.
- **CI**: 9 parallel jobs (check, nextest, coverage, outdated, chaos,
  fuzz, security-audit, cargo-deny, sbom, otel-e2e, proof-of-build).
- **Supply-chain security**: `cargo audit` (RUSTSEC), `cargo deny`
  (advisories + licenses + bans + sources), CycloneDX SBOM embedded in
  Docker image + OCI labels.
- **Documentation**: `docs/configuration.md`, `docs/local-dev.md`,
  `docs/persistence.md`, `docs/observability.md`, `docs/tls.md`,
  `docs/event-bus.md`.

### Removed — Authentication

- Deleted `src/api/auth.rs` and `tests/auth_a1.rs`.
- Dropped `subtle` dependency.
- Removed `Extension<AuthedIdentity>` extractors + `auth_layer` from routes.
- The engine is now internal-only; trust is established at the network
  boundary (private compose network, k8s NetworkPolicy).

## [0.1.0] — 2026-09-29

### Added — Initial release

- 22 rule evaluators (drawdown, targets, trade restrictions, position
  limits, time).
- `pure::evaluate` stateless function with `input_hash` (sha256) for
  byte-for-byte reproducibility.
- Broker-is-truth equity semantics (`EquityInput::BrokerReported` vs
  `Estimated`).
- Panic-safe `RuleRegistry::evaluate` (`catch_unwind` per rule).
- `RulePack` as data (JSON, not compiled Rust) with `Draft → Active →
  Superseded` lifecycle.
- 11 preset challenge plans (FTMO Phase 1/2/Funded, FTMO 1-Step, etc.).
- `ChallengePlan` builder DSL with timezone-aware day rollover.
- `RiskMetrics` (Sharpe, Sortino, Calmar, VaR, ES, etc.).
- `Pipeline` with 12 `PipelineEvent` variants.
- `Override` + `EmergencyStop` audit trails.
- `LiquidationInstruction` for the broker bridge.
- `PayoutEngine` with HWM-style profit basis + tier ladder.
- 166 tests (unit + integration + property + spec edge cases + API).
