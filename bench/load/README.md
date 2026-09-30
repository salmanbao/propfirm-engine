# Load testing with k6

This directory contains k6 load-test scripts for the propfirm-engine
HTTP API. They are designed to be runnable against the local
`docker compose up` deployment with zero extra setup.

## Prerequisites

Install k6 from https://grafana.com/docs/k6/latest/set-up/install-k6/
(one-time):

```bash
# macOS:
brew install k6

# Debian/Ubuntu:
sudo gpg -k
sudo gpg --no-default-keyring --keyring /usr/share/keyrings/k6-archive-keyring.gpg --keyserver hkp://keyserver.ubuntu.com:80 --recv-keys C5AD17C747E682D32E2AD1DBA6D2DB5DE5C69602
echo "deb [signed-by=/usr/share/keyrings/k6-archive-keyring.gpg] https://dl.k6.io/deb stable main" | sudo tee /etc/apt/sources.list.d/k6.list
sudo apt update && sudo apt install k6

# Docker (no install):
docker run --rm -i --network=host -v $(pwd):/workspace -w /workspace grafana/k6:latest run - <bench/load/evaluate.js
```

## Available scripts

### `evaluate.js` — the main load test

Tests the `/internal/v1/evaluate` endpoint with three traffic patterns:

| Pattern             | Description                                                  | % of traffic |
|---------------------|--------------------------------------------------------------|--------------|
| `pass` decisions    | Normal broker ticks that produce a `Pass` verdict            | 80%          |
| `breach` decisions  | Ticks that trigger a `MaxDrawdown` breach (equity < 90k)    | 20%          |
| `idempotency_replay`| Same `Idempotency-Key` twice — second call should be cached  | (separate)   |

#### Custom metrics exposed

| Metric                                | Type      | Labels         | Description |
|---------------------------------------|-----------|----------------|-------------|
| `propfirm_decisions`                  | counter   | `kind`         | Count of each decision kind (`Pass`, `Fail`, `Liquidate`, ...) |
| `propfirm_idempotency_replays`        | counter   | (none)         | Count of requests served from the idempotency cache (low latency) |
| `propfirm_idempotency_conflicts`      | counter   | (none)         | Count of 409 Conflict responses (should always be 0) |
| `propfirm_request_latency_ms`         | trend     | (none)         | End-to-end request latency in ms |

#### Built-in k6 metrics (always present)

- `http_req_duration` — overall request latency
- `http_req_failed` — rate of non-2xx responses
- `vus` / `iterations` / `data_received` / `data_sent` — standard k6

## Quick start

```bash
# 1. Bring up the engine stack:
docker compose up -d --build

# 2. Verify it's healthy:
curl http://localhost:8080/health  # → ok

# 3. Run a 1-minute smoke test at 50 RPS:
k6 run --env BASE_URL=http://localhost:8080 bench/load/evaluate.js

# 4. Run a 5-minute soak at 1000 RPS, 200 VUs:
k6 run --env BASE_URL=http://localhost:8080 --duration 5m --env RPS=1000 --env VUS=200 bench/load/evaluate.js

# 5. Stress-ramp to 5000 RPS (find where the engine breaks):
k6 run --env BASE_URL=http://localhost:8080 --env STAGE_RPS=5000 --duration 5m bench/load/evaluate.js
```

## Environment variables

| Variable     | Default                                | Description                          |
|--------------|----------------------------------------|--------------------------------------|
| `BASE_URL`   | `http://localhost:8080`                | Engine HTTP URL                      |
| `TENANT_ID`  | (random UUID, stable per run)          | Tenant to use for all requests       |
| `RPS`        | `50`                                   | Constant requests per second         |
| `VUS`        | `50`                                   | Pre-allocated virtual users          |
| `DURATION`   | `1m`                                   | Test duration                        |
| `STAGE_RPS`  | `0`                                    | If > 0, use ramp-to-RPS instead of constant |

## SLOs and thresholds

The script enforces these thresholds (configurable in the `options`
block at the top of `evaluate.js`):

