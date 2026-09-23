-- Day-rollover fix: persist the account's current trading-day boundary.
ALTER TABLE accounts
    ADD COLUMN IF NOT EXISTS current_trading_day_start TIMESTAMPTZ;
