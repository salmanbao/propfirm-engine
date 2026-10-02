import http from 'k6/http';
import { check, sleep, group } from 'k6';
import { Counter, Trend, Rate } from 'k6/metrics';
import { uuidv4 } from 'https://jslib.k6.io/k6-utils/1.5.0/index.js';
import { SharedArray } from 'k6/data';

// =============================================================================
// k6 load test for the propfirm-engine HTTP API.
// =============================================================================

// ----- Configuration -----
const BASE_URL = __ENV.BASE_URL || 'http://localhost:8080';
const TENANT_ID = __ENV.TENANT_ID || uuidv4();
const RPS = parseInt(__ENV.RPS || '50', 10);
const VUS = parseInt(__ENV.VUS || '50', 10);
const DURATION = __ENV.DURATION || '1m';
const STAGE_RPS = parseInt(__ENV.STAGE_RPS || '0', 10);

// ----- Custom metrics -----
const decisionCounts = new Counter('propfirm_decisions');
const idempotencyReplays = new Counter('propfirm_idempotency_replays');
const idempotencyConflicts = new Counter('propfirm_idempotency_conflicts');
const requestLatency = new Trend('propfirm_request_latency_ms');

// ----- Helpers -----
function makeAccountId(accountIdx) {
  const base = String(accountIdx).padStart(4, '0');
  return `00000000-0000-0000-${base.slice(0, 4)}-${base.slice(0, 4)}${base.slice(0, 4)}`;
}

function buildAccountState(accountId, tenantId) {
  const now = new Date().toISOString();
  return {
    id: accountId,
    account_type: 'Phase1',
    status: 'Active',
    challenge_id: accountId,
    plan: {
      id: accountId,
      phase: 'Phase1',
      meta: {
        firm_name: 'FTMO',
        program_name: 'Phase 1',
        version: '1',
        currency: 'USD',
        description: 'FTMO Phase 1 challenge',
      },
      initial_balance_money: 100000,
      profit_target_pct: 0.08,
      max_daily_drawdown_pct: 0.05,
      max_total_drawdown_pct: 0.10,
      max_loss_reference: 'Static',
      drawdown_on_balance: false,
      trailing_drawdown_enabled: false,
      trailing_drawdown_pct: 0,
      min_trading_days: 4,
      time_limit_days: 30,
      max_position_lots: null,
      max_total_lots: null,
      max_open_positions: null,
      max_daily_trades: null,
      news_trading_allowed: true,
      overnight_holding_allowed: true,
      weekend_holding_allowed: false,
      hedging_allowed: true,
      grid_trading_allowed: true,
      require_stop_loss: true,
      require_take_profit: false,
      consistency_pct: 0.40,
      cooldown_seconds: 0,
      copy_trading_allowed: false,
      refundable: true,
      leverage: 100,
      trading_hours: null,
      timezone: null,
      day_reset_time: 0,
      effective_at: now,
      per_trade_max_loss_pct: null,
      per_trade_max_loss_money: null,
      hft_ban_enabled: false,
      hft_min_round_trip_seconds: 60,
      inactivity_days: null,
      refund_fee_amount: 0,
      payout_config: null,
    },
    tenant_id: tenantId,
    initial_balance: 100000,
    balance: 100000,
    equity: 102000,
    estimated_equity: 102000,
    estimated_balance: 100000,
    peak_balance: 100000,
    peak_equity: 102000,
    started_at: now,
    deadline: new Date(Date.now() + 30 * 24 * 60 * 60 * 1000).toISOString(),
    day_start_balance: 100000,
    trading_day_index: 1,
    active_trading_days: 1,
    day_counted_today: true,
    today_realized_pnl: 0,
    total_realized_pnl: 0,
    total_commissions: 0,
    total_swaps: 0,
    largest_day_profit: 0,
    largest_day_loss: 0,
    sum_positive_days_profit: 0,
    day_start_equity: 102000,
    current_trading_day_start: now,
    target_reached_at: null,
    target_reached_on_day: 0,
    version: 0,
    last_tick_ts: null,
    last_trade_at: null,
    payout_count: 0,
    balance_at_last_payout: 100000,
    last_payout_at: null,
    refund_used: false,
    status_before_breach: null,
    open_positions: [],
    today_trades: [],
  };
}

