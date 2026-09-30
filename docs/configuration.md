# Configuration Reference

The propfirm-engine is configured via four layered sources, lowest
precedence first:

1. **Inline defaults** — built into the binary (see `DEFAULT_CONFIG_TOML`
   in `src/settings.rs`).
2. **`config/propfirm.toml`** — shipped default config, present in the
   working directory. Override via `PROPFIRM_CONFIG=path/to/file.toml`.
3. **`.env` file** — auto-loaded by `dotenvy` if present in the working
   directory.
4. **`PROPFIRM_*` env vars** — highest precedence; override everything.

Nested keys in env vars use `__` (double underscore) separator:

```bash
PROPFIRM_SERVER__BIND_ADDR=0.0.0.0:9090           # [server] bind_addr
PROPFIRM_SERVER__TLS__ENABLED=true                # [server.tls] enabled
PROPFIRM_SERVER__TLS__CLIENT_CA_PATH=/etc/ca.pem  # [server.tls] client_ca_path (mTLS)
PROPFIRM_POSTGRES__DSN=postgresql://user@host/db  # [postgres] dsn
PROPFIRM_OBSERVABILITY__OTLP__ENDPOINT=http://otel:4317  # [observability.otlp] endpoint
```

## Full settings reference

### `[server]`

| Key | Type | Default | Description |
|---|---|---|---|
| `bind_addr` | string | `"0.0.0.0:8080"` | Bind address:port. |
| `max_body_bytes` | int | `2097152` (2 MiB) | Max request body size. |
| `request_timeout_secs` | int | `30` | Per-request timeout (seconds). |
| `shutdown_timeout_secs` | int | `30` | Graceful shutdown drain timeout (seconds). |

### `[server.tls]`

