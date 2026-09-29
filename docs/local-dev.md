# Local Development & Verification Guide

This guide walks you through bringing up the full propfirm-engine stack
locally (Postgres + Redis + server + worker) and verifying each
integration works end-to-end.

## Prerequisites

- Docker Engine 24+ and Docker Compose v2
- Rust toolchain (only for building from source)
- `curl`, `jq`, `redis-cli`, `psql` (optional, for manual poking)

## Quick start

```bash
cd /path/to/propfirm-engine
cp .env.example .env
docker compose up -d --build
```

This brings up four containers:

| Service           | Container           | Port(s)              |
|-------------------|---------------------|----------------------|
| PostgreSQL 16     | `propfirm-postgres` | 5432                 |
| Redis 7           | `propfirm-redis`    | 6379                 |
| propfirm-server   | `propfirm-server`   | 8080 (HTTP)          |
| propfirm-worker   | `propfirm-worker`   | —                    |

## Verification checklist

### 1. Containers are up

```bash
docker compose ps
```

All four services should show `healthy` under STATUS.

### 2. Liveness probe

```bash
curl -fsS http://localhost:8080/health
# Expected: ok
```

### 3. Readiness probe

```bash
curl -fsS http://localhost:8080/ready
# Expected: ready
```

### 4. Prometheus metrics

```bash
curl -fsS http://localhost:8080/metrics | head -20
# Expected: a Prometheus-formatted metrics dump
```

### 5. Postgres schema applied

```bash
docker compose exec postgres psql -U propfirm -d propfirm -c '\dt'
# Expected: tables events, idempotency, rule_packs, audit_log
```

### 6. Redis Streams consumer group

```bash
docker compose exec redis redis-cli XINFO GROUPS propfirm:evaluate:requests
# Expected: one group named "propfirm-worker" with pending count 0
```

### 7. End-to-end evaluation (HTTP path)

```bash
# Submit an evaluation request.
TENANT_ID=$(uuidgen)
ACCOUNT_ID=$(uuidgen)

# Build the request body (this is the stateless evaluate contract):
cat > /tmp/eval.json <<EOF
{
  "account_id": "$ACCOUNT_ID",
  "account_state": {
    "id": "$ACCOUNT_ID",
    "tenant_id": "$TENANT_ID",
    "initial_balance": {"amount": "100000", "code": "USD"},
    "balance": {"amount": "100000", "code": "USD"},
    "equity": {"amount": "100000", "code": "USD"},
    "status": "Active",
    "plan": { ... },
    ...
  },
  "bridge_tick": {
    "payload": {
      "equity_cents": 10000000,
      "balance_cents": 10000000,
      "broker_time": $(date +%s%3N),
      "positions": []
    }
  }
}
EOF

curl -fsS -X POST http://localhost:8080/internal/v1/evaluate \
  -H "Content-Type: application/json" \
  -H "X-Tenant-Id: $TENANT_ID" \
  -H "Idempotency-Key: $(uuidgen)" \
  -d @/tmp/eval.json | jq .
```

Expected response shape:

```json
{
  "evaluated": true,
  "decision_kind": "Pass",
  "winning_priority": 0,
  "input_hash": "sha256:...",
  "pack_version": 1,
  "pack_id": "...",
  "violations": [],
  "violation_details": [],
  "account_state": { ... }
}
```

### 8. End-to-end evaluation (event bus path)

```bash
# Produce a request directly to the Redis stream.
REQUEST_ID=$(uuidgen)
TENANT_ID=$(uuidgen)
ACCOUNT_ID=$(uuidgen)

docker compose exec redis redis-cli XADD propfirm:evaluate:requests '*' \
  request_id "$REQUEST_ID" \
  tenant_id "$TENANT_ID" \
  account_id "$ACCOUNT_ID" \
  payload "{\"account_id\":\"$ACCOUNT_ID\", ...}"

# Within ~5s, the worker will consume, process, and XADD a response.
docker compose exec redis redis-cli XINFO GROUPS propfirm:evaluate:responses
docker compose exec redis redis-cli XRANGE propfirm:evaluate:responses - +
```

### 9. Idempotency replay

```bash
# Send the same request twice with the same Idempotency-Key — second
# call should return the same response (from the Redis cache) without
# re-evaluating.
KEY=$(uuidgen)
curl -fsS -X POST http://localhost:8080/internal/v1/evaluate \
  -H "X-Tenant-Id: $TENANT_ID" -H "Idempotency-Key: $KEY" \
  -H "Content-Type: application/json" -d @/tmp/eval.json | jq .input_hash

# Repeat — input_hash should be identical, but server logs show "Replay".
curl -fsS -X POST http://localhost:8080/internal/v1/evaluate \
  -H "X-Tenant-Id: $TENANT_ID" -H "Idempotency-Key: $KEY" \
  -H "Content-Type: application/json" -d @/tmp/eval.json | jq .input_hash
```

