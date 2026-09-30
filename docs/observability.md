# Observability

The propfirm-engine ships with **four pillars** of observability, all
wired and production-ready in v0.2.0:

1. **Structured logging** via `tracing` + `tracing-subscriber` (JSON or pretty to stdout)
2. **Prometheus metrics** via `metrics` + `metrics-exporter-prometheus` (`/metrics`)
3. **Panic hook** that routes panics through `tracing::error` instead of stderr
4. **OpenTelemetry OTLP exporter** (feature-gated via the `otel` cargo feature) —
   exports spans to Tempo / Jaeger / Honeycomb / Datadog / Lightstep / etc.

A fifth opt-in pillar — **flame-graph profiling** — is available behind
the `flame` cargo feature.

Audit-log writes (the `audit_log` Postgres table) are a sixth observability
surface for *who-did-what-when* on sensitive operations (override, emergency-stop,
manual-run, breach-report, evaluate-order, evaluate-internal non-Pass, worker
evaluate/error). See `src/api/audit_log.rs` (~301 LOC).

---

## 1. Cargo features

The observability surface is gated behind two cargo features so the
OpenTelemetry dependency tree (~50 crates) only lands in builds that
need it:

| Feature | What it pulls in | When to enable |
|---|---|---|
| `otel` | `tracing-opentelemetry`, `opentelemetry`, `opentelemetry-otlp`, `opentelemetry_sdk`, `opentelemetry-stdout` | When exporting spans to a collector (production). |
| `flame` | `tracing-flame` | When profiling (one-off perf investigations). |
| `openapi` | `utoipa`, `utoipa-swagger-ui` | When serving `/openapi.json` + `/swagger-ui/` (dev / API discovery). |
| `server` (already a base feature) | `tracing`, `tracing-subscriber`, `metrics`, `metrics-exporter-prometheus`, `prometheus` | Always-on when building the HTTP server or worker. |

```toml
# Cargo.toml — production build
[dependencies]
propfirm-engine = { features = ["server", "otel", "openapi"] }
```

```toml
# Cargo.toml — profiling build
[dependencies]
propfirm-engine = { features = ["server", "flame"] }
```

---

## 2. Configuration

All settings live under `[observability]` in `config/propfirm.toml`
(see `src/settings.rs`):

```toml
[observability]
log_filter  = "info,propfirm=debug,sqlx=warn,redis=warn"
log_format  = "json"            # or "pretty" for dev
metrics_enabled = true
metrics_path = "/metrics"
panic_hook  = true
flame_output_path = ""          # set to a path to enable flame profiling

[observability.otlp]
endpoint     = "http://otel-collector:4317"  # gRPC; 4318 for HTTP
protocol     = "grpc"                        # or "http"
service_name = "propfirm-engine"
stdout       = false                        # true → also print spans to stdout
sample_ratio = 1.0                          # 0.0–1.0; 1.0 = sample all spans
```

Env-var equivalents (double-underscore = nested key):

```bash
PROPFIRM_OBSERVABILITY__LOG_FILTER=info,propfirm=debug
PROPFIRM_OBSERVABILITY__LOG_FORMAT=json
PROPFIRM_OBSERVABILITY__METRICS_ENABLED=true
PROPFIRM_OBSERVABILITY__METRICS_PATH=/metrics
PROPFIRM_OBSERVABILITY__PANIC_HOOK=true
PROPFIRM_OBSERVABILITY__FLAME_OUTPUT_PATH=/tmp/propfirm-flame.trace
PROPFIRM_OBSERVABILITY__OTLP__ENDPOINT=http://otel-collector:4317
PROPFIRM_OBSERVABILITY__OTLP__PROTOCOL=grpc
PROPFIRM_OBSERVABILITY__OTLP__SERVICE_NAME=propfirm-engine
PROPFIRM_OBSERVABILITY__OTLP__STDOUT=false
PROPFIRM_OBSERVABILITY__OTLP__SAMPLE_RATIO=1.0
```

---

## 3. Structured logging

### Format

- **`json`** (default, recommended for production): one JSON object per log
  line, with `timestamp`, `level`, `target`, `message`, and all structured
  fields. Easy to ingest into Loki / ELK / Datadog / CloudWatch.
- **`pretty`** (recommended for dev): colored, multi-line, human-readable.

### Log levels

