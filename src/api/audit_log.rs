//! Audit log writes for sensitive operations (override, emergency-stop).
//!
//! Writes to the `audit_log` table (see `migrations/0001_init.sql`) on
//! the Postgres pool stored in `ServerState.pg_pool`. If the pool is
//! absent (e.g. memory-only dev mode), the writes are silently skipped
//! — the operation still succeeds, just no audit record.
//!
//! Schema:
//! ```sql
//! CREATE TABLE audit_log (
//!     id              BIGSERIAL PRIMARY KEY,
//!     occurred_at     TIMESTAMPTZ NOT NULL DEFAULT now(),
//!     correlation_id  UUID,
//!     actor_id        TEXT,
//!     action          TEXT NOT NULL,
//!     tenant_id       UUID,
//!     account_id      UUID,
//!     resource_kind   TEXT,
//!     resource_id     TEXT,
//!     request_hash    TEXT,
//!     response_status INTEGER,
//!     latency_ms      INTEGER,
//!     metadata        JSONB
//! );
//! ```

use std::sync::Arc;
use std::time::Instant;
use uuid::Uuid;

use crate::core::ids::{AccountId, ViolationId};
use crate::tenant::TenantId;

/// An in-progress audit record being built. Call `.finish(...)` to
/// persist. If the Postgres pool isn't configured, `.finish` is a no-op.
pub struct AuditEntry {
    pub actor_id: String,
    pub action: String,
    pub tenant_id: Option<TenantId>,
    pub account_id: Option<AccountId>,
    pub resource_kind: Option<String>,
    pub resource_id: Option<String>,
    pub request_hash: Option<String>,
    pub metadata: serde_json::Value,
    pub started_at: Instant,
}

impl AuditEntry {
    /// Begin a new audit entry. Use `.with_actor` / `.with_resource` to
    /// populate the optional fields, then call `.finish(...)`.
    #[must_use]
    pub fn start(action: impl Into<String>, actor_id: impl Into<String>) -> Self {
        AuditEntry {
            actor_id: actor_id.into(),
            action: action.into(),
            tenant_id: None,
            account_id: None,
            resource_kind: None,
            resource_id: None,
            request_hash: None,
            metadata: serde_json::Value::Null,
            started_at: Instant::now(),
        }
    }

    #[must_use]
    pub fn with_tenant(mut self, tenant: TenantId) -> Self {
        self.tenant_id = Some(tenant);
        self
    }

    #[must_use]
    pub fn with_account(mut self, account: AccountId) -> Self {
        self.account_id = Some(account);
        self
    }

    #[must_use]
    pub fn with_resource(mut self, kind: impl Into<String>, id: impl Into<String>) -> Self {
        self.resource_kind = Some(kind.into());
        self.resource_id = Some(id.into());
        self
    }

    #[must_use]
    pub fn with_request_hash(mut self, hash: impl Into<String>) -> Self {
        self.request_hash = Some(hash.into());
        self
    }

    #[must_use]
    pub fn with_metadata(mut self, metadata: serde_json::Value) -> Self {
        self.metadata = metadata;
        self
    }

    /// Persist the audit entry. No-op when `pg_pool` is None.
    pub async fn finish(
        self,
        pg_pool: Option<&Arc<sqlx::PgPool>>,
        correlation_id: Option<Uuid>,
        response_status: i32,
    ) {
        let Some(pool) = pg_pool else {
            // No audit log configured — silently skip. The operation
            // already succeeded; we just don't have a record of it.
            tracing::debug!(action = %self.action, "audit log skipped (no pg_pool)");
            return;
        };

        let elapsed_ms = self.started_at.elapsed().as_millis() as i32;
        let occurred_at = chrono::Utc::now();
        let tenant_uuid = self.tenant_id.map(|t| t.raw());
        let account_uuid = self.account_id.map(|a| a.raw());

        let result = sqlx::query(
            r#"INSERT INTO audit_log
                 (occurred_at, correlation_id, actor_id, action,
                  tenant_id, account_id, resource_kind, resource_id,
                  request_hash, response_status, latency_ms, metadata)
               VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12)"#,
        )
        .bind(occurred_at)
        .bind(correlation_id)
        .bind(&self.actor_id)
        .bind(&self.action)
        .bind(tenant_uuid)
        .bind(account_uuid)
        .bind(&self.resource_kind)
        .bind(&self.resource_id)
        .bind(&self.request_hash)
        .bind(response_status)
        .bind(elapsed_ms)
        .bind(self.metadata)
        .execute(&**pool)
        .await;

        if let Err(e) = result {
            tracing::error!(error = %e, action = %self.action, "audit log write failed");
        }
    }
}