### 10. Idempotency conflict

```bash
# Same Idempotency-Key with a different body → 409 Conflict.
KEY=$(uuidgen)
curl -fsS -X POST http://localhost:8080/internal/v1/evaluate \
  -H "X-Tenant-Id: $TENANT_ID" -H "Idempotency-Key: $KEY" \
  -H "Content-Type: application/json" -d @/tmp/eval.json

# Modify body, send again — expect 409:
jq '.account_state.equity.amount = "90000"' /tmp/eval.json > /tmp/eval2.json
curl -fsS -o /dev/null -w '%{http_code}\n' -X POST http://localhost:8080/internal/v1/evaluate \
  -H "X-Tenant-Id: $TENANT_ID" -H "Idempotency-Key: $KEY" \
  -H "Content-Type: application/json" -d @/tmp/eval2.json
# Expected: 409
```

### 11. Graceful shutdown

```bash
# Send a long-running request, then SIGTERM the server mid-flight.
docker compose exec propfirm-server sh -c 'kill -TERM 1'
docker compose logs --tail 20 propfirm-server
# Expected: "shutdown signal received, draining in-flight requests"
```

### 12. TLS verification (optional)

To enable in-process TLS for verification:

```bash
# Generate a self-signed cert + key:
openssl req -x509 -newkey rsa:2048 -nodes \
  -keyout /tmp/key.pem -out /tmp/cert.pem -days 365 \
  -subj '/CN=localhost'

# Run with TLS enabled:
PROPFIRM_SERVER__TLS__ENABLED=true \
PROPFIRM_SERVER__TLS__CERT_PATH=/tmp/cert.pem \
PROPFIRM_SERVER__TLS__KEY_PATH=/tmp/key.pem \
cargo run --release --features server --bin propfirm-server

# Verify (self-signed certs need --insecure):
curl --insecure https://localhost:8080/health
# Expected: ok
```

## Common integration issues and fixes

### "FATAL: failed to connect to Redis"

The Redis URL is unreachable. Check:

```bash
docker compose exec propfirm-server sh -c 'echo > /dev/tcp/redis/6379' && echo ok
```

If that fails, ensure Redis is healthy (`docker compose ps redis`) and
that `PROPFIRM_REDIS__URL` points to `redis://redis:6379` (the docker
service name).

### "FATAL: failed to load settings"

The most common cause is an env var with a wrong type (e.g.,
`PROPFIRM_REDIS__CLUSTER="yes"` instead of `"true"`). Settings use
serde — values must be valid TOML/JSON types.

Run with debug logging to see the actual error:

```bash
PROPFIRM_OBSERVABILITY__LOG_FORMAT=pretty \
PROPFIRM_OBSERVABILITY__LOG_FILTER=debug \
docker compose up
```

### "migrations failed: relation already exists"

The `run_migrations` flag is `true` by default and is idempotent, but if
you previously ran with `sqlx::migrate` from outside the engine (e.g.,
manually), the `_sqlx_migrations` table may be stale. Reset:

```bash
docker compose down -v  # destroys volumes
docker compose up -d
```

### "panic caught by hook"

The panic hook installed by `install_panic_hook()` logs the panic
message + backtrace via `tracing::error`. The process stays alive —
axum's framework-level `catch_unwind` converts the panic to a 500
response, and the next request is unaffected.

If you see this in production:

1. Note the `panic = ...` line in the logs
2. Find the `location` field — it tells you which file:line caused it
3. File a bug; the panic hook exists specifically to surface these

### Worker not consuming messages

Check:

```bash
docker compose exec redis redis-cli XINFO GROUPS propfirm:evaluate:requests
docker compose exec redis redis-cli XINFO CONSUMERS propfirm:evaluate:requests propfirm-worker
docker compose logs propfirm-worker
```

If the consumer group doesn't exist, the worker failed to create it on
startup (Redis might have rejected the `MKSTREAM` because of an ACL).
Restart the worker.

If pending count is high but no consumer is active, the worker crashed
mid-message. `XAUTOCLAIM` will pick up idle messages after
`idle_claim_ms` (default 60s).

## Verification scripts

A handful of convenience scripts live under `scripts/`:

- `scripts/health-check.sh` — hits `/health`, `/ready`, `/metrics` and prints status codes
- `scripts/redis-stream-test.sh` — produces a fake request, waits for the response

## Cleanup

```bash
docker compose down -v   # stops containers and wipes data volumes
```