| Level | When to use |
|---|---|
| `error` | Panic-caught handler errors; failures that affect the response |
| `warn` | Configuration issues; degraded mode (e.g. backend fell back to memory); idempotency conflict |
| `info` | Server start/stop; one line per request (via TraceLayer); audit-log write failures |
| `debug` | Per-handler decision logs; idempotency hits/misses; worker consume/produce/ack |
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
`http` span + the handler's own log lines. Every HTTP handler and worker
function is also annotated with `#[tracing::instrument]`, so deeper
spans (per-rule, per-message) appear nested under the request span.

### JSON sample (production)

```json
{
  "timestamp":"2026-09-30T10:30:00.123Z",
  "level":"INFO",
  "target":"propfirm::api::server",
  "message":"server starting",
  "bind_addr":"0.0.0.0:8080",
  "tls_enabled":false,
  "idempotency_backend":"redis",
  "metrics_enabled":true,
  "otlp_enabled":true
}
```

### Pretty sample (dev)

```
2026-09-30T10:30:00.123Z INFO propfirm::api::server: server starting
    bind_addr=0.0.0.0:8080 tls_enabled=false
  2026-09-30T10:30:00.456Z INFO propfirm::api::handlers: received request
    method=POST uri=/internal/v1/evaluate request_id=abc-123
    2026-09-30T10:30:00.460Z DEBUG propfirm::api::handlers: evaluating
      account_id=... tenant_id=...
  2026-09-30T10:30:00.490Z INFO propfirm::api::server: response sent
    status=200 latency=34ms
```

---

## 4. Prometheus metrics

### Endpoint

`GET /metrics` (when `metrics_enabled = true`). The exporter is installed
via `PrometheusBuilder::install_recorder()` at binary startup (see
`src/api/server.rs::default_metrics_handle()` — idempotent via `OnceLock`).

Configure your `prometheus.yml` scrape config:

```yaml
scrape_configs:
  - job_name: propfirm-engine
    metrics_path: /metrics
    static_configs:
      - targets: ['propfirm-server:8080']
```

### Available metrics — HTTP layer

All HTTP-layer metrics are emitted from `src/api/metrics.rs` (175 LOC) and
called from every handler in `src/api/handlers.rs`.

| Metric | Type | Labels | Source |
|---|---|---|---|
| `propfirm_http_requests_total` | counter | `method`, `status` | `record_request` |
| `propfirm_evaluate_decisions_total` | counter | `kind` | `record_decision` (called on every `/internal/v1/evaluate` response) |
| `propfirm_idempotency_outcomes_total` | counter | `outcome` (`fresh` / `replay` / `conflict` / `error`) | `record_idempotency_outcome` |
| `propfirm_errors_total` | counter | `endpoint`, `kind` | `record_error` (4xx/5xx paths) |
| `propfirm_request_duration_seconds` | histogram | `endpoint` | `LatencyScope` (RAII timer started at handler entry) |

The `LatencyScope` is a `Drop`-based RAII timer — every handler opens one
at entry with `LatencyScope::start("/internal/v1/evaluate")` and the
histogram records the elapsed seconds when the guard goes out of scope.

### Available metrics — worker (event bus)

All worker metrics live in `src/api/metrics.rs::worker` and are called
from `src/bin/worker.rs`:

| Metric | Type | Labels | Source |
|---|---|---|---|
| `propfirm_event_bus_messages_consumed_total` | counter | `consumer` | `record_message_consumed` (after `XREADGROUP`) |
| `propfirm_event_bus_messages_produced_total` | counter | (none) | `record_message_produced` (after `XADD` response) |
| `propfirm_event_bus_messages_acked_total` | counter | (none) | `record_message_acked` (after `XACK`) |
| `propfirm_event_bus_messages_claimed_total` | counter | (none) | `record_message_claimed` (after `XAUTOCLAIM`) |
| `propfirm_event_bus_errors_total` | counter | `kind` | `record_error` (decode/redis/panic) |
| `propfirm_request_duration_seconds` | histogram | `endpoint=event_bus_worker` | `worker::latency_scope()` |

### Sample scrape

