# Observability

The propfirm-engine ships with **three pillars** of observability:

1. **Structured logging** via `tracing` + `tracing-subscriber` (JSON to stdout)
2. **Prometheus metrics** via `metrics` + `metrics-exporter-prometheus` (`/metrics`)
3. **Panic hook** that routes panics through `tracing::error` instead of stderr

OpenTelemetry / distributed tracing is intentionally NOT wired in v1 —
the engine is internal-only and the platform gateway injects the
trace context for any cross-service spans. If you ever expose the
service publicly, add `tracing-opentelemetry` + an OTLP exporter.

## Configuration

All settings live under `[observability]` in `config/propfirm.toml`:

```toml
[observability]
log_filter = "info,propfirm=debug,sqlx=warn,redis=warn"
log_format = "json"            # or "pretty" for dev
metrics_enabled = true
metrics_path = "/metrics"
panic_hook = true
```

Env-var equivalents:

```bash
PROPFIRM_OBSERVABILITY__LOG_FILTER=info,propfirm=debug
PROPFIRM_OBSERVABILITY__LOG_FORMAT=json
PROPFIRM_OBSERVABILITY__METRICS_ENABLED=true
PROPFIRM_OBSERVABILITY__PANIC_HOOK=true
```

## 1. Structured logging

### Format

- **`json`** (default, recommended for production): one JSON object per
  log line, with `timestamp`, `level`, `target`, `message`, and all
  structured fields. Easy to ingest into Loki / ELK / Datadog / CloudWatch.
- **`pretty`** (recommended for dev): colored, multi-line, human-readable.

### Log levels

| Level | When to use |
|---|---|
| `error` | Panic-caught handler errors; failures that affect the response |
| `warn` | Configuration issues; degraded mode (e.g. backend fell back to memory) |
| `info` | Server start/stop; auth events; one line per request (via TraceLayer) |
| `debug` | Per-handler decision logs; idempotency hits/misses |
| `trace` | Per-rule verdicts; raw SQL; Redis commands |

### Filter syntax

The `log_filter` is parsed by `EnvFilter` (RUST_LOG-style):

```toml
log_filter = "info,propfirm=debug,sqlx=warn,redis=warn"
```

This means:
- Global level = `info`
- The `propfirm` crate logs at `debug`
- The `sqlx` crate logs at `warn`
- The `redis` crate logs at `warn`

Override at runtime via `RUST_LOG`:

```bash
RUST_LOG=propfirm=trace,sqlx=warn docker compose up
```

### Span fields

The `tower-http` `TraceLayer` emits one span per HTTP request with
these fields:

| Field | Source |
|---|---|
| `method` | HTTP method |
| `uri` | Request URI |
| `request_id` | From `x-request-id` header (or generated UUID) |

Each span is automatically closed when the response is sent, with the
status code and duration logged. So one request produces one
`http` span + the handler's own log lines.

### JSON sample (production)

```json
{
  "timestamp":"2026-09-29T10:30:00.123Z",
  "level":"INFO",
  "target":"propfirm::api::auth",
  "message":"internal request authenticated",
  "service":"platform-bridge",
  "correlation_id":"abc-123",
  "tenant_id":"...",
  "path":"/internal/v1/evaluate"
}
```

(Pre-removal-of-auth; now the auth line is gone, but the per-request
span remains.)

### Pretty sample (dev)

```
2026-09-29T10:30:00.123Z INFO propfirm::api::server: server starting bind_addr=0.0.0.0:8080 tls_enabled=false
  2026-09-29T10:30:00.456Z INFO propfirm::api::server: received request method=POST uri=/internal/v1/evaluate request_id=abc-123
    2026-09-29T10:30:00.460Z DEBUG propfirm::api::handlers: evaluating account_id=... tenant_id=...
  2026-09-29T10:30:00.490Z INFO propfirm::api::server: response sent status=200 latency=34ms
```

## 2. Prometheus metrics

### Endpoint

`GET /metrics` (when `metrics_enabled = true`).

Returns a Prometheus-formatted scrape. Configure your
`prometheus.yml` scrape config:

```yaml
scrape_configs:
  - job_name: propfirm-engine
    metrics_path: /metrics
    static_configs:
      - targets: ['propfirm-server:8080']
```

### Available metrics

The exporter is installed via `PrometheusBuilder::install()` at binary
startup. The crate uses the `metrics` macros (`counter!`, `histogram!`,
`gauge!`) throughout to record into the global registry.

| Metric | Type | Labels | Source |
|---|---|---|---|
| `http_requests_total` | counter | method, status | `TraceLayer` (planned) |
| `http_request_duration_seconds` | histogram | method | `TraceLayer` (planned) |
| `evaluate_total` | counter | decision_kind | handlers |
| `evaluate_duration_seconds` | histogram | (none) | handlers |
| `idempotency_outcomes_total` | counter | outcome | handlers |
| `event_bus_messages_consumed_total` | counter | consumer_name | worker |
| `event_bus_messages_produced_total` | counter | stream | worker |
| `event_bus_processing_duration_seconds` | histogram | (none) | worker |
| `rule_evaluations_total` | counter | rule_id, verdict | registry |
| `rule_eval_duration_seconds` | histogram | rule_id | registry |

