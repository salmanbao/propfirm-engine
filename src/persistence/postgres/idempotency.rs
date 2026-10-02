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
            .upsert(UpsertParams {
                composite: &composite,
                tenant_id,
                endpoint,
                key,
                body_hash: &body_hash,
                response,
                now,
                expires_at,
            })
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
            .upsert(UpsertParams {
                composite: &composite,
                tenant_id,
                endpoint,
                key,
                body_hash: &body_hash,
                response,
                now,
                expires_at,
            })
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

    async fn upsert(&self, params: UpsertParams<'_>) -> sqlx::Result<IdempotencyOutcome> {
        let UpsertParams {
            composite,
            tenant_id,
            endpoint,
            key,
            body_hash,
            response,
            now,
            expires_at,
        } = params;
        let tenant_uuid = tenant_id.raw();

        // Single round-trip upsert: INSERT ... ON CONFLICT DO UPDATE
        // (no-op update so we can fire RETURNING) RETURNING
        //   - `fresh` (xmax = 0): the row was newly inserted;
        //   - the stored `body_hash` and `response` for replay-vs-conflict
        //     detection on the conflict path.
        //
        // The `SET expires_at = idempotency.expires_at` is a deliberate
        // no-op — Postgres detects unchanged column values during
        // `ON CONFLICT DO UPDATE` and skips the physical heap update
        // (the "skip-update" optimization, PG 9.5+), so we pay only
        // the index lookup, not a full row rewrite. The point of the
        // SET is just to make `RETURNING` fire on the conflict path.
        //
        // This collapses the prior two-RT path
        // (INSERT ... ON CONFLICT DO NOTHING → SELECT) to one RT on
        // every conflict (the common steady-state case once the
        // idempotency table has warmed up).
        let row: Option<(bool, String, String, chrono::DateTime<chrono::Utc>)> = sqlx::query_as(
            r#"INSERT INTO idempotency
                 (composite_key, tenant_id, endpoint, idempotency_key,
                  body_hash, response, inserted_at, expires_at)
                VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
                ON CONFLICT (composite_key) DO UPDATE
                  SET expires_at = idempotency.expires_at
                RETURNING
                  (xmax = 0) AS fresh,
                  body_hash,
                  response,
                  expires_at"#,
        )
        .bind(composite)
        .bind(tenant_uuid)
        .bind(endpoint)
        .bind(key)
        .bind(body_hash)
        .bind(response)
        .bind(now)
        .bind(expires_at)
        .fetch_optional(&*self.pool)
        .await?;

        let Some((fresh, stored_hash, stored_response, stored_expires)) = row else {
            // Shouldn't happen with DO UPDATE — RETURNING always fires.
            // Fall back to lookup so we degrade gracefully rather than panic.
            return self.lookup(composite, body_hash, now).await;
        };

        if fresh {
            Ok(IdempotencyOutcome::Fresh)
        } else if stored_expires <= now {
            // The conflicting row has already expired. The DO UPDATE
            // above set `expires_at = idempotency.expires_at` (a no-op),
            // so the expired value is still on disk. Force-overwrite it
            // with our fresh data and return Fresh. This is the rare
            // case (rows age out within `ttl`; autovacuum eventually
            // cleans them), so the second RT here is acceptable.
            self.revive_expired(composite, body_hash, response, now, expires_at)
                .await
        } else if stored_hash == body_hash {
            Ok(IdempotencyOutcome::Replay(stored_response))
        } else {
            Ok(IdempotencyOutcome::Conflict)
        }
    }
}

#[derive(Debug)]
struct UpsertParams<'a> {
    composite: &'a str,
    tenant_id: TenantId,
    endpoint: &'a str,
    key: &'a str,
    body_hash: &'a str,
    response: &'a str,
    now: chrono::DateTime<chrono::Utc>,
    expires_at: chrono::DateTime<chrono::Utc>,
}

impl PostgresIdempotencyBackend {
    /// Force-overwrite an expired idempotency row with the fresh
    /// request data. Returns `Fresh` because, from the caller's
    /// perspective, this is a new write — no prior response exists
    /// that the caller could replay.
    ///
    /// The `WHERE expires_at <= $4` clause protects against a race
    /// where another worker revives the row between our upsert and
    /// this call: if zero rows are affected, we fall back to a plain
    /// lookup to discover replay-vs-conflict.
    async fn revive_expired(
        &self,
        composite: &str,
        body_hash: &str,
        response: &str,
        now: chrono::DateTime<chrono::Utc>,
        expires_at: chrono::DateTime<chrono::Utc>,
    ) -> sqlx::Result<IdempotencyOutcome> {
        let updated = sqlx::query(
            r#"UPDATE idempotency
                  SET body_hash = $2,
                      response = $3,
                      inserted_at = $4,
                      expires_at = $5
                WHERE composite_key = $1
                  AND expires_at <= $4"#,
        )
        .bind(composite)
        .bind(body_hash)
        .bind(response)
        .bind(now)
        .bind(expires_at)
        .execute(&*self.pool)
        .await?;

        if updated.rows_affected() == 1 {
            Ok(IdempotencyOutcome::Fresh)
        } else {
            // Lost the race — another worker revived it. Look it up
            // to determine replay vs conflict.
            self.lookup(composite, body_hash, now).await
        }
    }
}

fn composite_key(tenant: TenantId, endpoint: &str, key: &str) -> String {
    format!("{tenant}\u{0}{endpoint}\u{0}{key}")
}
