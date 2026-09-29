# Persistence

The propfirm-engine ships with three pluggable persistence backends:

| Backend | Use case | Survives restart | Survives crash | Shared across replicas |
|---|---|---|---|---|
| `memory` | Dev / tests | ❌ | ❌ | ❌ |
| `postgres` | Durable audit + idempotency | ✅ | ✅ | ✅ |
| `redis` | Durable idempotency + event bus | ✅ (AOF) | ✅ | ✅ |

Select via `PROPFIRM_IDEMPOTENCY__BACKEND` env var (or
`[idempotency] backend = "..."` in TOML).

## Architecture

```
┌──────────────────────────────────────────────────────────┐
│                      HTTP server                         │
│  ┌───────────────────┐         ┌──────────────────────┐ │
│  │   /internal/v1/  │         │     /metrics         │ │
│  │     evaluate     │         └──────────────────────┘ │
│  └─────────┬────────┘                                  │
│            │                                            │
└────────────┼────────────────────────────────────────────┘
             │
             ▼
   ┌──────────────────┐
   │  IdempotencyBackend  ◄── trait, dyn Arc
   └─────────┬────────┘
             │
       ┌─────┴─────┬─────────────┐
       ▼           ▼             ▼
 ┌──────────┐ ┌───────────┐ ┌───────────┐
 │ Memory   │ │ Postgres  │ │  Redis    │
 │ (default)│ │           │ │           │
 └──────────┘ └───────────┘ └───────────┘
                              ▲
                              │
                ┌─────────────┴────────────┐
                │   propfirm-worker binary │
                │   (Redis Streams         │
                │    event bus)            │
                └──────────────────────────┘
```

## 1. PostgreSQL (recommended for durable audit)

### Schema

Migration: `src/persistence/migrations/0001_init.sql`

Tables:

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
    inserted_at     TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX events_account_idx ON events (account_id, occurred_at);
CREATE INDEX events_tenant_idx  ON events (tenant_id, occurred_at);
CREATE INDEX events_kind_idx     ON events (kind);
```

Used by `PostgresEventStore` for the durable domain-event audit
log. The `replay(account_id)` method reconstructs `Account` state
from the event log — the dispute-resolution seam.

#### `idempotency`

```sql
CREATE TABLE idempotency (
    composite_key   TEXT PRIMARY KEY,
    tenant_id       UUID NOT NULL,
    endpoint        TEXT NOT NULL,
    idempotency_key TEXT NOT NULL,
    body_hash       TEXT NOT NULL,
    response        TEXT NOT NULL,
    inserted_at     TIMESTAMPTZ NOT NULL DEFAULT now(),
    expires_at      TIMESTAMPTZ NOT NULL
);
CREATE INDEX idempotency_tenant_idx ON idempotency (tenant_id);
CREATE INDEX idempotency_expires_idx ON idempotency (expires_at);
```

Used by `PostgresIdempotencyBackend`. Atomic check-and-remember via
`INSERT ... ON CONFLICT DO NOTHING`, then a `SELECT` to determine
Replay vs Conflict.

#### `rule_packs`

For versioned rule packs (Draft → Active → Superseded). Lifecycle
preserved for replay. See `src/rulepack.rs`.

#### `audit_log`

For who-did-what-when audit trail (overrides, emergency stops).
Schema exists; write-path not yet wired (TODO).

### Connection pooling

`sqlx::PgPool` with `max_connections` from settings (default 10).
Pool is wrapped in `Arc` and shared across all handlers.

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

- **Single-node**: `MultiplexedConnection` (cloneable, async-safe,
  handles pipelining internally).
- **Cluster**: `Arc<tokio::sync::Mutex<ClusterConnection>>` —
  serialized cluster ops. For higher throughput, run multiple workers.

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

- **Request stream**: `propfirm:evaluate:requests`
- **Response stream**: `propfirm:evaluate:responses`
- **Consumer group**: `propfirm-worker` (configurable)

Wire format (XADD fields):

- `request_id` — UUID (correlation key)
- `payload` — JSON-serialized request/response

#### Consumer lifecycle

1. `ensure_group()` creates the consumer group with `MKSTREAM` —
   idempotent (ignores `BUSYGROUP`).
2. Each worker task calls `XREADGROUP` with `COUNT 1 BLOCK <block_ms>`.
3. After processing, `XACK` removes the message from the PEL.
4. A periodic `XAUTOCLAIM` task picks up messages idle for
   `idle_claim_ms` (default 60s) — recovery for crashed workers.

#### Sample

```bash
# Produce a request
redis-cli XADD propfirm:evaluate:requests '*' \
  request_id "$(uuidgen)" \
  payload '{"account_id":"...", "tenant_id":"...", ...}'

# Consume (the worker does this; here's the underlying call)
redis-cli XREADGROUP GROUP propfirm-worker worker-1 \
  COUNT 1 BLOCK 5000 STREAMS propfirm:evaluate:requests '>'

# Ack
redis-cli XACK propfirm:evaluate:requests propfirm-worker 1234-0

# Inspect pending
redis-cli XPENDING propfirm:evaluate:requests propfirm-worker

# Reclaim idle messages (the worker's periodic task does this)
redis-cli XAUTOCLAIM propfirm:evaluate:requests propfirm-worker worker-1 60000 0-0 COUNT 10
```

### Cluster mode

Set `redis.cluster = true` and provide a comma-separated URL list:

```toml
[redis]
url = "redis://node-1:6379,redis://node-2:6379,redis://node-3:6379"
cluster = true
```

The `redis` crate's `cluster-async` feature handles slot routing
internally. Note: cluster ops are serialized through a Mutex in
this PoC — for production throughput, run multiple worker processes
each with their own `ClusterConnection`.

## 3. In-memory (dev only)

`IdempotencyStore` and `InMemoryEventStore` are the defaults when
`backend = "memory"`. Useful for:

- Unit tests (no I/O)
- Local dev where you don't want to bring up Postgres/Redis
- CI smoke tests

**Never use in production** — all state is lost on every deploy,
causing double-applied mutations on client retries.

## Backend selection matrix

| Setup | Use when |
|---|---|
| `backend = memory` | Local dev / CI smoke |
| `backend = postgres` | Durable idempotency + audit log; HTTP-only (no event bus) |
| `backend = redis` | Durable idempotency + event bus; recommended for HA |
| Both `postgres` + `redis` | `backend = redis` (idempotency) + Postgres still used for event store + audit log |

The default `docker-compose.yml` ships with `backend = redis` and
Postgres still wired for the event store.

## Verification

See `docs/local-dev.md` for the full verification checklist.