```
# HELP propfirm_http_requests_total Total HTTP requests received
# TYPE propfirm_http_requests_total counter
propfirm_http_requests_total{method="POST",status="200"} 1234
propfirm_http_requests_total{method="POST",status="409"} 3
propfirm_http_requests_total{method="POST",status="500"} 0
propfirm_http_requests_total{method="GET",status="200"} 56

# HELP propfirm_evaluate_decisions_total Evaluate decisions by kind
# TYPE propfirm_evaluate_decisions_total counter
propfirm_evaluate_decisions_total{kind="Pass"} 1100
propfirm_evaluate_decisions_total{kind="Warn"} 80
propfirm_evaluate_decisions_total{kind="Fail"} 35
propfirm_evaluate_decisions_total{kind="Liquidate"} 12
propfirm_evaluate_decisions_total{kind="Emergency"} 7

# HELP propfirm_idempotency_outcomes_total Idempotency outcomes
# TYPE propfirm_idempotency_outcomes_total counter
propfirm_idempotency_outcomes_total{outcome="fresh"} 1200
propfirm_idempotency_outcomes_total{outcome="replay"} 34
propfirm_idempotency_outcomes_total{outcome="conflict"} 3
propfirm_idempotency_outcomes_total{outcome="error"} 0

# HELP propfirm_request_duration_seconds HTTP request latency
# TYPE propfirm_request_duration_seconds histogram
propfirm_request_duration_seconds_bucket{endpoint="/internal/v1/evaluate",le="0.005"} 1200
propfirm_request_duration_seconds_bucket{endpoint="/internal/v1/evaluate",le="0.01"} 1230
propfirm_request_duration_seconds_bucket{endpoint="/internal/v1/evaluate",le="0.025"} 1234
propfirm_request_duration_seconds_bucket{endpoint="/internal/v1/evaluate",le="+Inf"} 1234
propfirm_request_duration_seconds_sum{endpoint="/internal/v1/evaluate"} 1.234
propfirm_request_duration_seconds_count{endpoint="/internal/v1/evaluate"} 1234

# HELP propfirm_event_bus_messages_consumed_total Messages consumed from the event bus
# TYPE propfirm_event_bus_messages_consumed_total counter
propfirm_event_bus_messages_consumed_total{consumer="worker-0"} 4521

# HELP propfirm_event_bus_messages_produced_total Responses produced to the event bus
# TYPE propfirm_event_bus_messages_produced_total counter
propfirm_event_bus_messages_produced_total 4520

# HELP propfirm_event_bus_messages_acked_total Messages acknowledged (XACK)
# TYPE propfirm_event_bus_messages_acked_total counter
propfirm_event_bus_messages_acked_total 4520

# HELP propfirm_event_bus_messages_claimed_total Messages auto-claimed from the PEL
# TYPE propfirm_event_bus_messages_claimed_total counter
propfirm_event_bus_messages_claimed_total 3

# HELP propfirm_event_bus_errors_total Worker errors
# TYPE propfirm_event_bus_errors_total counter
propfirm_event_bus_errors_total{kind="decode"} 0
propfirm_event_bus_errors_total{kind="redis"} 0
propfirm_event_bus_errors_total{kind="panic"} 0
```

### Grafana dashboard

A prebuilt Grafana dashboard JSON ships in the Helm chart at
**`deploy/helm/dashboards/propfirm-overview.json`** and is auto-imported
by the chart's `grafana-dashboard.yaml` template (a `ConfigMap` consumed
by the Grafana Helm chart's dashboard-provisioning sidecar).

Panels:

1. Request rate by status code (time series — `propfirm_http_requests_total`)
2. p50/p95/p99 latency (time series — `propfirm_request_duration_seconds`)
3. Decision distribution (bar chart — `propfirm_evaluate_decisions_total`)
4. Worker throughput: consumed vs acked (time series)
5. PEL backlog gauge (`consumed - acked`)
6. Idempotency outcome distribution (gauge — `propfirm_idempotency_outcomes_total`)
7. Error rate by endpoint + kind (time series — `propfirm_errors_total`)
8. Worker error rate (time series — `propfirm_event_bus_errors_total`)

### Grafana alerting rules

Prebuilt alerting rules live at
**`deploy/helm/alertrules/propfirm-engine.yaml`** and are provisioned via
the `grafana-alerting.yaml` Helm template. There are 9 alerting rules
across 4 groups:

