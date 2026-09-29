-- 0001_init.sql — initial schema for propfirm-engine durable persistence.
--
-- Tables:
--   events              — append-only domain event log (event-sourcing seam)
--   idempotency         — idempotency-key deduplication table
--   rule_packs          — versioned rule packs (Draft/Active/Superseded)
--   audit_log           — who-did-what-when audit trail (overrides, emergency stops)

-- +migrate Up
CREATE TABLE IF NOT EXISTS events (
    id              UUID PRIMARY KEY,
    account_id      UUID NOT NULL,
    tenant_id       UUID NOT NULL,
    kind            TEXT NOT NULL,
    payload         JSONB NOT NULL,
    occurred_at     TIMESTAMPTZ NOT NULL,
    causation_id   UUID,
    inserted_at     TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS events_account_idx     ON events (account_id, occurred_at);
CREATE INDEX IF NOT EXISTS events_tenant_idx      ON events (tenant_id, occurred_at);
CREATE INDEX IF NOT EXISTS events_kind_idx       ON events (kind);
CREATE INDEX IF NOT EXISTS events_causation_idx  ON events (causation_id);

CREATE TABLE IF NOT EXISTS idempotency (
    composite_key   TEXT PRIMARY KEY,         -- "{tenant}\0{endpoint}\0{key}"
    tenant_id       UUID NOT NULL,
    endpoint        TEXT NOT NULL,
    idempotency_key TEXT NOT NULL,
    body_hash       TEXT NOT NULL,             -- sha256 hex of request body
    response        TEXT NOT NULL,             -- serialized first response
    inserted_at     TIMESTAMPTZ NOT NULL DEFAULT now(),
    expires_at      TIMESTAMPTZ NOT NULL
);
CREATE INDEX IF NOT EXISTS idempotency_tenant_idx   ON idempotency (tenant_id);
CREATE INDEX IF NOT EXISTS idempotency_expires_idx ON idempotency (expires_at);

CREATE TABLE IF NOT EXISTS rule_packs (
    id              TEXT NOT NULL,
    version         INTEGER NOT NULL,
    tenant_id       UUID NOT NULL,
    lifecycle       TEXT NOT NULL,            -- 'draft' | 'active' | 'superseded'
    effective_from  TIMESTAMPTZ NOT NULL,
    superseded_by   TEXT,
    content_hash    TEXT NOT NULL,            -- sha256 of rule-pack content
    rules_json      JSONB NOT NULL,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (id, version, tenant_id)
);
CREATE INDEX IF NOT EXISTS rule_packs_tenant_active_idx
    ON rule_packs (tenant_id, lifecycle) WHERE lifecycle = 'active';

CREATE TABLE IF NOT EXISTS audit_log (
    id              BIGSERIAL PRIMARY KEY,
    occurred_at     TIMESTAMPTZ NOT NULL DEFAULT now(),
    correlation_id  UUID,
    actor_id        TEXT,                    -- "evaluate_internal" / actor override / "worker"
    action          TEXT NOT NULL,            -- "evaluate" / "override" / "emergency_stop" / ...
    tenant_id       UUID,
    account_id      UUID,
    resource_kind   TEXT,                    -- "account" / "rule_pack" / ...
    resource_id     TEXT,
    request_hash    TEXT,                    -- sha256 of request body if available
    response_status INTEGER,
    latency_ms      INTEGER,
    metadata        JSONB                    -- free-form context
);
CREATE INDEX IF NOT EXISTS audit_log_tenant_idx   ON audit_log (tenant_id, occurred_at DESC);
CREATE INDEX IF NOT EXISTS audit_log_account_idx ON audit_log (account_id, occurred_at DESC);
CREATE INDEX IF NOT EXISTS audit_log_action_idx  ON audit_log (action, occurred_at DESC);

-- +migrate Down
DROP TABLE IF EXISTS audit_log;
DROP TABLE IF EXISTS rule_packs;
DROP TABLE IF EXISTS idempotency;
DROP TABLE IF EXISTS events;