function buildEvaluateRequest(accountIdx, decisionKind) {
  const accountId = makeAccountId(accountIdx % 10000);
  const equityCents = decisionKind === 'breach' ? 850000 : 10200000;
  const now = Date.now();
  return {
    account_id: accountId,
    account_state: buildAccountState(accountId, TENANT_ID),
    bridge_tick: {
      type: 'bridge.tick',
      version: 1,
      tenant_id: TENANT_ID,
      occurred_at: now,
      payload: {
        equity_cents: equityCents,
        balance_cents: 10000000,
        margin_cents: 0,
        free_margin_cents: 0,
        leverage: 100,
        positions: [],
        deals_count: 0,
        broker_time: now,
      },
    },
  };
}

// ----- k6 options -----
export const options = {
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
      maxVUs: VUS * 2,
    },
  },
  thresholds: {
    http_req_duration: ['p(99)<100'],
    http_req_failed: ['rate<0.01'],
    propfirm_idempotency_conflicts: ['count==0'],
  },
  tags: {
    service: 'propfirm-engine',
    tenant: TENANT_ID,
  },
};

// ----- Default function -----
export default function () {
  const idx = (__ITER % 10000);
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
  if (Number.isFinite(res.timings.waiting) && Number.isFinite(res.timings.processing)) {
    requestLatency.add(res.timings.waiting + res.timings.processing);
  }

  let decisionKind = 'Error';
  try {
    const j = res.json();
    decisionKind = j.decision_kind || 'Unknown';
  } catch (_) {}
  decisionCounts.add(1, { kind: decisionKind });

  check(res, {
    'status is 200': (r) => r.status === 200,
    'has input_hash': (r) => {
      try { return r.json('input_hash')?.startsWith('sha256:'); } catch (_) { return false; }
    },
  });
}

// ----- Idempotency replay -----
export function idempotencyReplay() {
  const body = JSON.stringify(buildEvaluateRequest(parseInt(__ITER, 10) % 10000, 'pass'));
  const idemKey = uuidv4();

  const params = {
    headers: {
      'Content-Type': 'application/json',
      'X-Tenant-Id': TENANT_ID,
      'Idempotency-Key': idemKey,
    },
    tags: { scenario: 'idempotency_replay' },
  };

  const first = http.post(`${BASE_URL}/internal/v1/evaluate`, body, params);
  if (first.status === 200) {
    idempotencyReplays.add(0);
  }

  const second = http.post(`${BASE_URL}/internal/v1/evaluate`, body, params);
  if (second.status === 200) {
    try {
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

// ----- Setup -----
export function setup() {
  console.log(`k6 load test starting: BASE_URL=${BASE_URL}, RPS=${RPS}, VUS=${VUS}, DURATION=${DURATION}`);
  const health = http.get(`${BASE_URL}/health`);
  if (health.status !== 200) {
    throw new Error(`engine not healthy at ${BASE_URL}/health (status=${health.status})`);
  }
  console.log(`engine healthy at ${BASE_URL} (${health.body})`);
  const metrics = http.get(`${BASE_URL}/metrics`);
  if (metrics.status !== 200) {
    console.warn(`/metrics returned ${metrics.status} (continuing anyway)`);
  }
  return { started_at: new Date().toISOString() };
}

export function teardown(data) {
  console.log(`k6 load test finished at ${new Date().toISOString()}`);
}

// ----- Summary -----
import { textSummary } from 'https://jslib.k6.io/k6-summary/0.0.2/index.js';
export function handleSummary(data) {
  return {
    'stdout': textSummary(data, { indent: ' ', enableColors: true }),
  };
}