| Group | Alert | Severity | Trigger |
|---|---|---|---|
| `propfirm-engine.api` | `PropfirmHighErrorRate` | critical | 5xx rate > 0.01/s for 2m |
| `propfirm-engine.api` | `PropfirmHighLatencyP99` | critical | p99 latency > 200ms for 5m |
| `propfirm-engine.api` | `PropfirmIdempotencyConflicts` | warning | any `outcome=conflict` for 5m |
| `propfirm-engine.api` | `PropfirmIdempotencyBackendErrors` | warning | any `outcome=error` for 2m |
| `propfirm-engine.decisions` | `PropfirmLowPassRate` | info | Pass rate < 80% for 15m |
| `propfirm-engine.decisions` | `PropfirmEmergencyStopSpike` | critical | `Emergency` decisions > 1/s for 1m |
| `propfirm-engine.decisions` | `PropfirmLiquidateRate` | warning | `Liquidate` decisions > 5/min sustained for 5m |
| `propfirm-engine.worker` | `PropfirmWorkerNotConsuming` | critical | `consumed_total` == 0 for 5m |
| `propfirm-engine.worker` | `PropfirmWorkerErrorRate` | critical | `event_bus_errors_total` rate > 1/s for 2m |

Every alert has `service=propfirm-engine` and `component=server`/`worker`
labels for routing by alertmanager. Annotations include the runbook URL
and the exact `kubectl` / SQL commands to investigate.

---

## 5. OpenTelemetry OTLP exporter

When the `otel` cargo feature is enabled **and** `observability.otlp.endpoint`
is non-empty, `init_tracing()` (in `src/api/otel.rs`) installs an OTLP layer
alongside the JSON/pretty fmt layer. Spans are then exported to the configured
collector (Tempo / Jaeger / Honeycomb / Datadog / Lightstep).

### Protocols

| `protocol` | Endpoint port | Transport | Notes |
|---|---|---|---|
| `grpc` (default) | `4317` | HTTP/2 over TCP | Recommended. Tonic-based. |
| `http` | `4318` | HTTP/1.1 + Protobuf body | Use when port 4317 is firewalled. |

### Resource

The OTLP resource carries `service.name = $OTLP__SERVICE_NAME`
(default `propfirm-engine`). Add more attributes by extending
`src/api/otel.rs::init_tracing`.

### Sampling

`sample_ratio` (0.0–1.0) controls the SDK sampler. `1.0` = sample all
spans (good for dev / low traffic). For high-QPS production, set
`0.1` or lower to keep collector volume manageable.

### stdout mode

Setting `observability.otlp.stdout = true` swaps the OTLP exporter for
`opentelemetry_stdout::SpanExporter` — useful for dev when no collector
is available. Spans print as JSON to stdout alongside the fmt layer.

### Shutdown flush

`otel::shutdown_otlp()` is called from `propfirm-server` and
`propfirm-worker` on shutdown so spans in flight are flushed before the
process exits. Best-effort; errors are logged but not returned.

### Local verification

```bash
# Bring up a local OTLP collector
docker run --rm -p 4317:4317 -p 4318:4318 \
  otel/opentelemetry-collector:latest

# Run the engine with OTLP enabled
PROPFIRM_OBSERVABILITY__OTLP__ENDPOINT=http://localhost:4317 \
cargo run --features server,otel --bin propfirm-server

# Make a few requests, then inspect spans in the collector logs
```

The OTLP end-to-end test (`tests/otlp_e2e.rs`, `#[ignore]` by default —
requires a live collector) asserts the collector receives spans with the
expected service name and per-handler span names.

---

## 6. Flame-graph profiling

When the `flame` cargo feature is enabled **and**
`observability.flame_output_path` is non-empty, `init_tracing()` installs
a `tracing-flame::FlameLayer` instead of the OTLP layer. The layer writes
a flame-graph-compatible text trace to the configured path.

```toml
[observability]
flame_output_path = "/tmp/propfirm-flame.trace"
```

```bash
cargo run --features server,flame --bin propfirm-server
# ... drive traffic, then SIGTERM ...

# Convert to SVG:
flamegraph /tmp/propfirm-flame.trace > flamegraph.svg

# Or use the `inferno` equivalent:
inferno-flamegraph < /tmp/propfirm-flame.trace > flamegraph.svg
```

The `FLAME_GUARD` `OnceLock<FlushGuard<...>>` keeps the writer alive for
the lifetime of the process so the trace is flushed on Drop.

---

## 7. Panic hook

