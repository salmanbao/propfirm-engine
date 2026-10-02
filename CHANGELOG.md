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
