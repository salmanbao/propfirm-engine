# Persistence

The propfirm-engine ships with four pluggable persistence layers:

| Layer | Backends | Use case | Survives restart | Survives crash | Shared across replicas |
|---|---|---|---|---|---|
| Event store | `InMemoryEventStore`, `PostgresEventStore` | Append-only domain-event log; `replay(account_id)` rebuilds account state | ✅ (postgres) | ✅ (postgres) | ✅ (postgres) |
| Idempotency | `IdempotencyStore` (memory), `PostgresIdempotencyBackend`, `RedisIdempotencyBackend` | Dedupes mutating HTTP retries | depends on backend | depends on backend | depends on backend |
| Audit log | Postgres `audit_log` table | Who-did-what-when for sensitive operations | ✅ | ✅ | ✅ |
| Event bus | `RedisEventBus` | Async eval-request delivery to the worker | ✅ (Redis Streams) | ✅ (PEL + XAUTOCLAIM) | ✅ |

Select idempotency backend via `PROPFIRM_IDEMPOTENCY__BACKEND` env var
(or `[idempotency] backend = "..."` in TOML). The event store and
audit_log are auto-selected based on the configured idempotency backend
(Postgres is provisioned whenever `backend` is `postgres` or `redis`).

| `backend` | Event store | Idempotency backend | Audit log | Event bus |
|---|---|---|---|---|
| `memory` (default) | `InMemoryEventStore` | `IdempotencyStore` (in-process HashMap) | not written (`pg_pool = None`) | `RedisEventBus` (if `redis.url` reachable) |
| `postgres` | `PostgresEventStore` | `PostgresIdempotencyBackend` | ✅ | `RedisEventBus` (if `redis.url` reachable) |
| `redis` | `PostgresEventStore` (preferred) or `InMemoryEventStore` (fallback) | `RedisIdempotencyBackend` | ✅ (if pg pool present) | `RedisEventBus` |

## Architecture

```
┌──────────────────────────────────────────────────────────┐
│                      HTTP server                         │
│  ┌───────────────────┐         ┌──────────────────────┐ │
│  │  /internal/v1/    │         │     /metrics         │ │
│  │   evaluate,       │         └──────────────────────┘ │
│  │   override,       │                                  │
│  │   manual-run,     │         ┌──────────────────────┐ │
│  │   emergency-stop, │         │  /openapi.json       │ │
│  │   breach-report  │         │  /swagger-ui/        │ │
│  └─────────┬────────┘         └──────────────────────┘ │
│            │                                            │
│            ▼                                            │
│  ┌──────────────────────────────────────────────┐       │
│  │ ServerState {                                │       │
│  │   idempotency: Arc<dyn IdempotencyBackend>,  │       │
│  │   event_store: Arc<dyn EventStore>,          │       │
│  │   pg_pool: Option<Arc<sqlx::PgPool>>,        │       │
│  │   metrics_handle: PrometheusHandle,          │       │
│  │ }                                            │       │
│  └─────────┬─────────────────────┬────────────┘        │
└────────────┼─────────────────────┼────────────────────┘
             │                     │
             ▼                     ▼
   ┌──────────────────┐  ┌──────────────────┐
   │ Idempotency      │  │ audit_log table  │
   │ (memory/postgres │  │ (Postgres only;  │
   │  /redis)         │  │  no-op if no     │
   │                  │  │  pg_pool)        │
   └──────────────────┘  └──────────────────┘
                                ▲
   ┌──────────────────┐         │
   │ Event store      │         │ audit_log::AuditEntry::finish()
   │ (InMemory or     │         │ called from every sensitive handler
   │  Postgres)       │         │
   └──────────────────┘

┌──────────────────────────────────────────────────────────┐
│              propfirm-worker binary                      │
│  ┌──────────────────────────────────────────────┐         │
│  │ N concurrent consumer tasks                 │         │
│  │   XREADGROUP → pure::evaluate → XADD         │         │
│  │   response + XACK request                     │         │
│  │   + audit_log::worker_evaluate/_error write  │         │
│  └──────────────┬───────────────────────────────┘         │
│                 │                                         │
│                 ▼                                         │
│        ┌──────────────────┐                               │
│        │ RedisEventBus    │                               │
│        │ (Redis Streams,  │                               │
│        │  consumer group, │                               │
│        │  bb8 pool for    │                               │
│        │  cluster mode)   │                               │
│        └──────────────────┘                               │
└──────────────────────────────────────────────────────────┘
```

