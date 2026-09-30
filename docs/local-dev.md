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

## Required `X-Tenant-Id` header

Every `/internal/v1/*` and `/v1/*` request **must** carry an
`X-Tenant-Id` header — without it, the engine returns HTTP 400 with
the body `missing X-Tenant-Id header` (see
`src/api/handlers.rs::extract_tenant_id`, lines 40–52).

The engine has **no authentication** (intentionally removed in v0.2.0).
Trust is established at the network boundary (private compose network,
k8s NetworkPolicy, and optionally mTLS — see `docs/tls.md`). The
`X-Tenant-Id` header is **not** cryptographic identity; it's the
typed tenant id the engine uses for:

- Audit-log scoping (`audit_log.tenant_id` column).
- Request validation: the engine cross-checks the header against
  `account_state.tenant_id` and returns HTTP 403 if they mismatch.

So every curl request to a stateful endpoint needs `-H "X-Tenant-Id: <uuid>"`.
For probes (`/health`, `/ready`, `/metrics`), the header is not required.

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
# Expected: a Prometheus-formatted metrics dump with every metric
# prefixed by `propfirm_` (http_requests_total, evaluate_decisions_total,
# idempotency_outcomes_total, request_duration_seconds, event_bus_*).
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

### 11. Missing `X-Tenant-Id` returns 400

```bash
curl -fsS -o /dev/null -w '%{http_code}\n' -X POST http://localhost:8080/internal/v1/evaluate \
  -H "Content-Type: application/json" \
  -H "Idempotency-Key: $(uuidgen)" \
  -d @/tmp/eval.json
# Expected: 400 (the body says "missing X-Tenant-Id header")
```

### 12. Breach-report (POST with JSON body)

```bash
curl -fsS -X POST http://localhost:8080/internal/v1/breach-report \
  -H "Content-Type: application/json" \
  -H "X-Tenant-Id: $TENANT_ID" \
  -d "{\"account_id\":\"$ACCOUNT_ID\"}" | jq .
```

The route is `POST /internal/v1/breach-report` with a JSON body
`{"account_id": "uuid"}` (not a path parameter). The engine
cross-checks the account's tenant against the `X-Tenant-Id` header
and returns 403 on mismatch.

### 13. Audit log writes

```bash
# Send a non-Pass evaluation (e.g. trigger a drawdown breach by
# submitting a tick with low equity), then check the audit_log table:
docker compose exec postgres psql -U propfirm -d propfirm -c \
  "SELECT occurred_at, action, account_id, metadata->>'decision_kind' AS decision, \
   request_hash FROM audit_log ORDER BY id DESC LIMIT 10;"
```

Expected: rows for `action = 'evaluate'` (non-Pass only), `breach_report`
queries, etc. The `evaluate` audit_log row only appears when the
decision is non-Pass (Pass verdicts are observable via the
`propfirm_evaluate_decisions_total{kind="Pass"}` metric instead).

### 14. Graceful shutdown

```bash
# Send a long-running request, then SIGTERM the server mid-flight.
docker compose exec propfirm-server sh -c 'kill -TERM 1'
docker compose logs --tail 20 propfirm-server
# Expected: "shutdown signal received, draining in-flight requests"
```

### 15. TLS verification (optional)

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

For mTLS verification, see `docs/tls.md` — generate a CA, server cert,
and client cert, then set `PROPFIRM_SERVER__TLS__CLIENT_CA_PATH` to
the CA bundle.

### 16. Worker subcommands

```bash
# Healthcheck (useful for k8s liveness probes):
docker compose exec propfirm-worker /app/propfirm-worker healthcheck

# Status (PEL stats + consumer list):
docker compose exec propfirm-worker /app/propfirm-worker status

# Drain stuck PEL entries older than 120s:
docker compose exec propfirm-worker /app/propfirm-worker drain 120

# Recreate the consumer group (after a Redis flush):
docker compose exec propfirm-worker /app/propfirm-worker reset-group
```

### 17. OpenAPI + Swagger UI (when `openapi` feature is enabled)

```bash
# Build with the openapi feature:
cargo run --release --features server,openapi --bin propfirm-server

# Fetch the OpenAPI spec:
curl -fsS http://localhost:8080/openapi.json | jq .info

# Open the Swagger UI in a browser:
open http://localhost:8080/swagger-ui/
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

### "missing X-Tenant-Id header" (HTTP 400)

You sent a request to `/internal/v1/*` or `/v1/*` without the
`X-Tenant-Id` header. Add `-H "X-Tenant-Id: $(uuidgen)"` to your curl
command. (Probes like `/health`, `/ready`, `/metrics`, `/openapi.json`,
`/swagger-ui/` do not require it.)

### "account_state.tenant_id ... does not match authenticated tenant" (HTTP 403)

The `X-Tenant-Id` header value does not match the `tenant_id` field
inside the `account_state` JSON body. Make sure both are the same UUID.

### Audit log table is empty after Pass evaluations

This is expected — the `evaluate` audit_log write only fires for
**non-Pass** verdicts (to avoid drowning the table in normal traffic).
Pass verdicts are observable via the
`propfirm_evaluate_decisions_total{kind="Pass"}` metric instead. To
verify audit_log writes, trigger a breach (e.g. submit a tick with
low equity to trigger `MaxDrawdown`) and re-check the table.

## Cleanup

```bash
docker compose down -v   # stops containers and wipes data volumes
```
