//! Redis-backed idempotency backend.
//!
//! Uses an atomic Lua script for check-and-remember:
//!
//! ```text
//! if redis.call('EXISTS', KEYS[1]) == 1 then
//!     local stored = redis.call('GET', KEYS[1])
//!     if stored == ARGV[1] then
//!         return {'replay', redis.call('GET', KEYS[2]) or ''}
//!     else
//!         return {'conflict', ''}
//!     end
//! end
//! redis.call('SET', KEYS[1], ARGV[1], 'EX', ARGV[3], 'NX')
//! redis.call('SET', KEYS[2], ARGV[2], 'EX', ARGV[3])
//! return {'fresh', ''}
//! ```
//!
//! ## Key format
//!
//! `propfirm:idem:{tenant}:{endpoint_hash}:{key}` (hash stored at this key)
//! `propfirm:idem:{tenant}:{endpoint_hash}:{key}:val` (response stored here)
//!
//! ## Atomicity
//!
//! The Lua script is single-threaded inside Redis, so two concurrent workers
//! processing the same key cannot both win — one will see `fresh`, the other
//! will see `replay` or `conflict`.
//!
//! ## EVALSHA optimization
//!
//! The script body is loaded once via `SCRIPT LOAD` on first use and
//! cached as a SHA1 in [`RedisIdempotencyBackend::script_sha`].
//! Subsequent calls use `EVALSHA <sha>`, saving the script-body
//! bytes on every invocation (~300 bytes/call on the hot path). On
//! `NOSCRIPT` (Redis evicted the script from its LRU cache, e.g.
//! after a restart or `FLUSHALL`), the SHA is invalidated and the
//! script is reloaded transparently — callers see no behavioral
//! change.

use async_trait::async_trait;
use std::sync::Arc;
use std::time::Duration;

use crate::api::idempotency::{hash_body, IdempotencyOutcome};
use crate::api::IdempotencyBackend;
use crate::persistence::redis_store::RedisConn;
use crate::tenant::TenantId;

/// Redis-backed idempotency backend (survives restarts, shared across replicas).
///
/// The atomic check-and-remember Lua script is loaded once via
/// `SCRIPT LOAD` on first use; subsequent calls use `EVALSHA` with the
/// cached SHA1. This saves ~300 bytes/call on the hot path (the script
/// body is not re-sent). On a `NOSCRIPT` error (script was evicted from
/// Redis' cache, e.g. after a flush or restart) the cached SHA is
/// invalidated and the script is reloaded transparently.
#[derive(Clone)]
pub struct RedisIdempotencyBackend {
    conn: RedisConn,
    ttl: Duration,
    /// Cached SHA1 of [`IDEMPOTENCY_SCRIPT`]. `None` until the first
    /// `SCRIPT LOAD` succeeds. Protected by a `Mutex` so multiple
    /// concurrent calls don't race to load — but the race is harmless
    /// (`SCRIPT LOAD` is idempotent, so a duplicate load just returns
    /// the same SHA and the cache ends up with the same value).
    script_sha: Arc<parking_lot::Mutex<Option<String>>>,
}

impl RedisIdempotencyBackend {
    #[must_use]
    pub fn new(conn: RedisConn, ttl: Duration) -> Self {
        RedisIdempotencyBackend {
            conn,
            ttl,
            script_sha: Arc::new(parking_lot::Mutex::new(None)),
        }
    }

    /// Clone the underlying connection.
    #[must_use]
    fn conn(&self) -> RedisConn {
        self.conn.clone()
    }
}