## 1. PostgreSQL (recommended for durable audit + event store)

### Schema

Migration: `src/persistence/migrations/0001_init.sql` (run
automatically on startup when `postgres.run_migrations = true`).

#### `events`

```sql
CREATE TABLE events (
    id              UUID PRIMARY KEY,
    account_id      UUID NOT NULL,
    tenant_id       UUID NOT NULL,
    kind            TEXT NOT NULL,
    payload         JSONB NOT NULL,
    occurred_at     TIMESTAMPTZ NOT NULL,
    causation_id    UUID,
    inserted_at    TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX events_account_idx  ON events (account_id, occurred_at);
CREATE INDEX events_tenant_idx   ON events (tenant_id, occurred_at);
CREATE INDEX events_kind_idx     ON events (kind);
CREATE INDEX events_causation_idx ON events (causation_id);
```

Used by `PostgresEventStore` for the durable domain-event audit log.
The `replay(account_id)` method reconstructs `Account` state from the
event log — the dispute-resolution seam.

#### `idempotency`

```sql
CREATE TABLE idempotency (
    composite_key   TEXT PRIMARY KEY,         -- "{tenant}\0{endpoint}\0{key}"
    tenant_id        UUID NOT NULL,
    endpoint         TEXT NOT NULL,
    idempotency_key  TEXT NOT NULL,
    body_hash        TEXT NOT NULL,            -- sha256 hex of request body
    response         TEXT NOT NULL,            -- serialized first response
    inserted_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    expires_at       TIMESTAMPTZ NOT NULL
);
CREATE INDEX idempotency_tenant_idx   ON idempotency (tenant_id);
CREATE INDEX idempotency_expires_idx  ON idempotency (expires_at);
```

Used by `PostgresIdempotencyBackend`. Atomic check-and-remember via
`INSERT ... ON CONFLICT DO NOTHING`, then a `SELECT` to determine
Replay vs Conflict.

#### `rule_packs`

For versioned rule packs (Draft → Active → Superseded). Lifecycle
preserved for replay. See `src/rulepack.rs`.

#### `audit_log`

For who-did-what-when audit trail (overrides, emergency stops,
manual-runs, breach-report queries, evaluate-order, evaluate-internal
non-Pass, worker evaluate / worker_error).

```sql
CREATE TABLE audit_log (
    id              BIGSERIAL PRIMARY KEY,
    occurred_at     TIMESTAMPTZ NOT NULL DEFAULT now(),
    correlation_id  UUID,
    actor_id        TEXT,                    -- "evaluate_internal" / actor / consumer name
    action          TEXT NOT NULL,            -- "evaluate" / "override_breach" / ...
    tenant_id       UUID,
    account_id      UUID,
    resource_kind   TEXT,                    -- "violation" / "order" / "request" / ...
    resource_id     TEXT,
    request_hash    TEXT,                    -- sha256 of request body if available
    response_status INTEGER,
    latency_ms      INTEGER,
    metadata        JSONB                    -- free-form context
);
CREATE INDEX audit_log_tenant_idx   ON audit_log (tenant_id, occurred_at DESC);
CREATE INDEX audit_log_account_idx ON audit_log (account_id, occurred_at DESC);
CREATE INDEX audit_log_action_idx  ON audit_log (action, occurred_at DESC);
```

