# Configuration Reference

The propfirm-engine is configured via three layered sources, lowest
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
PROPFIRM_SERVER__BIND_ADDR=0.0.0.0:9090          # [server] bind_addr
PROPFIRM_SERVER__TLS__ENABLED=true              # [server.tls] enabled
PROPFIRM_POSTGRES__DSN=postgresql://user@host/db # [postgres] dsn
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
| `cert_path` | path | `/etc/propfirm/tls/cert.pem` | Path to PEM cert. |
| `key_path` | path | `/etc/propfirm/tls/key.pem` | Path to PEM key. |

See `docs/tls.md` for details.

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
| `cluster` | bool | `false` | Whether to use cluster mode. |
| `connect_timeout_secs` | int | `3` | Connect timeout. |
| `pool_size` | int | `8` | Pool size per worker. |

### `[observability]`

| Key | Type | Default | Description |
|---|---|---|---|
| `log_filter` | string | `"info,propfirm=debug"` | RUST_LOG-style filter. |
| `log_format` | string | `"json"` | `"json"` or `"pretty"`. |
| `metrics_enabled` | bool | `true` | Whether to expose `/metrics`. |
| `metrics_path` | string | `"/metrics"` | Path for the metrics endpoint. |
| `panic_hook` | bool | `true` | Install the panic hook. |

See `docs/observability.md` for details.

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

### Production (Redis idempotency + Postgres audit)

```toml
[server]
bind_addr = "0.0.0.0:8080"
request_timeout_secs = 15

[server.tls]
enabled = true
cert_path = "/etc/propfirm/tls/cert.pem"
key_path = "/etc/propfirm/tls/key.pem"

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
PROPFIRM_EVENT_BUS__CONSUMER_NAME=worker-1 cargo run --bin propfirm-worker
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
cluster, comma-separate multiple URLs.

### `PROPFIRM_POSTGRES__DSN=host=localhost port=5432` (key-value form)

`sqlx` expects a libpq-style DSN:
`postgresql://user:pass@host:port/db`. Key-value form is not supported.

### `PROPFIRM_IDEMPOTENCY__BACKEND=postgres` without `[postgres]` config

The server will fail to start with "failed to connect to Postgres"
because the `dsn` defaults to `localhost:5432` — likely unreachable
from your dev machine. Either:

- Configure `[postgres]` properly, or
- Use `backend = "memory"` for dev.
