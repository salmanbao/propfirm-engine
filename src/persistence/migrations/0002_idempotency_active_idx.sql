-- 0002_idempotency_active_idx.sql — partial index for active idempotency rows.
--
-- Background: the idempotency table grows monotonically (writes from the
-- server / worker, TTL-bounded deletes). The previous full-table btree
-- index on (tenant_id) and the one on (expires_at) both index ALL rows,
-- including the expired ones that the steady-state read path actively
-- filters out with `WHERE expires_at > now()`.
--
-- This partial index keys on (composite_key) but only over rows where
-- `expires_at > now()`. It's used by:
--   - `lookup()`  — the slow-path SELECT after an upsert conflict.
--   - Future queries that filter active rows.
--
-- Why it works: when the query predicate is `expires_at > now()` (the
-- SQL function, not a bind parameter), Postgres recognizes the partial
-- index's predicate as implied and uses the partial index. The result
-- is a much smaller index (only currently-active rows) for the same
-- lookups, which means:
--   - Better cache locality (fewer index pages hit per lookup).
--   - Faster autovacuum (smaller index to maintain).
--   - Smaller on-disk footprint for the index.
--
-- The bind-parameter form `expires_at > $1` would NOT use this partial
-- index because the planner can't statically know $1 ≈ now(). The
-- idempotency backend's `lookup()` SQL has been adjusted to use SQL's
-- `now()` directly so the partial index kicks in.
--
-- Idempotent: `CREATE INDEX IF NOT EXISTS` is a no-op if the index
-- already exists.

-- +migrate Up
CREATE INDEX IF NOT EXISTS idempotency_active_idx
    ON idempotency (composite_key)
    WHERE expires_at > now();

-- +migrate Down
DROP INDEX IF EXISTS idempotency_active_idx;
