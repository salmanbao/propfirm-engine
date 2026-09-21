-- §E.1: rule_packs table
CREATE TABLE IF NOT EXISTS rule_packs (
    id TEXT PRIMARY KEY,
    tenant_id UUID NOT NULL,
    version INTEGER NOT NULL DEFAULT 1,
    lifecycle TEXT NOT NULL,
    effective_from TIMESTAMPTZ,
    rules JSONB NOT NULL,
    content_hash TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_rule_packs_tenant_id ON rule_packs(tenant_id);
CREATE INDEX IF NOT EXISTS idx_rule_packs_lifecycle ON rule_packs(lifecycle);