Installed by `install_panic_hook()` (in `src/api/middleware.rs`) at binary
startup when `panic_hook = true` (the default).

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
- A panicking worker task kills the task but the message is lost.

With the hook: all three surface as `tracing::error` log lines with the
location, so you can find and fix them. The worker also writes an audit_log
entry with `action="worker_error"` and `error_kind="panic"` on a panic.

### Sample panic log

```json
{
  "timestamp":"2026-09-30T10:30:00.123Z",
  "level":"ERROR",
  "target":"propfirm::api::middleware",
  "message":"panic caught by hook",
  "panic":"called `Option::unwrap()` on a `None` value",
  "location":"src/api/handlers.rs:124:5"
}
```

---

## 8. Audit log

`src/api/audit_log.rs` (301 LOC) writes to the `audit_log` Postgres table
from every sensitive code path. Writes are best-effort: if `pg_pool` is
`None` (memory-only dev mode), the write is silently skipped — the
operation still succeeds.

### Writes emitted

| Action | Source handler | Condition |
|---|---|---|
| `evaluate` | `evaluate_internal` | decision_kind != `Pass` (avoid drowning the table in normal traffic) |
| `evaluate_order` | `evaluate_order` | every call |
| `override_breach` | `override_breach` | every successful override |
| `emergency_stop` | `emergency_stop` | every successful emergency stop |
| `manual_run` | `manual_run` | every successful manual run |
| `breach_report` | `breach_report` | every query (read but audited) |
| `worker_evaluate` | `propfirm-worker` | every consumed message with a verdict |
| `worker_error` | `propfirm-worker` | decode failure / redis error / panic |

### Schema

```sql
CREATE TABLE audit_log (
    id              BIGSERIAL PRIMARY KEY,
    occurred_at     TIMESTAMPTZ NOT NULL DEFAULT now(),
    correlation_id  UUID,
    actor_id        TEXT,                -- "evaluate_internal" / actor / consumer name
    action          TEXT NOT NULL,       -- "evaluate" / "override_breach" / ...
    tenant_id       UUID,
    account_id      UUID,
    resource_kind   TEXT,                -- "violation" / "order" / "request" / ...
    resource_id     TEXT,
    request_hash    TEXT,                -- sha256 of the request body
    response_status INTEGER,
    latency_ms      INTEGER,
    metadata        JSONB                -- free-form context (reason, decision_kind, ...)
);
```

### Query patterns

```sql
-- Who overrode breach X for account Y?
SELECT * FROM audit_log
  WHERE action = 'override_breach'
    AND account_id = '...'
  ORDER BY occurred_at DESC;

-- Every EmergencyStop decision in the last hour, with reasons:
SELECT occurred_at, actor_id, metadata->>'reason' AS reason
  FROM audit_log
  WHERE action = 'emergency_stop'
    AND occurred_at > now() - interval '1 hour'
  ORDER BY occurred_at DESC;

-- Worker error rate (correlates with the propfirm_event_bus_errors_total metric):
SELECT occurred_at, actor_id, metadata
  FROM audit_log
  WHERE action = 'worker_error'
  ORDER BY occurred_at DESC
  LIMIT 50;
```

---

## 9. Request correlation

Every HTTP request gets a `request_id`:

- If the client sends `X-Request-Id`, it's reused.
- Otherwise, a fresh UUID is generated via `SetRequestIdLayer`.

The `request_id` flows through:

- The `tracing` span (`http` span with `request_id` field)
- Any `#[tracing::instrument]`-annotated handler
- The response header `x-request-id`
- The OTLP span (when `otel` is enabled)
- The worker's `worker_evaluate` / `worker_error` audit entries (via
  `request_id` field on the stream payload)

So if you see a request_id in a log line, you can grep for all log lines
and audit_log rows from that request across the entire pipeline.

---

## 10. Local verification

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

# 4. Check the metrics (verify the propfirm_ prefix on every metric name)
curl -fsS http://localhost:8080/metrics | grep -E 'propfirm_http_requests_total|propfirm_evaluate_decisions_total|propfirm_idempotency_outcomes_total|propfirm_request_duration_seconds'

# 5. Check the audit_log table (non-Pass evaluations get written here)
docker compose exec postgres psql -U propfirm -d propfirm -c \
  "SELECT occurred_at, action, account_id, metadata FROM audit_log ORDER BY id DESC LIMIT 10;"
```
