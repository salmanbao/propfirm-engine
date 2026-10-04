# Configuration Reference (D81 Stateless Design)

**Important**: As of v0.2.0, the propfirm-engine follows the D81 stateless compute service design.
The engine is configured only for server binding, TLS, and observability. Persistence-related
configuration (Postgres, Redis, idempotency, event bus) is now the responsibility of the platform.

The propfirm-engine is configured via four layered sources, lowest precedence first:

1. **Inline defaults** — built into the binary (see `DEFAULT_CONFIG_TOML` in `src/settings.rs`).
2. **`config/propfirm.toml`** — shipped default config, present in the working directory. Override via `PROPFIRM_CONFIG=path/to/file.toml`.
3. **`.env` file** — auto-loaded by `dotenvy` if present in the working directory.
4. **`PROPFIRM_*` env vars** — highest precedence; override everything.

Nested keys in env vars use `__` (double underscore) separator:

```bash
PROPFIRM_SERVER__BIND_ADDR=0.0.0.0:9090           # [server] bind_addr
PROPFIRM_SERVER__TLS__ENABLED=true                # [server.tls] enabled
PROPFIRM_SERVER__TLS__CLIENT_CA_PATH=/etc/ca.pem  # [server.tls] client_ca_path (mTLS)
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

# Special
PROPFIRM_CONFIG=/path/to/custom.toml  # Override the bundled config file path
RUST_LOG=info,propfirm=debug           # Legacy (also honored by tracing)
```

## Examples

### Local dev (no TLS, plain logging)

```toml
[server]
bind_addr = "0.0.0.0:8080"

[observability]
log_format = "pretty"
```

```bash
cargo run --release --features server --bin propfirm-server
```

### Production (mTLS + OTLP)

```toml
[server]
bind_addr = "0.0.0.0:8080"
request_timeout_secs = 15

[server.tls]
enabled = true
cert_path = "/etc/propfirm/tls/cert.pem"
key_path = "/etc/propfirm/tls/key.pem"
client_ca_path = "/etc/propfirm/tls/ca.pem"   # mTLS

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

## Verifying config

```bash
# Load settings and dump them:
cargo run --bin propfirm-server -- --print-config
# (TODO: not yet wired — would dump Settings as TOML on startup)

# Or use a small test:
cargo test --features server -- settings::tests -- --nocapture
```

## Common config mistakes

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

## Note on Persistence Configuration

Persistence-related configuration (Postgres, Redis, idempotency, event bus)
has been removed from the engine. The platform is responsible for:
- Providing idempotency before calling the engine
- Storing and versioning account state
- Consuming and persisting DomainEvent emissions from the engine
- Building and maintaining the audit trail

The engine now focuses solely on pure evaluation and emits DomainEvent
objects for the platform to process.
