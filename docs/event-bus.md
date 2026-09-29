# Redis Streams Event Bus

The propfirm-engine is designed for **async communication** with the
platform backend via Redis Streams. This document covers the wire
format, the consumer model, and verification steps.

## Why streams (not pub/sub, not HTTP)

| | Pub/Sub | HTTP | **Streams** |
|---|---|---|---|
| Persistence | ❌ (lost if no subscriber) | n/a | ✅ (until trimmed) |
| At-least-once delivery | ❌ | ❌ (sync) | ✅ (consumer groups + PEL) |
| Backpressure | ❌ | ❌ | ✅ (consumer can poll) |
| Restart-safe | ❌ | ❌ | ✅ (XAUTOCLAIM recovers PEL) |
| Latency | low (push) | high (sync round-trip) | medium (XREADGROUP block) |
| Throughput | very high | medium | high |

Streams are the right choice for async evaluation traffic:
- A worker restart never loses a request.
- A crashed worker's pending messages get auto-claimed after
  `idle_claim_ms`.
- Throughput scales horizontally with worker count.

## Streams

| Stream | Direction | Producer | Consumer |
|---|---|---|---|
| `propfirm:evaluate:requests` | platform → engine | platform backend | propfirm-worker |
| `propfirm:evaluate:responses` | engine → platform | propfirm-worker | platform backend |

Both streams are created with `MKSTREAM` (auto-created on first XADD).

## Consumer group

`propfirm-worker` (configurable via `event_bus.consumer_group`).

A consumer group lets multiple workers share the request stream
without overlap: each message goes to exactly one consumer. Each
consumer tracks a PEL (Pending Entries List) of messages it has read
but not yet XACKed.

If a consumer dies, `XAUTOCLAIM` lets another consumer pick up its
pending messages after `idle_claim_ms` (default 60s).

## Wire format

### Request payload

XADD fields:

| Field | Type | Description |
|---|---|---|
| `request_id` | string (UUID) | Correlation key for the response. |
| `payload` | string (JSON) | The full request body (see DTO below). |

The `payload` JSON has the same shape as the HTTP
`InternalEvaluateRequest`:

```json
{
  "account_id": "uuid",
  "tenant_id": "uuid",
  "account_state": { ... Account ... },
  "bridge_tick": { ... optional BridgeTickV1 ... },
  "tick": { ... optional legacy per-symbol tick ... },
  "equity_source": "broker_reported" | "estimated",
  "open_positions": [...],
  "today_trades": [...],
  "cross_reference_trades": [...]
}
```

### Response payload

XADD fields:

| Field | Type | Description |
|---|---|---|
| `request_id` | string (UUID) | Matches the request. |
| `payload` | string (JSON) | The full response body (see DTO below). |

Response `payload` JSON:

```json
{
  "request_id": "uuid",
  "decision_kind": "Pass" | "Warn" | "Fail" | "Liquidate" | "Emergency" | ...,
  "input_hash": "sha256:...",
  "account_state": { ... updated Account ... },
  "violations": [...],
  "processed_at": "2026-09-29T10:30:00.123Z",
  "error": null | "error message"
}
```

If `error` is non-null, the worker couldn't process the request
(deserialization error, panic, etc.). The platform backend can retry
or surface to the operator.

## Worker model

### Concurrency

Each `propfirm-worker` process spawns N concurrent consumer tasks
(configurable via `event_bus.concurrency`, default 16). Each task:

1. Calls `XREADGROUP` with `COUNT 1 BLOCK <block_ms>` — blocks up to
   5 seconds waiting for a message.
2. Deserializes the request payload.
3. Builds an evaluator from the account's plan (P0.2 defense-in-depth).
4. Runs `pure::evaluate` (P1-7 stateless, with `input_hash`).
5. Applies the decision to the account (state mutation, ADR-11 caller-owned).
6. Serializes the response and `XADD`s it to the response stream.
7. `XACK`s the request message (removes from PEL).

If any step panics, the panic hook logs it via `tracing::error`, the
worker writes a response with `error: "panic: ..."` and still `XACK`s
so the message leaves the PEL (don't poison the queue with a
poison-message loop).

### PEL recovery

A periodic task (every 30 seconds) calls `XAUTOCLAIM` to grab messages
that have been pending for more than `idle_claim_ms` (default 60s).
This handles:

- Worker process killed (SIGKILL, OOM)
- Worker panicked and exited
- Network partition between worker and Redis

### Idempotency

Delivery is **at-least-once**. The platform backend must include an
`Idempotency-Key` field in the request payload (currently the worker
doesn't read this — it relies on the request_id for correlation, but
re-processing the same request_id is safe because the evaluation is
pure).

In a future PR, the worker will use the `IdempotencyBackend` (Redis
backend recommended) to short-circuit duplicate processing.

## Verification

### 1. Start the stack

```bash
docker compose up -d
```