/// Convenience: log an override action. Returns a builder ready to
/// `.finish(...)`.
#[must_use]
pub fn override_breach(
    actor_id: &str,
    tenant: TenantId,
    account: AccountId,
    clears_violation: ViolationId,
    reason: &str,
) -> AuditEntry {
    AuditEntry::start("override_breach", actor_id)
        .with_tenant(tenant)
        .with_account(account)
        .with_resource("violation", clears_violation.to_string())
        .with_metadata(serde_json::json!({
            "reason": reason,
            "clears_violation_id": clears_violation.to_string(),
        }))
}

/// Convenience: log an emergency-stop action.
#[must_use]
pub fn emergency_stop(
    actor_id: &str,
    tenant: TenantId,
    account: AccountId,
    reason: &str,
) -> AuditEntry {
    AuditEntry::start("emergency_stop", actor_id)
        .with_tenant(tenant)
        .with_account(account)
        .with_metadata(serde_json::json!({
            "reason": reason,
        }))
}

/// Convenience: log a manual-run action.
#[must_use]
pub fn manual_run(actor_id: &str, tenant: TenantId, account: AccountId) -> AuditEntry {
    AuditEntry::start("manual_run", actor_id)
        .with_tenant(tenant)
        .with_account(account)
}

/// Convenience: log an evaluate action. Called from the
/// `/internal/v1/evaluate` handler — but only when the decision is
/// non-Pass (so the audit log doesn't drown in normal traffic).
#[must_use]
pub fn evaluate(
    tenant: TenantId,
    account: AccountId,
    decision_kind: &str,
    input_hash: &str,
) -> AuditEntry {
    AuditEntry::start("evaluate", "evaluate_internal")
        .with_tenant(tenant)
        .with_account(account)
        .with_request_hash(input_hash)
        .with_metadata(serde_json::json!({
            "decision_kind": decision_kind,
            "input_hash": input_hash,
        }))
}

/// Convenience: log an evaluate-order action (pre-trade order check).
#[must_use]
pub fn evaluate_order(
    tenant: TenantId,
    account: AccountId,
    symbol: &str,
    side: &str,
    decision: &str,
) -> AuditEntry {
    AuditEntry::start("evaluate_order", "evaluate_order")
        .with_tenant(tenant)
        .with_account(account)
        .with_resource("order", format!("{symbol}:{side}"))
        .with_metadata(serde_json::json!({
            "symbol": symbol,
            "side": side,
            "decision": decision,
        }))
}

/// Convenience: log a breach-report query (trader-facing read of
/// violations). Read-only but audited so the platform can see who
/// queried breach reports when.
#[must_use]
pub fn breach_report(
    tenant: TenantId,
    account: AccountId,
    violation_count: usize,
    cleared_count: usize,
) -> AuditEntry {
    AuditEntry::start("breach_report", "breach_report")
        .with_tenant(tenant)
        .with_account(account)
        .with_metadata(serde_json::json!({
            "violation_count": violation_count,
            "cleared_count": cleared_count,
        }))
}

/// Convenience: log an event-bus worker evaluation. Called by the
/// `propfirm-worker` binary after each message is consumed + processed
/// + ack'd. The `consumer_name` distinguishes which worker pod handled
/// the message; `request_id` is the cross-correlation key with the
/// platform backend.
#[must_use]
pub fn worker_evaluate(
    consumer_name: &str,
    tenant: TenantId,
    account: AccountId,
    decision_kind: &str,
    request_id: &str,
    input_hash: &str,
) -> AuditEntry {
    AuditEntry::start("worker_evaluate", consumer_name)
        .with_tenant(tenant)
        .with_account(account)
        .with_request_hash(input_hash)
        .with_resource("request", request_id.to_string())
        .with_metadata(serde_json::json!({
            "consumer": consumer_name,
            "decision_kind": decision_kind,
            "request_id": request_id,
            "input_hash": input_hash,
        }))
}

/// Convenience: log a worker error (decode failure, redis error, panic,
/// etc.). The `consumer_name` is the actor; `request_id` is the
/// correlation key with the platform backend.
#[must_use]
pub fn worker_error(
    consumer_name: &str,
    tenant_id: Option<TenantId>,
    account_id: Option<AccountId>,
    request_id: &str,
    error_kind: &str,
    error_msg: &str,
) -> AuditEntry {
    let mut entry = AuditEntry::start("worker_error", consumer_name)
        .with_resource("request", request_id.to_string())
        .with_metadata(serde_json::json!({
            "consumer": consumer_name,
            "request_id": request_id,
            "error_kind": error_kind,
            "error_msg": error_msg,
        }));
    if let Some(t) = tenant_id {
        entry = entry.with_tenant(t);
    }
    if let Some(a) = account_id {
        entry = entry.with_account(a);
    }
    entry
}
