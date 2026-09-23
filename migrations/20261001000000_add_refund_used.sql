-- Add refund_used to accounts
ALTER TABLE accounts ADD COLUMN IF NOT EXISTS refund_used BOOLEAN NOT NULL DEFAULT FALSE;
