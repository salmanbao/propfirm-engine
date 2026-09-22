-- §E.1: positions table
CREATE TABLE IF NOT EXISTS positions (
    id UUID PRIMARY KEY,
    account_id UUID NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    symbol TEXT NOT NULL,
    side TEXT NOT NULL,
    opened_quantity NUMERIC NOT NULL,
    open_quantity NUMERIC NOT NULL,
    avg_entry_price NUMERIC NOT NULL,
    status TEXT NOT NULL,
    opened_at TIMESTAMPTZ NOT NULL,
    closed_at TIMESTAMPTZ,
    closed_price NUMERIC,
    realized_pnl NUMERIC,
    swap NUMERIC,
    commission NUMERIC,
    stop_loss NUMERIC,
    take_profit NUMERIC,
    magic BIGINT,
    comment TEXT,
    metadata JSONB,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_positions_account_id ON positions(account_id);
CREATE INDEX IF NOT EXISTS idx_positions_status ON positions(status);
