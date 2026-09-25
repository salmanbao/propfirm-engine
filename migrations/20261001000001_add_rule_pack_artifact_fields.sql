-- Add missing rule-pack artifact fields so round-trip preserves full pack data.
ALTER TABLE rule_packs
    ADD COLUMN IF NOT EXISTS description TEXT NOT NULL DEFAULT '',
    ADD COLUMN IF NOT EXISTS initial_balance NUMERIC NOT NULL DEFAULT 0,
    ADD COLUMN IF NOT EXISTS leverage INTEGER NOT NULL DEFAULT 1,
    ADD COLUMN IF NOT EXISTS profit_target_pct NUMERIC NOT NULL DEFAULT 0,
    ADD COLUMN IF NOT EXISTS superseded_by TEXT;