> **Note**: as of v0.2.0 only the PrometheusBuilder is installed. The
> per-handler `counter!` / `histogram!` calls are scaffolded but not
> wired through every code path. Add `metrics::counter!("evaluate_total",
> "decision_kind" => kind).increment(1)` in each handler as a follow-up.

### Sample scrape

```
# HELP http_requests_total Total number of HTTP requests received
# TYPE http_requests_total counter
http_requests_total{method="POST",status="200"} 1234
http_requests_total{method="POST",status="500"} 0
http_requests_total{method="GET",status="200"} 56
http_requests_total{method="GET",status="404"} 2

# HELP http_request_duration_seconds HTTP request latency
# TYPE http_request_duration_seconds histogram
http_request_duration_seconds_bucket{method="POST",le="0.005"} 1200
http_request_duration_seconds_bucket{method="POST",le="0.01"} 1230
http_request_duration_seconds_bucket{method="POST",le="0.025"} 1234
http_request_duration_seconds_bucket{method="POST",le="0.05"} 1234
http_request_duration_seconds_bucket{method="POST",le="0.1"} 1234
http_request_duration_seconds_bucket{method="POST",le="+Inf"} 1234
http_request_duration_seconds_sum{method="POST"} 1.234
http_request_duration_seconds_count{method="POST"} 1234
```

### Grafana dashboard

A basic Grafana dashboard JSON is at `docs/grafana-dashboard.json`
(planned). Panels:

1. Request rate by status code (time series)
2. p50/p95/p99 latency (time series)
3. Top rule violations by kind (bar chart)
4. Worker throughput (time series)
5. Idempotency hit rate (gauge)

## 3. Panic hook

Installed by `install_panic_hook()` at binary startup (when
`panic_hook = true`).

### What it does

When any code panics:

1. The hook captures the `PanicInfo` (message + location).
2. Logs it via `tracing::error!(panic = %info, location = ..., "panic caught by hook")`.
3. Calls the previous (default) hook so the stderr print still happens
   (useful in dev when no subscriber is configured).

### Why it matters

Without the hook:

- A panicking axum handler is caught by axum's framework-level
  `catch_unwind`, turned into a 500, and the panic message is lost.
- A panicking rule is caught by `RuleRegistry::evaluate`'s
  `catch_unwind`, downgraded to a `Warn` verdict, and the panic message
  is lost.

With the hook: both surface as `tracing::error` log lines with the
location, so you can find and fix them.

### Sample panic log

```json
{
  "timestamp":"2026-09-29T10:30:00.123Z",
  "level":"ERROR",
  "target":"propfirm::api::middleware",
  "message":"panic caught by hook",
  "panic":"called `Option::unwrap()` on a `None` value",
  "location":"src/api/handlers.rs:124:5"
}
```

## 4. Request correlation

Every HTTP request gets a `request_id`:

- If the client sends `X-Request-Id`, it's reused.
- Otherwise, a fresh UUID is generated via `SetRequestIdLayer`.

The `request_id` flows through:

- The `tracing` span (`http` span with `request_id` field)
- Any `#[tracing::instrument]`-annotated handler
- The response header `x-request-id`

So if you see a request_id in a log line, you can grep for all log
lines from that request across the entire pipeline.

## 5. Local verification

```bash
# 1. Bring up the stack
docker compose up -d

# 2. Make a few requests
for i in 1 2 3; do
  curl -fsS -X POST http://localhost:8080/internal/v1/evaluate \
    -H "Content-Type: application/json" \
    -H "X-Tenant-Id: $(uuidgen)" \
    -H "Idempotency-Key: $(uuidgen)" \
    -d @/tmp/eval.json
done

# 3. Check the logs
docker compose logs propfirm-server | tail -50

# 4. Check the metrics
curl -fsS http://localhost:8080/metrics | grep -E 'http_requests_total|http_request_duration_seconds'
```

## 6. What's NOT included (and why)

- **OpenTelemetry / OTLP exporter**: not wired because the engine is
  internal-only and the platform gateway manages trace context. If you
  ever expose the service publicly, add:

  ```toml
  # Cargo.toml
  tracing-opentelemetry = "0.27"
  opentelemetry-otlp = { version = "0.27", features = ["tonic"] }
  ```

  Then in `init_tracing`:

  ```rust
  let tracer = opentelemetry_otlp::new_pipeline()
      .tonic()
      .install_batch(opentelemetry_sdk::runtime::Tokio)?;
  let opentelemetry_layer = tracing_opentelemetry::layer().with_tracer(tracer);
  tracing_subscriber::registry()
      .with(opentelemetry_layer)
      .with(EnvFilter::from_default_env())
      .with(fmt::layer().json())
      .init();
  ```

- **Tracing for the Redis Streams worker**: the worker logs per-message
  spans, but doesn't propagate OpenTelemetry trace context yet. The
  request_id is the correlation key — the worker logs it on consume
  and on produce.

- **Structured audit log table**: the `audit_log` table exists in the
  Postgres schema but no code writes to it yet. The `Override` and
  `EmergencyStop` flows should write to it in a follow-up PR.