### 2. Verify the consumer group exists

```bash
docker compose exec redis redis-cli XINFO GROUPS propfirm:evaluate:requests
```

Expected output:

```
1)  1) "name"
    2) "propfirm-worker"
    3) "consumers"
    4) (integer) 1
    5) "pending"
    6) (integer) 0
    7) "last-delivered-id"
    8) "0-0"
```

### 3. Produce a request directly

```bash
REQUEST_ID=$(uuidgen)
TENANT_ID=$(uuidgen)
ACCOUNT_ID=$(uuidgen)

docker compose exec -T redis redis-cli XADD propfirm:evaluate:requests '*' \
  request_id "$REQUEST_ID" \
  payload "{
    \"account_id\": \"$ACCOUNT_ID\",
    \"tenant_id\": \"$TENANT_ID\",
    \"account_state\": {
      \"id\": \"$ACCOUNT_ID\",
      \"tenant_id\": \"$TENANT_ID\",
      \"initial_balance\": {\"amount\": \"100000\", \"code\": \"USD\"},
      \"balance\": {\"amount\": \"100000\", \"code\": \"USD\"},
      \"equity\": {\"amount\": \"100000\", \"code\": \"USD\"},
      \"status\": \"Active\",
      \"plan\": { ... ftmo_phase1 ... }
    },
    \"bridge_tick\": {
      \"payload\": {
        \"equity_cents\": 10000000,
        \"balance_cents\": 10000000,
        \"broker_time\": $(date +%s%3N),
        \"positions\": []
      }
    }
  }"
```

### 4. Wait for the response

```bash
sleep 2
docker compose exec redis redis-cli XINFO GROUPS propfirm:evaluate:responses
docker compose exec redis redis-cli XRANGE propfirm:evaluate:responses - +
```

The response stream should contain one entry with the matching
`request_id` and a populated `decision_kind` field.

### 5. Verify the worker logs

```bash
docker compose logs --tail 30 propfirm-worker
```

Expected log lines:

```
INFO propfirm-worker: consumer started consumer=worker-0
INFO propfirm-worker: received request consumer=worker-0 stream_id=... request_id=...
INFO propfirm-worker: request processed request_id=... stream_id=...
```

### 6. Test PEL recovery

```bash
# Stop the worker.
docker compose stop propfirm-worker

# Produce a request (it'll sit in the stream, undelivered).
docker compose exec -T redis redis-cli XADD propfirm:evaluate:requests '*' \
  request_id "$(uuidgen)" \
  payload '...'

# Wait a minute.
sleep 70

# Restart the worker.
docker compose start propfirm-worker

# The worker should pick up the pending message via XAUTOCLAIM and process it.
docker compose logs --tail 30 propfirm-worker | grep "claimed idle"
```

## Throughput considerations

Each `XREADGROUP` call has a `block_ms` latency floor (default 5s).
For high-throughput scenarios:

1. **Lower `block_ms`**: trade latency for CPU. `block_ms = 100` gives
   ~10ms latency floor at the cost of busy-polling.
2. **Raise `concurrency`**: more consumer tasks per worker. Default 16
   is fine for most workloads; raise to 32-64 for very high QPS.
3. **Run more workers**: each worker is independent; just deploy more
   replicas. The consumer group handles deduplication.
4. **Use single-node Redis**: cluster mode serializes ops through a
   Mutex in v0.2.0. For 100k+ RPS, prefer single-node Redis with a
   replica for HA.

## Failure modes

### Worker crashes mid-message

The message stays in the PEL. After `idle_claim_ms` (default 60s),
another consumer's `XAUTOCLAIM` will pick it up and reprocess.

If the message has side effects (it doesn't — pure evaluate is
side-effect-free), the platform backend must handle at-least-once
delivery via its own idempotency layer.

### Redis goes down

The worker logs `consume_request error; sleeping 1s` and retries
forever. No messages are lost (they're still in the stream). Once
Redis is back, the worker resumes consuming.

### Worker can't deserialize a request

The worker writes a response with `error: "decode payload: <reason>"`
and XACKs the message so it leaves the PEL. The platform backend sees
the error and can choose to investigate.

### Worker panics during evaluation

The panic hook logs the panic. The worker writes a response with
`error: "panic: <message>"` and XACKs. The process stays alive (panic
hook + axum-style catch_unwind in the worker loop).

## Future work

- **Idempotency short-circuit**: check the `IdempotencyBackend` before
  evaluating. If we've already seen this `Idempotency-Key`, replay the
  cached response instead of re-evaluating. Saves CPU on retries.
- **Bulk XADD responses**: instead of one XADD per response, batch N
  responses per XADD (Redis supports multi-entry XADD). Lower Redis
  round-trips.
- **Producer-side idempotency**: the platform backend should include
  an `Idempotency-Key` in the request so the worker can dedup at the
  evaluation seam, not just at the HTTP seam.
