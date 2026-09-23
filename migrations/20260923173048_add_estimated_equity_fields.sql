-- Add separate estimated-equity/balance fields for the TickEstimated path.
-- These are display/backtest-only and must never drive authoritative equity or peak tracking.
ALTER TABLE accounts
    ADD COLUMN IF NOT EXISTS estimated_equity   numeric(38,18) NOT NULL DEFAULT 0,
    ADD COLUMN IF NOT EXISTS estimated_balance  numeric(38,18) NOT NULL DEFAULT 0;
