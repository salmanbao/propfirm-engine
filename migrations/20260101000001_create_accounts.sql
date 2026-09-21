-- §E.1: accounts table with OCC + tenant isolation
CREATE TABLE IF NOT EXISTS accounts (
    id UUID PRIMARY KEY,
    tenant_id UUID NOT NULL,
    account_type TEXT NOT NULL,
    status TEXT NOT NULL,
    challenge_id UUID NOT NULL,
    plan JSONB NOT NULL,
    initial_balance NUMERIC NOT NULL,
    balance NUMERIC NOT NULL,
    equity NUMERIC NOT NULL,
    peak_equity NUMERIC NOT NULL,
    peak_balance NUMERIC NOT NULL,
    started_at TIMESTAMPTZ,
    deadline TIMESTAMPTZ,
    day_start_balance NUMERIC NOT NULL,
    trading_day_index INTEGER NOT NULL DEFAULT 0,
    active_trading_days INTEGER NOT NULL DEFAULT 0,
    day_counted_today BOOLEAN NOT NULL DEFAULT FALSE,
    today_realized_pnl NUMERIC NOT NULL DEFAULT 0,
    total_realized_pnl NUMERIC NOT NULL DEFAULT 0,
    total_commissions NUMERIC NOT NULL DEFAULT 0,
    total_swaps NUMERIC NOT NULL DEFAULT 0,
    largest_day_profit NUMERIC NOT NULL DEFAULT 0,
    largest_day_loss NUMERIC NOT NULL DEFAULT 0,
    sum_positive_days_profit NUMERIC NOT NULL DEFAULT 0,
    day_start_equity NUMERIC NOT NULL DEFAULT 0,
    target_reached_at TIMESTAMPTZ,
    target_reached_on_day INTEGER,
    version BIGINT NOT NULL DEFAULT 0,
    last_tick_ts TIMESTAMPTZ,
    last_trade_at TIMESTAMPTZ,
    payout_count INTEGER NOT NULL DEFAULT 0,
    balance_at_last_payout NUMERIC NOT NULL DEFAULT 0,
    last_payout_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_accounts_tenant_id ON accounts(tenant_id);
CREATE INDEX IF NOT EXISTS idx_accounts_status ON accounts(status);
CREATE INDEX IF NOT EXISTS idx_accounts_challenge_id ON accounts(challenge_id);
