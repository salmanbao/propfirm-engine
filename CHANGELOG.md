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