```javascript
thresholds: {
  http_req_duration: ['p(99)<100'],         // 99th percentile < 100ms
  http_req_failed: ['rate<0.01'],           // < 1% error rate (5xx)
  propfirm_idempotency_conflicts: ['count==0'],  // 0 conflicts (test bug if any)
}
```

If any threshold fails, k6 exits with non-zero status — useful for CI.

## Expected performance

On a single-node Redis + memory backend, the engine sustains
~205,000 evals/sec on the pure path (per the cargo bench). HTTP adds
~50-200µs of overhead per request. So:

- 1000 RPS, 200 VUs → p99 should be ~5-10ms (in-memory backend)
- 5000 RPS, 500 VUs → p99 should be ~50ms (in-memory backend)
- 1000 RPS, 200 VUs → p99 should be ~20-30ms (postgres backend)
- 1000 RPS, 200 VUs → p99 should be ~10-15ms (redis backend)

If you see p99 > 100ms at modest RPS, something's wrong:

- Postgres backend with cold pool → first N requests are slow until the
  pool warms up. Run a 30s warmup before measuring.
- Memory backend saturated → the bounded LRU+TTL store (`capacity=10_000`)
  starts evicting. Bump `idempotency.max_entries` or use `postgres` /
  `redis` backends for high-QPS tests.
- HTTP timeouts at low concurrency → check
  `PROPFIRM_SERVER__REQUEST_TIMEOUT_SECS=30` is enough; if you're
  loading > 30s p99 you have a deeper problem.

## CI integration

Add this to your CI pipeline as a smoke test:

```bash
# Start the engine:
docker compose up -d --build

# Wait for /health:
for i in 1 2 3 4 5; do
  curl -fsS http://localhost:8080/health && break
  sleep 2
done

# Run a 30s smoke at 10 RPS:
k6 run --env BASE_URL=http://localhost:8080 --duration 30s --env RPS=10 bench/load/evaluate.js

# Capture the JSON summary as an artifact:
cp bench/load/last-run-summary.json k6-summary-$(date -u +%Y%m%dT%H%M%SZ).json

# Tear down:
docker compose down -v
```

The k6 exit code is non-zero if any threshold is violated — wire this
up as a CI gate: 5xx rate > 0.1% = test failure = pipeline fails.

## Verifying idempotency correctness

The script's `idempotency_replay` scenario sends the same
`Idempotency-Key` twice and asserts the second response is served
from the cache (detected via latency). If you see
`propfirm_idempotency_conflicts > 0`:

1. Check that you're using the same body on both requests.
2. Check that the idempotency backend isn't evicting (memory backend
   has `max_entries=10_000`).
3. Check the engine logs for `idempotency lookup failed` errors —
   these indicate the backend is unreachable.

## Output samples

### Successful smoke test (in-memory backend, 50 RPS, 1m)

```
  ✓ status is 200
  ✓ has input_hash

  checks.................................: 100.00% ✓ 6000  ✗ 0
  data_received..........................: 1.2 MB  20 kB/s
  data_sent..............................: 4.3 MB  71 kB/s
  http_req_duration......................: avg=3.2ms   min=850µs  med=2.9ms  max=18ms   p(90)=4.5ms  p(95)=5.2ms  p(99)=7.1ms
  http_req_failed........................: 0.00%   ✓ 0     ✗ 3000
  http_reqs.............................: 3000    50.00/sec
  iterations............................: 3000    50.00/sec
  propfirm_decisions{kind=Pass}.........: 2400    40.00/sec
  propfirm_decisions{kind=Fail}........: 600     10.00/sec
  propfirm_idempotency_conflicts........: 0       0.00/sec
  propfirm_idempotency_replays.........: 0       0.00/sec
  vus...................................: 50      min=50   max=50
```

### Failure case (postgres backend cold-start)

```
  ✓ status is 200
  ✗ has input_hash: 30% of requests returned 500 (postgres pool exhausted)

  http_req_duration......................: avg=120ms  min=850µs  med=80ms  max=2.5s   p(99)=1.8s
  http_req_failed........................: 30%     ✗ 900   ✓ 2100
```

→ Fix: warm the Postgres pool with a 30-second ramp before the test.
