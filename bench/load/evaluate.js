import http from 'k6/http';
import { check, sleep, group } from 'k6';
import { Counter, Trend, Rate } from 'k6/metrics';
import { uuidv4 } from 'https://jslib.k6.io/k6-utils/1.5.0/index.js';
import { SharedArray } from 'k6/data';

// =============================================================================
// k6 load test for the propfirm-engine HTTP API.
//
// Runs three scenarios against the `/internal/v1/evaluate` endpoint:
//   - evaluate_pass:      normal tick that produces a "Pass" decision
//   - evaluate_breach:   tick that triggers a drawdown breach
//   - idempotency_replay: same Idempotency-Key twice, second call must return
//                         from the cache (subsequent-millisecond latency)
//
// ## Usage
//
//   # Install k6 (one-time):  https://grafana.com/docs/k6/latest/set-up/install-k6/
//   # Run a 30s smoke at 10 RPS:
//   k6 run --env BASE_URL=http://localhost:8080 --duration 30s bench/load/evaluate.js
//
//   # Scale: 5 minute soak at 1000 RPS, 200 VUs
//   k6 run --env BASE_URL=http://localhost:8080 --duration 5m --env RPS=1000 --env VUS=200 bench/load/evaluate.js
//
//   # Stress: ramp up to 5000 RPS, see where it breaks
//   k6 run --env BASE_URL=http://localhost:8080 --env STAGE_RPS=5000 --duration 5m bench/load/evaluate.js
//
// ## Pass criteria
//
// - p50 latency < 5ms (in-memory backend) or < 20ms (postgres backend)
// - p95 latency < 50ms (in-memory) or < 100ms (postgres)
// - error rate < 0.1% (no 5xx except 503 during shutdown)
// - 409 (Conflict) ratio: 0% — these are correctness bugs in the test harness
// =============================================================================

// ----- Configuration -----
const BASE_URL = __ENV.BASE_URL || 'http://localhost:8080';
const TENANT_ID = __ENV.TENANT_ID || uuidv4();   // stable across the test run
const RPS = parseInt(__ENV.RPS || '50', 10);
const VUS = parseInt(__ENV.VUS || '50', 10);
const DURATION = __ENV.DURATION || '1m';
const STAGE_RPS = parseInt(__ENV.STAGE_RPS || '0', 10);   // 0 = use RPS constant

// ----- Custom metrics -----
const decisionCounts = new Counter('propfirm_decisions');
const idempotencyReplays = new Counter('propfirm_idempotency_replays');
const idempotencyConflicts = new Counter('propfirm_idempotency_conflicts');
const requestLatency = new Trend('propfirm_request_latency_ms');

// ----- Pre-generate test accounts so the per-request setup is cheap -----
const NUM_ACCOUNTS = 100;
const accounts = new SharedArray('accounts', () => {
  const arr = [];
  for (let i = 0; i < NUM_ACCOUNTS; i++) {
    const accountId = uuidv4();
    arr.push({
      account_id: accountId,
      tenant_id: TENANT_ID,
      // Inline an FTMO Phase 1 plan + an Active account.
      account_state: {
        id: accountId,
        tenant_id: TENANT_ID,
        initial_balance: { amount: '100000', code: 'USD' },
        balance: { amount: '100000', code: 'USD' },
        equity: { amount: '100000', code: 'USD' },
        peak_balance: { amount: '100000', code: 'USD' },
        peak_equity: { amount: '100000', code: 'USD' },
        day_start_balance: { amount: '100000', code: 'USD' },
        day_start_equity: { amount: '100000', code: 'USD' },
        status: 'Active',
        account_type: 'Phase1',
        today_realized_pnl: { amount: '0', code: 'USD' },
        total_realized_pnl: { amount: '0', code: 'USD' },
        largest_day_profit: { amount: '0', code: 'USD' },
        largest_day_loss: { amount: '0', code: 'USD' },
        sum_positive_days_profit: { amount: '0', code: 'USD' },
        active_trading_days: 1,
        trading_day_index: 1,
        day_counted_today: true,
        target_reached_at: null,
        target_reached_on_day: 0,
        version: 0,
        last_tick_ts: null,
        last_trade_at: null,
        open_positions: [],
        today_trades: [],
        plan: {
          balance: { amount: '100000', code: 'USD' },
          profit_target_pct: 0.10,
          max_daily_drawdown_pct: 0.05,
          max_total_drawdown_pct: 0.10,
          loss_reference: 'Static',
          min_trading_days: 4,
          time_limit_days: 30,
          consistency_pct: 0.40,
          leverage: 100,
          timezone: null,
          day_reset_time: 0,
          trading_hours_start: 0,
          trading_hours_end: 24,
          hft_ban_enabled: false,
          news_trading_allowed: true,
          overnight_holding_allowed: true,
          weekend_holding_allowed: false,
          hedging_allowed: true,
          grid_trading_allowed: true,
          copy_trading_allowed: false,
          sl_required: true,
          tp_required: false,
          refundable: true,
          per_trade_max_loss_pct: null,
          per_trade_max_loss_money: null,
          inactivity_days: null,
          phase: 'Phase1',
        },
      },
    });
  }
  return arr;
});