#[async_trait]
impl IdempotencyBackend for RedisIdempotencyBackend {
    async fn check(
        &self,
        tenant_id: TenantId,
        endpoint: &str,
        key: &str,
        request_body: &str,
    ) -> IdempotencyOutcome {
        let (hash_key, val_key) = build_keys(tenant_id, endpoint, key);
        let body_hash = hash_body(request_body);
        match self.lookup(&hash_key, &val_key, &body_hash).await {
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
        let (hash_key, val_key) = build_keys(tenant_id, endpoint, key);
        let body_hash = hash_body(request_body);
        match self
            .atomic_check_and_remember(&hash_key, &val_key, &body_hash, response)
            .await
        {
            Ok(o) => o,
            Err(_) => IdempotencyOutcome::Error,
        }
    }

    /// Lua-script atomic check-and-remember.
    async fn check_and_remember(
        &self,
        tenant_id: TenantId,
        endpoint: &str,
        key: &str,
        request_body: &str,
        response: &str,
    ) -> IdempotencyOutcome {
        let (hash_key, val_key) = build_keys(tenant_id, endpoint, key);
        let body_hash = hash_body(request_body);
        match self
            .atomic_check_and_remember(&hash_key, &val_key, &body_hash, response)
            .await
        {
            Ok(o) => o,
            Err(_) => IdempotencyOutcome::Error,
        }
    }
}

impl RedisIdempotencyBackend {
    async fn lookup(
        &self,
        hash_key: &str,
        val_key: &str,
        body_hash: &str,
    ) -> redis::RedisResult<IdempotencyOutcome> {
        let stored_hash: Option<String> = match self.conn() {
            RedisConn::Single { producer, .. } => {
                redis::cmd("GET")
                    .arg(hash_key)
                    .query_async(&mut producer.clone())
                    .await?
            }
            RedisConn::Cluster(pool) => {
                let mut conn = pool.get().await.map_err(super::io_error_to_redis)?;
                redis::cmd("GET")
                    .arg(hash_key)
                    .query_async(&mut *conn)
                    .await?
            }
        };
        match stored_hash {
            None => Ok(IdempotencyOutcome::Fresh),
            Some(stored) => {
                if stored == body_hash {
                    let response: Option<String> = match self.conn() {
                        RedisConn::Single { producer, .. } => {
                            redis::cmd("GET")
                                .arg(val_key)
                                .query_async(&mut producer.clone())
                                .await?
                        }
                        RedisConn::Cluster(pool) => {
                            let mut conn = pool.get().await.map_err(super::io_error_to_redis)?;
                            redis::cmd("GET")
                                .arg(val_key)
                                .query_async(&mut *conn)
                                .await?
                        }
                    };
                    match response {
                        Some(resp) => Ok(IdempotencyOutcome::Replay(resp)),
                        None => Ok(IdempotencyOutcome::Fresh),
                    }
                } else {
                    Ok(IdempotencyOutcome::Conflict)
                }
            }
        }
    }

