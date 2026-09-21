-- §E.1: trades table
CREATE TABLE IF NOT EXISTS trades (
    id UUID PRIMARY KEY,
    account_id UUID NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    symbol TEXT NOT NULL,
    side TEXT NOT NULL,
    trade_side TEXT NOT NULL,
    price NUMERIC NOT NULL,
    quantity NUMERIC NOT NULL,
    realized_pnl NUMERIC NOT NULL DEFAULT 0,
    commission NUMERIC NOT NULL DEFAULT 0,
    swap NUMERIC NOT NULL DEFAULT 0,
    executed_at TIMESTAMPTZ NOT NULL,
    position_id UUID,
    exit_price NUMERIC,
    closed_quantity NUMERIC,
    entry_price NUMERIC,
    metadata JSONB,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_trades_account_id ON trades(account_id);
CREATE INDEX IF NOT EXISTS idx_trades_executed_at ON trades(executed_at);