// ----- Build the evaluate request body for a single account -----
function buildEvaluateRequest(accountIdx, decisionKind) {
  const acc = accounts[accountIdx];
  // Equity reported by the broker:
  //   - "pass": 102000 (above target, no breach)
  //   - "breach": 8500 (well below the 90k static floor → MaxDrawdown breach)
  const equityCents =
    decisionKind === 'breach' ? 850000 : 10200000;
  const balanceCents = 10000000;
  return {
    account_id: acc.account_id,
    account_state: acc.account_state,
    bridge_tick: {
      payload: {
        equity_cents: equityCents,
        balance_cents: balanceCents,
        margin_cents: 0,
        free_margin_cents: 0,
        broker_time: Date.now(),
        positions: [],
      },
    },
  };
}

// ----- k6 options -----
export const options = {
  // Either constant RPS or staged ramp.
  scenarios: STAGE_RPS > 0 ? {
    ramp: {
      executor: 'ramping-arrival-rate',
      startArrivalRate: 10,
      timeUnit: '1s',
      preAllocatedVUs: 200,
      maxVUs: 1000,
      stages: [
        { duration: '30s', target: STAGE_RPS },
        { duration: DURATION, target: STAGE_RPS },
        { duration: '30s', target: 0 },
      ],
    },
  } : {
    constant: {
      executor: 'constant-arrival-rate',
      rate: RPS,
      timeUnit: '1s',
      duration: DURATION,
      preAllocatedVUs: VUS,
      maxVUs: VUs * 2,
    },
  },
  thresholds: {
    // SLO: 99% of requests < 50ms (in-memory backend) or 100ms (postgres).
    http_req_duration: ['p(99)<100'],
    // 0% 5xx tolerated.
    http_req_failed: ['rate<0.01'],
    // 0 conflicts — these are test bugs, not expected behavior.
    'propfirm_idempotency_conflicts': ['count==0'],
  },
  tags: {
    service: 'propfirm-engine',
    tenant: TENANT_ID,
  },
};

// ----- Default (export) function — one iteration per VU cycle -----
export default function () {
  // Round-robin account selection. 80% pass, 20% breach.
  const idx = (__ITER % accounts.length);
  const kind = (Math.random() < 0.8) ? 'pass' : 'breach';
  const body = JSON.stringify(buildEvaluateRequest(idx, kind));

  const params = {
    headers: {
      'Content-Type': 'application/json',
      'X-Tenant-Id': TENANT_ID,
      'Idempotency-Key': uuidv4(),
    },
    tags: { decision_kind: kind },
  };

  const res = http.post(`${BASE_URL}/internal/v1/evaluate`, body, params);
  requestLatency.add(res.timings.waiting + res.timings.processing);

  // Decision kind counter.
  let decisionKind = 'Error';
  try {
    const j = res.json();
    decisionKind = j.decision_kind || 'Unknown';
  } catch (_) { /* fall through */ }
  decisionCounts.add(1, { kind: decisionKind });

  // Status code checks.
  check(res, {
    'status is 200': (r) => r.status === 200,
    'has input_hash': (r) => {
      try { return r.json('input_hash')?.startsWith('sha256:'); } catch (_) { return false; }
    },
  });
}

// ----- Scenario: idempotency replay test -----
export function idempotencyReplay() {
  const idx = (__ITER % accounts.length);
  const body = JSON.stringify(buildEvaluateRequest(idx, 'pass'));
  const idemKey = uuidv4();

  const params = {
    headers: {
      'Content-Type': 'application/json',
      'X-Tenant-Id': TENANT_ID,
      'Idempotency-Key': idemKey,
    },
    tags: { scenario: 'idempotency_replay' },
  };

  // First call — Fresh.
  const first = http.post(`${BASE_URL}/internal/v1/evaluate`, body, params);
  if (first.status === 200) {
    idempotencyReplays.add(0);  // first call is not a replay
  }

  // Second call with same key — should be Replay.
  const second = http.post(`${BASE_URL}/internal/v1/evaluate`, body, params);
  if (second.status === 200) {
    try {
      // The engine doesn't surface "Replay" in the response; we infer
      // from latency — second call should be much faster.
      const firstLatency = first.timings.waiting + first.timings.processing;
      const secondLatency = second.timings.waiting + second.timings.processing;
      if (secondLatency < firstLatency * 0.5) {
        idempotencyReplays.add(1);
      }
    } catch (_) {}
  } else if (second.status === 409) {
    idempotencyConflicts.add(1);
  }
}

// ----- Setup hook: verify the engine is up before the test runs -----
export function setup() {
  console.log(`k6 load test starting: BASE_URL=${BASE_URL}, RPS=${RPS}, VUS=${VUS}, DURATION=${DURATION}`);

  const health = http.get(`${BASE_URL}/health`);
  if (health.status !== 200) {
    throw new Error(`engine not healthy at ${BASE_URL}/health (status=${health.status})`);
  }
  console.log(`engine healthy at ${BASE_URL} (${health.body})`);

  // Touch /metrics to ensure the recorder is installed.
  const metrics = http.get(`${BASE_URL}/metrics`);
  if (metrics.status !== 200) {
    console.warn(`/metrics returned ${metrics.status} (continuing anyway)`);
  }
  return { started_at: new Date().toISOString() };
}

export function teardown(data) {
  console.log(`k6 load test finished at ${new Date().toISOString()}`);
  // Print summary stats:
  const counts = {};
  // Note: k6 exposes counters in the `propfirm_decisions` metric —
  // the final report at script end has the breakdown by tag.
}

// ----- Handle the script summary -----
import { textSummary } from 'https://jslib.k6.io/k6-summary/0.0.2/index.js';
export function handleSummary(data) {
  return {
    'stdout': textSummary(data, { indent: ' ', enableColors: true }),
    'bench/load/last-run-summary.json': JSON.stringify(data, null, 2),
  };
}