The write-path is **wired** in v0.2.0 (see `src/api/audit_log.rs`,
301 LOC). Every sensitive handler constructs an `AuditEntry` via the
convenience constructors (`override_breach`, `emergency_stop`,
`manual_run`, `breach_report`, `evaluate`, `evaluate_order`,
`worker_evaluate`, `worker_error`) and calls `.finish(pg_pool,
correlation_id, response_status)`. If `pg_pool` is `None` (memory-only
dev mode), the write is a silent no-op — the operation still
succeeds, just without an audit record.

The `audit_log.action` column receives one of:

- `evaluate` — non-Pass `/internal/v1/evaluate` verdicts
- `evaluate_order` — every `/v1/evaluate-order` call
- `override_breach` — successful `/internal/v1/override`
- `emergency_stop` — successful `/internal/v1/emergency-stop`
- `manual_run` — successful `/internal/v1/manual-run`
- `breach_report` — every `/internal/v1/breach-report` query (read but audited)
- `worker_evaluate` — every consumed event-bus message with a verdict
- `worker_error` — decode / redis / panic failures in the worker loop

### Connection pooling

`sqlx::PgPool` with `max_connections` from settings (default 10).
Pool is wrapped in `Arc` and shared across all handlers + worker
tasks via `ServerState.pg_pool`.

```rust
let pool = PgPoolOptions::new()
    .max_connections(settings.postgres.max_connections)
    .acquire_timeout(Duration::from_secs(settings.postgres.acquire_timeout_secs))
    .connect(&settings.postgres.dsn)
    .await?;
```

### Migrations

Run automatically on startup when `postgres.run_migrations = true`
(default). Disable for manual ops:

```bash
PROPFIRM_POSTGRES__RUN_MIGRATIONS=false docker compose up
```

Manual run:

```bash
docker compose exec propfirm-server sh -c 'apt-get install -y sqlx-cli && sqlx migrate run --database-url $PROPFIRM_POSTGRES__DSN'
```

Or from source:

```bash
cargo install sqlx-cli --no-default-features --features postgres,rustls
sqlx migrate run --source src/persistence/migrations \
  --database-url postgresql://propfirm:propfirm@localhost:5432/propfirm
```

### Replay semantics

`PostgresEventStore::replay(account_id)` walks events in
`occurred_at ASC, inserted_at ASC` order and rebuilds the `Account`:

1. The first event must be `AccountStarted { plan }` — the seed.
2. Each subsequent event applies its delta (status change, trade fill,
   rollover, etc.).

This is identical to `InMemoryEventStore::replay` semantics —
backends are interchangeable for the replay use case.

## 2. Redis (recommended for idempotency + event bus)

### Connection model

- **Single-node**: `redis::aio::MultiplexedConnection` (cloneable,
  async-safe, handles pipelining internally). One underlying connection
  multiplexed across all clones.
- **Cluster**: `bb8::Pool<bb8_redis::RedisConnectionManager>`. The
  pool maintains N concurrent connections (configurable via
  `Settings::redis.pool_size`, default 8), so cluster ops run in
  parallel without serializing through a Mutex.

This is a deliberate upgrade from the previous
`tokio::sync::Mutex<ClusterConnection>` model, which serialized all
cluster ops through a single connection — only one in-flight command
per worker process. With bb8, the pool hands out a fresh (multiplexed)
connection per `get()` call, so concurrent tasks can issue Redis
commands in parallel. For a 16-concurrency worker, this is roughly a
16x throughput improvement on the Redis side.

The `RedisConn` wrapper enum (`persistence::redis_store::mod`) hides
the single-vs-cluster split behind one type:

```rust
pub enum RedisConn {
    Single(redis::aio::MultiplexedConnection),
    Cluster(Pool<RedisConnectionManager>),
}
```

### Idempotency backend

