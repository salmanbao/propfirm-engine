//! PostgreSQL-backed idempotency store.
//!
//! Uses `INSERT ... ON CONFLICT DO NOTHING` for atomic check-and-remember.
//! A periodic `VACUUM` (or even just the index on `expires_at`) keeps the
//! table from growing unbounded — old entries are filtered out by the
//! `expires_at > now()` predicate in the SELECT.

use async_trait::async_trait;
use sqlx::{PgPool, Row};
use std::sync::Arc;
use std::time::Duration;
use uuid::Uuid;

use crate::api::idempotency::{hash_body, IdempotencyOutcome};
use crate::api::IdempotencyBackend;
use crate::tenant::TenantId;

/// Postgres-backed idempotency backend.
#[derive(Clone)]
pub struct PostgresIdempotencyBackend {
    pool: Arc<PgPool>,
    ttl: Duration,
}

impl PostgresIdempotencyBackend {
    /// Create a new backend. `ttl` controls the `expires_at` column on
    /// new inserts; expired entries are filtered on read.
    #[must_use]
    pub fn new(pool: Arc<PgPool>, ttl: Duration) -> Self {
        PostgresIdempotencyBackend { pool, ttl }
    }
}

#[async_trait]
impl IdempotencyBackend for PostgresIdempotencyBackend {
    async fn check(
        &self,
        tenant_id: TenantId,
        endpoint: &str,
        key: &str,
        request_body: &str,
    ) -> IdempotencyOutcome {
        let composite = composite_key(tenant_id, endpoint, key);
        let body_hash = hash_body(request_body);
        let now = chrono::Utc::now();
        match self.lookup(&composite, &body_hash, now).await {
            Ok(o) => o,
            Err(_) => IdempotencyOutcome::Error,
        }
    }

    async fn remember(
        &self,
        tenant_id: TenantId,
        endpoint: &str,
        key: &str,
        request_body: &str,
        response: &str,
    ) -> IdempotencyOutcome {
        let composite = composite_key(tenant_id, endpoint, key);
        let body_hash = hash_body(request_body);
        let now = chrono::Utc::now();
        let expires_at = now
            + chrono::Duration::from_std(self.ttl).unwrap_or_else(|_| chrono::Duration::days(1));
        match self
            .upsert(
                &composite, tenant_id, endpoint, key, &body_hash, response, now, expires_at,
            )
            .await
        {
            Ok(o) => o,
            Err(_) => IdempotencyOutcome::Error,
        }
    }

    /// Override the default with a single conditional INSERT.
    ///
    /// `INSERT ... ON CONFLICT DO NOTHING RETURNING` either returns the row
    /// we just wrote (Fresh) or nothing (we then read the existing row to
    /// see if it's a Replay or Conflict).
    async fn check_and_remember(
        &self,
        tenant_id: TenantId,
        endpoint: &str,
        key: &str,
        request_body: &str,
        response: &str,
    ) -> IdempotencyOutcome {
        let composite = composite_key(tenant_id, endpoint, key);
        let body_hash = hash_body(request_body);
        let now = chrono::Utc::now();
        let expires_at = now
            + chrono::Duration::from_std(self.ttl).unwrap_or_else(|_| chrono::Duration::days(1));

        // Try the upsert; if the row already exists, fall through to lookup
        // to determine Replay vs Conflict.
        match self
            .upsert(
                &composite, tenant_id, endpoint, key, &body_hash, response, now, expires_at,
            )
            .await
        {
            Ok(o) => o,
            Err(_) => {
                // upsert errored — could be a constraint violation from a
                // concurrent writer; fall back to a lookup.
                match self.lookup(&composite, &body_hash, now).await {
                    Ok(o) => o,
                    Err(_) => IdempotencyOutcome::Error,
                }
            }
        }
    }
}

impl PostgresIdempotencyBackend {
    async fn lookup(
        &self,
        composite: &str,
        body_hash: &str,
        now: chrono::DateTime<chrono::Utc>,
    ) -> sqlx::Result<IdempotencyOutcome> {
        let row = sqlx::query(
            r#"SELECT body_hash, response FROM idempotency
               WHERE composite_key = $1 AND expires_at > $2"#,
        )
        .bind(composite)
        .bind(now)
        .fetch_optional(&*self.pool)
        .await?;

        Ok(match row {
            None => IdempotencyOutcome::Fresh,
            Some(row) => {
                let stored_hash: String = row.try_get("body_hash")?;
                let response: String = row.try_get("response")?;
                if stored_hash == body_hash {
                    IdempotencyOutcome::Replay(response)
                } else {
                    IdempotencyOutcome::Conflict
                }
            }
        })
    }

    async fn upsert(
        &self,
        composite: &str,
        tenant_id: TenantId,
        endpoint: &str,
        key: &str,
        body_hash: &str,
        response: &str,
        now: chrono::DateTime<chrono::Utc>,
        expires_at: chrono::DateTime<chrono::Utc>,
    ) -> sqlx::Result<IdempotencyOutcome> {
        let tenant_uuid = tenant_id.raw();
        // INSERT ... ON CONFLICT DO NOTHING — if the row already exists,
        // we'll fall through to the lookup to determine Replay vs Conflict.
        let inserted = sqlx::query(
            r#"INSERT INTO idempotency
                 (composite_key, tenant_id, endpoint, idempotency_key,
                  body_hash, response, inserted_at, expires_at)
               VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
               ON CONFLICT (composite_key) DO NOTHING"#,
        )
        .bind(composite)
        .bind(tenant_uuid)
        .bind(endpoint)
        .bind(key)
        .bind(body_hash)
        .bind(response)
        .bind(now)
        .bind(expires_at)
        .execute(&*self.pool)
        .await?;

        if inserted.rows_affected() == 1 {
            Ok(IdempotencyOutcome::Fresh)
        } else {
            // Row already existed — find out if it's a replay or conflict.
            self.lookup(composite, body_hash, now).await
        }
    }
}

fn composite_key(tenant: TenantId, endpoint: &str, key: &str) -> String {
    format!("{tenant}\u{0}{endpoint}\u{0}{key}")
}
