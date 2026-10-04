# Observability

D81: the engine is a stateless compute service — no database, no Redis,
no worker. Observability covers the HTTP server only. The platform's
event processing system has its own observability stack.

## Logging

Logs are emitted via `tracing` + `tracing-subscriber`. The log format
and filter are configurable via `[observability]` in `config/propfirm.toml`
or `PROPFIRM_OBSERVABILITY__*` env vars.

```toml
[observability]
log_filter = "info,propfirm=debug"
log_format = "json"          # json (prod) | pretty (dev)
metrics_enabled = true
metrics_path = "/metrics"
panic_hook = true
```

### Log levels

| Level | What you see |
|-------|-------------|
| `error` | Rule evaluation failures, panic hook catches |
| `warn` | Idempotency conflicts, NOSCRIPT reloads, stateless-engine startup warnings |
| `info` | Server startup, per-request `http` span, evaluate verdicts for non-Pass decisions |
| `debug` | Per-handler decision logs, idempotency hits/misses, floor hint computation |
| `trace` | Full rule-by-rule evaluation trace |

## Metrics

All metrics are registered via the global `metrics` recorder installed by
`PrometheusBuilder::install_recorder()` (idempotent via `OnceLock`). The
`/metrics` endpoint renders Prometheus-format text.

### Available metrics — HTTP server

| Metric | Type | Labels | Source |
|--------|------|--------|--------|
| `propfirm_http_requests_total` | counter | `method`, `status` | `record_http_response()` (outermost axum middleware) |
| `propfirm_evaluate_decisions_total` | counter | `kind` | `record_decision()` (evaluate handler) |
| `propfirm_idempotency_outcomes_total` | counter | `outcome` | `record_idempotency_outcome()` (evaluate handler) |
| `propfirm_errors_total` | counter | `endpoint`, `kind` | `record_error()` (handlers on 4xx/5xx) |
| `propfirm_request_duration_seconds` | histogram | `endpoint` | `LatencyScope` RAII on drop |

### Sample /metrics output

```
# HELP propfirm_http_requests_total HTTP request counter (method + status)
# TYPE propfirm_http_requests_total counter
propfirm_http_requests_total{method="POST",status="200"} 4521
propfirm_http_requests_total{method="POST",status="409"} 3

# HELP propfirm_evaluate_decisions_total Decision kind counter
# TYPE propfirm_evaluate_decisions_total counter
propfirm_evaluate_decisions_total{kind="Pass"} 4400
propfirm_evaluate_decisions_total{kind="Warn"} 100
propfirm_evaluate_decisions_total{kind="Liquidate"} 21

# HELP propfirm_request_duration_seconds Request latency histogram
# TYPE propfirm_request_duration_seconds histogram
propfirm_request_duration_seconds_bucket{endpoint="/internal/v1/evaluate",le="0.001"} 4100
propfirm_request_duration_seconds_bucket{endpoint="/internal/v1/evaluate",le="0.005"} 4500
propfirm_request_duration_seconds_bucket{endpoint="/internal/v1/evaluate",le="+Inf"} 4521
```

## Distributed tracing (OTLP)

When the `otel` cargo feature is enabled AND `observability.otlp.endpoint`
is non-empty, the engine installs an OTLP layer alongside the JSON log
layer. Spans are exported to a collector (Tempo / Jaeger / Honeycomb).

The `TraceLayer` in `src/api/routes.rs` creates a per-request span with:
- `method`, `uri`, `request_id` (from `x-request-id` header)
- `status` (filled by `on_response` — the HTTP status code)
- `latency_ms` (filled by `on_response`)
- `error` (filled by `on_failure` for 5xx)

Every handler also has `#[tracing::instrument]` for per-handler spans.

## Floor hints (D81 §4.5)

The evaluate response includes `hints.floor_daily_cents`,
`hints.floor_total_cents`, `hints.target_equity_cents`,
`hints.floors_version`. These are advisory (I-27: the engine never
reads them back). **The platform's event processing system persists them**
into `evaluation_state.floor_*` and may publish them via its preferred
mechanism (e.g., Redis pub/sub, database, or message queue).

## Panic safety

When `observability.panic_hook = true`, panics are routed through
`tracing::error` instead of the default stderr print. Handler panics in
axum are caught by the framework and turned into 500 responses, but the
panic message + backtrace are preserved in the log.

Additionally, each rule evaluation is wrapped in `catch_unwind` — a
buggy rule degrades to `Warn` instead of crashing the process.

## Health endpoints

| Endpoint | Purpose |
|----------|---------|
| `GET /health` | Liveness probe — always returns `ok` |
| `GET /ready` | Readiness probe — verifies the in-memory idempotency backend is wired. D81: no DB dependency check (there is no DB). |
| `GET /metrics` | Prometheus scrape target |

## Platform Observability Responsibilities

While this document covers engine observability, the platform is responsible for:

1. **Event Processing Observability**: Monitoring the consumption and processing of `DomainEvent` objects emitted by the engine
2. **Storage Observability**: Monitoring account state storage, idempotency stores, and event stores
3. **Audit Trail Observability**: Monitoring the completeness and correctness of the audit trail built from engine events
4. **End-to-end Latency**: Measuring total time from request receipt to response emission including platform processing
5. **Platform-specific Metrics**: Idempotency outcomes, event processing lag, storage performance, etc.

The engine focuses on evaluation correctness and emits sufficient events for the platform to build a complete observability picture.