`RedisIdempotencyBackend` uses an atomic Lua script for
check-and-remember:

```lua
if redis.call('EXISTS', KEYS[1]) == 1 then
    local stored = redis.call('GET', KEYS[1])
    if stored == ARGV[1] then
        return {'replay', redis.call('GET', KEYS[2]) or ''}
    else
        return {'conflict', ''}
    end
end
redis.call('SET', KEYS[1], ARGV[1], 'EX', ARGV[3], 'NX')
redis.call('SET', KEYS[2], ARGV[2], 'EX', ARGV[3])
return {'fresh', ''}
```

Key layout:

- `propfirm:idem:{tenant}:{endpoint_hash}:{key}` (body hash)
- `propfirm:idem:{tenant}:{endpoint_hash}:{key}:val` (response)

TTL: from `idempotency.ttl_secs` (default 24h).

### Event bus

`RedisEventBus` uses Redis Streams for at-least-once delivery:

- **Request stream**: `propfirm:evaluate:requests` (configurable via `event_bus.request_stream`)
- **Response stream**: `propfirm:evaluate:responses` (configurable via `event_bus.response_stream`)
- **Consumer group**: `propfirm-worker` (configurable via `event_bus.consumer_group`)

Wire format (XADD fields):

- `request_id` — UUID (correlation key)
- `payload` — JSON-serialized request/response

See `docs/event-bus.md` for the full consumer lifecycle, recovery
semantics, and the bb8 pool's role in cluster mode.

### Cluster mode

Set `redis.cluster = true` and provide a comma-separated URL list:

```toml
[redis]
url = "redis://node-1:6379,redis://node-2:6379,redis://node-3:6379"
cluster = true
pool_size = 16
```

The `bb8_redis::RedisConnectionManager` handles slot routing
internally — it only needs one seed URL to discover the rest of the
cluster via `CLUSTER NODES` / `CLUSTER SLOTS`. All cluster ops run
through the bb8 pool, so concurrent worker tasks issue commands in
parallel without serialization.

For higher throughput, raise `redis.pool_size` (more concurrent
connections per worker) and / or run more worker replicas.

## 3. In-memory (dev only)

`IdempotencyStore` and `InMemoryEventStore` are the defaults when
`backend = "memory"`. Useful for:

- Unit tests (no I/O)
- Local dev where you don't want to bring up Postgres/Redis
- CI smoke tests

**Never use in production** — all state is lost on every deploy,
causing double-applied mutations on client retries, and there's no
audit_log record of any sensitive action.

## Backend selection matrix

| Setup | Use when |
|---|---|
| `backend = memory` | Local dev / CI smoke |
| `backend = postgres` | Durable idempotency + audit log; HTTP-only (no event bus worker) |
| `backend = redis` | Durable idempotency + event bus; recommended for HA deployments |
| Both `postgres` + `redis` | `backend = redis` (idempotency) + Postgres still used for event store + audit log |

The default `docker-compose.yml` ships with `backend = redis` and
Postgres still wired for the event store + audit log.

## Throughput recommendation

For high-throughput workers (≥ 100k RPS):

- Use `redis.backend = redis` (Lua-script idempotency, no Postgres round-trip on every request).
- Use cluster-mode Redis with `pool_size >= concurrency` (16 or 32 — one connection per in-flight task).
- Use single-node Redis with a replica for HA when 100k+ RPS is required on a single stream (cluster mode adds slot-routing overhead per command).
- Keep `audit_log` writes off the hot path: the engine already only writes `evaluate` audit entries on non-Pass verdicts, so the audit-log write rate equals the breach rate, not the request rate.

## Verification

See `docs/local-dev.md` for the full verification checklist —
including the audit_log write check:

```bash
docker compose exec postgres psql -U propfirm -d propfirm -c \
  "SELECT occurred_at, action, account_id, metadata FROM audit_log ORDER BY id DESC LIMIT 10;"
```