| Key | Type | Default | Description |
|---|---|---|---|
| `enabled` | bool | `false` | Whether TLS is enabled. |
| `cert_path` | path | `/etc/propfirm/tls/cert.pem` | Path to PEM cert (leaf first, then intermediates). |
| `key_path` | path | `/etc/propfirm/tls/key.pem` | Path to PEM key (PKCS#8 or PKCS#1). |
| `client_ca_path` | `Option<PathBuf>` | `None` | **mTLS** — when set, the server builds a `rustls::server::ServerConfig` with `WebPkiClientVerifier` and rejects any client without a cert signed by this CA. When `None`, the server runs one-way TLS. |

See `docs/tls.md` for cert generation, mTLS setup, and deployment topologies.

### `[postgres]`

| Key | Type | Default | Description |
|---|---|---|---|
| `dsn` | string | `postgresql://propfirm:propfirm@localhost:5432/propfirm` | libpq DSN. |
| `max_connections` | int | `10` | Pool size. |
| `run_migrations` | bool | `true` | Whether to run pending migrations on startup. |
| `acquire_timeout_secs` | int | `5` | Connection acquire timeout. |

See `docs/persistence.md` for the schema.

### `[redis]`

| Key | Type | Default | Description |
|---|---|---|---|
| `url` | string | `redis://localhost:6379` | Redis URL. `rediss://` for TLS. Comma-separated for cluster. |
| `cluster` | bool | `false` | Whether to use cluster mode (uses `bb8::Pool<bb8_redis::RedisConnectionManager>`). |
| `connect_timeout_secs` | int | `3` | Connect timeout. |
| `pool_size` | int | `8` | Pool size (cluster mode) — number of concurrent multiplexed connections per worker. |

### `[observability]`

| Key | Type | Default | Description |
|---|---|---|---|
| `log_filter` | string | `"info,propfirm=debug"` | RUST_LOG-style filter. |
| `log_format` | string | `"json"` | `"json"` or `"pretty"`. |
| `metrics_enabled` | bool | `true` | Whether to expose `/metrics`. |
| `metrics_path` | string | `"/metrics"` | Path for the metrics endpoint. |
| `panic_hook` | bool | `true` | Install the panic hook that routes panics through `tracing::error`. |
| `flame_output_path` | string | `""` | When non-empty (and the `flame` cargo feature is enabled), installs a `tracing-flame` layer that writes a flame-graph-compatible trace to this path. Convert to SVG with `flamegraph <path> > flamegraph.svg`. |

See `docs/observability.md` for details.

### `[observability.otlp]`

OpenTelemetry OTLP exporter. Only used when the `otel` cargo feature
is enabled **AND** `otlp.endpoint` is non-empty (or `otlp.stdout = true`).

| Key | Type | Default | Description |
|---|---|---|---|
| `endpoint` | string | `""` | OTLP endpoint URL. Empty = disabled. `http://otel-collector:4317` (gRPC) or `http://otel-collector:4318` (HTTP). |
| `protocol` | string | `"grpc"` | `"grpc"` (recommended) or `"http"`. |
| `service_name` | string | `"propfirm-engine"` | Service name reported to the collector. |
| `stdout` | bool | `false` | When `true`, also export spans to stdout (useful for dev when no collector is available). |
| `sample_ratio` | float | `1.0` | Sample ratio (0.0–1.0). `1.0` = sample all spans. Lower for high-QPS production. |

### `[idempotency]`

| Key | Type | Default | Description |
|---|---|---|---|
| `backend` | string | `"memory"` | `"memory"`, `"postgres"`, or `"redis"`. |
| `ttl_secs` | int | `86400` (24h) | TTL for stored keys. `0` = no TTL. |
| `max_entries` | int | `10000` | Max entries (memory backend only). |

### `[event_bus]`

Used by the `propfirm-worker` binary.

| Key | Type | Default | Description |
|---|---|---|---|
| `request_stream` | string | `"propfirm:evaluate:requests"` | Redis Stream for inbound requests. |
| `response_stream` | string | `"propfirm:evaluate:responses"` | Redis Stream for outbound responses. |
| `consumer_group` | string | `"propfirm-worker"` | Redis consumer group name. |
| `consumer_name` | string | `""` | Consumer name (auto-UUID if empty). |
| `block_ms` | int | `5000` | XREADGROUP block timeout (ms). |
| `concurrency` | int | `16` | Concurrent consumer tasks per worker. |
| `idle_claim_ms` | int | `60000` | Idle threshold before XAUTOCLAIM. |

## Environment variables summary

```bash
# Server
PROPFIRM_SERVER__BIND_ADDR=0.0.0.0:8080
PROPFIRM_SERVER__TLS__ENABLED=false
PROPFIRM_SERVER__TLS__CERT_PATH=/etc/propfirm/tls/cert.pem
PROPFIRM_SERVER__TLS__KEY_PATH=/etc/propfirm/tls/key.pem
PROPFIRM_SERVER__TLS__CLIENT_CA_PATH=                # mTLS; leave unset to disable
PROPFIRM_SERVER__MAX_BODY_BYTES=2097152
PROPFIRM_SERVER__REQUEST_TIMEOUT_SECS=30
PROPFIRM_SERVER__SHUTDOWN_TIMEOUT_SECS=30

# Postgres
PROPFIRM_POSTGRES__DSN=postgresql://propfirm:propfirm@localhost:5432/propfirm
PROPFIRM_POSTGRES__MAX_CONNECTIONS=10
PROPFIRM_POSTGRES__RUN_MIGRATIONS=true
PROPFIRM_POSTGRES__ACQUIRE_TIMEOUT_SECS=5

# Redis
PROPFIRM_REDIS__URL=redis://localhost:6379
PROPFIRM_REDIS__CLUSTER=false
PROPFIRM_REDIS__CONNECT_TIMEOUT_SECS=3
PROPFIRM_REDIS__POOL_SIZE=8

# Observability
PROPFIRM_OBSERVABILITY__LOG_FILTER=info,propfirm=debug
PROPFIRM_OBSERVABILITY__LOG_FORMAT=json
PROPFIRM_OBSERVABILITY__METRICS_ENABLED=true
PROPFIRM_OBSERVABILITY__METRICS_PATH=/metrics
PROPFIRM_OBSERVABILITY__PANIC_HOOK=true
PROPFIRM_OBSERVABILITY__FLAME_OUTPUT_PATH=                # empty = disabled

# OTLP (requires `otel` cargo feature)
PROPFIRM_OBSERVABILITY__OTLP__ENDPOINT=http://otel-collector:4317
PROPFIRM_OBSERVABILITY__OTLP__PROTOCOL=grpc
PROPFIRM_OBSERVABILITY__OTLP__SERVICE_NAME=propfirm-engine
PROPFIRM_OBSERVABILITY__OTLP__STDOUT=false
PROPFIRM_OBSERVABILITY__OTLP__SAMPLE_RATIO=1.0

# Idempotency
PROPFIRM_IDEMPOTENCY__BACKEND=redis
PROPFIRM_IDEMPOTENCY__TTL_SECS=86400
PROPFIRM_IDEMPOTENCY__MAX_ENTRIES=10000

# Event bus
PROPFIRM_EVENT_BUS__REQUEST_STREAM=propfirm:evaluate:requests
PROPFIRM_EVENT_BUS__RESPONSE_STREAM=propfirm:evaluate:responses
PROPFIRM_EVENT_BUS__CONSUMER_GROUP=propfirm-worker
PROPFIRM_EVENT_BUS__CONSUMER_NAME=
PROPFIRM_EVENT_BUS__BLOCK_MS=5000
PROPFIRM_EVENT_BUS__CONCURRENCY=16
PROPFIRM_EVENT_BUS__IDLE_CLAIM_MS=60000

# Special
PROPFIRM_CONFIG=/path/to/custom.toml  # Override the bundled config file path
RUST_LOG=info,propfirm=debug           # Legacy (also honored by tracing)
```

## Examples

### Local dev (memory backend, no infra)

```toml
[server]
bind_addr = "0.0.0.0:8080"

[idempotency]
backend = "memory"

[observability]
log_format = "pretty"
```

```bash
cargo run --release --features server --bin propfirm-server
```

### Production (Redis idempotency + Postgres audit + mTLS + OTLP)

```toml
[server]
bind_addr = "0.0.0.0:8080"
request_timeout_secs = 15

[server.tls]
enabled = true
cert_path = "/etc/propfirm/tls/cert.pem"
key_path = "/etc/propfirm/tls/key.pem"
client_ca_path = "/etc/propfirm/tls/ca.pem"   # mTLS

[postgres]
dsn = "postgresql://propfirm:secret@postgres-cluster:5432/propfirm"
max_connections = 20

[redis]
url = "redis://redis-cluster:6379"
cluster = true
pool_size = 16

[idempotency]
backend = "redis"
ttl_secs = 604800  # 7 days

[observability]
log_format = "json"
log_filter = "info,propfirm=info,sqlx=warn,redis=warn"
metrics_enabled = true
panic_hook = true

[observability.otlp]
endpoint = "http://otel-collector.observability.svc:4317"
protocol = "grpc"
service_name = "propfirm-engine"
sample_ratio = 0.25
stdout = false
```

```bash
cargo run --release --features server,otel,openapi --bin propfirm-server
```

### Profiling (flame graph)

```toml
[observability]
flame_output_path = "/tmp/propfirm-flame.trace"
log_format = "pretty"
```

```bash
cargo run --release --features server,flame --bin propfirm-server
# ... drive traffic, then SIGTERM ...

flamegraph /tmp/propfirm-flame.trace > flamegraph.svg
```

### High-throughput worker

```toml
[event_bus]
request_stream = "propfirm:evaluate:requests"
response_stream = "propfirm:evaluate:responses"
consumer_group = "propfirm-worker"
concurrency = 32
block_ms = 1000
idle_claim_ms = 30000

[redis]
url = "redis://redis-cluster:6379"
cluster = true
pool_size = 32

[idempotency]
backend = "redis"
```

```bash
PROPFIRM_EVENT_BUS__CONSUMER_NAME=worker-1 cargo run --features server --bin propfirm-worker
```

## Verifying config

```bash
# Load settings and dump them:
cargo run --bin propfirm-server -- --print-config
# (TODO: not yet wired — would dump Settings as TOML on startup)

# Or use a small test:
cargo test --features server -- settings::tests -- --nocapture
```

## Common config mistakes

### `PROPFIRM_REDIS__CLUSTER=yes` (wrong type)

`cluster` is a bool, so use `true` / `false` (or `yes`/`no` work too,
but stick to `true`/`false` for clarity).

### `PROPFIRM_REDIS__URL=redis-cluster` (wrong protocol)

The URL must include the scheme: `redis://` or `rediss://` (TLS). For
cluster, comma-separate multiple URLs (the bb8 manager only needs one
seed URL — it discovers the rest via `CLUSTER NODES`).

### `PROPFIRM_POSTGRES__DSN=host=localhost port=5432` (key-value form)

`sqlx` expects a libpq-style DSN:
`postgresql://user:pass@host:port/db`. Key-value form is not supported.

### `PROPFIRM_IDEMPOTENCY__BACKEND=postgres` without `[postgres]` config

The server will fail to start with "failed to connect to Postgres"
because the `dsn` defaults to `localhost:5432` — likely unreachable
from your dev machine. Either:

- Configure `[postgres]` properly, or
- Use `backend = "memory"` for dev.

### `PROPFIRM_SERVER__TLS__CLIENT_CA_PATH` set but client cert missing

When `client_ca_path` is set, the server enforces mTLS — clients
without a cert signed by the configured CA get rejected at handshake
time. Verify with:

```bash
curl --cacert /etc/propfirm/tls/ca.pem \
  --cert /etc/propfirm/tls/client.pem \
  --key  /etc/propfirm/tls/client.key \
  https://localhost:8080/health
```

### `PROPFIRM_OBSERVABILITY__OTLP__ENDPOINT` set without the `otel` feature

The OTLP exporter is gated behind the `otel` cargo feature. Setting
the endpoint without enabling the feature is a silent no-op — the
binary just doesn't compile in the OTLP layer. Rebuild with
`--features server,otel`.

### `PROPFIRM_OBSERVABILITY__FLAME_OUTPUT_PATH` set without the `flame` feature

Same caveat — the `tracing-flame` layer is gated behind the `flame`
cargo feature. Rebuild with `--features server,flame`.
