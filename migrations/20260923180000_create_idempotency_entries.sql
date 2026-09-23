-- Idempotency entries: durable, tenant-scoped deduplication store.
-- Each row is scoped to a tenant so cross-tenant replay is impossible.
CREATE TABLE IF NOT EXISTS idempotency_entries (
    tenant_id      uuid NOT NULL,
    endpoint      text NOT NULL,
    idempotency_key text NOT NULL,
    body_hash     text NOT NULL,
    response      text NOT NULL,
    created_at    timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (tenant_id, endpoint, idempotency_key)
);

CREATE INDEX IF NOT EXISTS idx_idempotency_entries_tenant_created
    ON idempotency_entries (tenant_id, created_at);