    async fn atomic_check_and_remember(
        &self,
        hash_key: &str,
        val_key: &str,
        body_hash: &str,
        response: &str,
    ) -> redis::RedisResult<IdempotencyOutcome> {
        let ttl_secs: i64 = self.ttl.as_secs().max(1).try_into().unwrap_or(86_400);

        // First try EVALSHA with the cached SHA (or load it lazily).
        let sha = self.get_or_load_script_sha().await?;
        let conn = self.conn();
        let result: redis::RedisResult<(String, String)> = match conn {
            RedisConn::Single { producer, .. } => {
                redis::cmd("EVALSHA")
                    .arg(&sha)
                    .arg(2)
                    .arg(hash_key)
                    .arg(val_key)
                    .arg(body_hash)
                    .arg(response)
                    .arg(ttl_secs)
                    .query_async(&mut producer.clone())
                    .await
            }
            RedisConn::Cluster(pool) => {
                let mut conn = pool.get().await.map_err(super::io_error_to_redis)?;
                redis::cmd("EVALSHA")
                    .arg(&sha)
                    .arg(2)
                    .arg(hash_key)
                    .arg(val_key)
                    .arg(body_hash)
                    .arg(response)
                    .arg(ttl_secs)
                    .query_async(&mut *conn)
                    .await
            }
        };

        // NOSCRIPT: Redis evicted the script from its LRU cache
        // (e.g. after a flushall, restart, or just enough script churn).
        // Invalidate our cached SHA, reload, and retry once.
        let result = match result {
            Err(ref e) if is_noscript(e) => {
                tracing::warn!("redis NOSCRIPT — reloading idempotency Lua script");
                {
                    let mut guard = self.script_sha.lock();
                    *guard = None;
                }
                let sha = self.get_or_load_script_sha().await?;
                let conn = self.conn();
                match conn {
                    RedisConn::Single { producer, .. } => {
                        redis::cmd("EVALSHA")
                            .arg(&sha)
                            .arg(2)
                            .arg(hash_key)
                            .arg(val_key)
                            .arg(body_hash)
                            .arg(response)
                            .arg(ttl_secs)
                            .query_async(&mut producer.clone())
                            .await
                    }
                    RedisConn::Cluster(pool) => {
                        let mut conn = pool.get().await.map_err(super::io_error_to_redis)?;
                        redis::cmd("EVALSHA")
                            .arg(&sha)
                            .arg(2)
                            .arg(hash_key)
                            .arg(val_key)
                            .arg(body_hash)
                            .arg(response)
                            .arg(ttl_secs)
                            .query_async(&mut *conn)
                            .await
                    }
                }
            }
            other => other,
        }?;

        Ok(match result.0.as_str() {
            "fresh" => IdempotencyOutcome::Fresh,
            "replay" => IdempotencyOutcome::Replay(result.1),
            "conflict" => IdempotencyOutcome::Conflict,
            _ => IdempotencyOutcome::Error,
        })
    }
}

/// The Lua script that does atomic check-and-remember for the
/// idempotency backend. Loaded via `SCRIPT LOAD` and invoked via
/// `EVALSHA` on the hot path.
const IDEMPOTENCY_SCRIPT: &str = r#"
    if redis.call('EXISTS', KEYS[1]) == 1 then
        local stored = redis.call('GET', KEYS[1])
        if stored == ARGV[1] then
            return {'replay', redis.call('GET', KEYS[2]) or ''}
        else
            return {'conflict', ''}
        end
    end
    redis.call('SET', KEYS[1], ARGV[1], 'EX', ARGV[3], 'NX')
    redis.call('SET', KEYS[2], ARGV[2], 'EX', ARGV[3])
    return {'fresh', ''}
"#;

impl RedisIdempotencyBackend {
    /// Return the cached SHA1 of [`IDEMPOTENCY_SCRIPT`], or load it
    /// via `SCRIPT LOAD` if not cached. Concurrent callers may race
    /// to load, but `SCRIPT LOAD` is idempotent (same script body
    /// → same SHA1), so the worst case is one redundant load.
    async fn get_or_load_script_sha(&self) -> redis::RedisResult<String> {
        {
            let guard = self.script_sha.lock();
            if let Some(sha) = guard.as_ref() {
                return Ok(sha.clone());
            }
        }
        let sha: String = match self.conn() {
            RedisConn::Single { producer, .. } => {
                redis::cmd("SCRIPT")
                    .arg("LOAD")
                    .arg(IDEMPOTENCY_SCRIPT)
                    .query_async(&mut producer.clone())
                    .await?
            }
            RedisConn::Cluster(pool) => {
                let mut conn = pool.get().await.map_err(super::io_error_to_redis)?;
                redis::cmd("SCRIPT")
                    .arg("LOAD")
                    .arg(IDEMPOTENCY_SCRIPT)
                    .query_async(&mut *conn)
                    .await?
            }
        };
        let mut guard = self.script_sha.lock();
        *guard = Some(sha.clone());
        Ok(sha)
    }
}

/// Detect a `NOSCRIPT` error from Redis. The redis crate doesn't
/// expose a typed enum for this, so we substring-match on the error
/// kind label (Redis returns `NOSCRIPT No matching script. Please use
/// EVAL.`). This matches what `redis-py`, `jedis`, and other clients
/// do.
fn is_noscript(e: &redis::RedisError) -> bool {
    // The redis crate sets `ErrorKind::ResponseError` with the
    // server's `NOSCRIPT` prefix in the message.
    e.to_string().contains("NOSCRIPT")
}

fn build_keys(tenant: TenantId, endpoint: &str, key: &str) -> (String, String) {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut h = DefaultHasher::new();
    endpoint.hash(&mut h);
    let endpoint_hash = format!("{:x}", h.finish());
    let key_trunc = if key.len() > 256 { &key[..256] } else { key };
    // Use Redis Cluster hash tags ({...}) on the tenant UUID so
    // both the hash key and the val key hash to the same cluster
    // slot. Without this, the Lua script that touches both keys
    // will fail with CROSSSLOT on Redis Cluster deployments.
    let base = format!("propfirm:idem:{{{tenant}}}:{endpoint_hash}:{key_trunc}");
    let val = format!("{base}:val");
    (base, val)
}
