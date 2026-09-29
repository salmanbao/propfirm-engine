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

use async_trait::async_trait;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex;

use crate::api::idempotency::{hash_body, IdempotencyOutcome};
use crate::api::IdempotencyBackend;
use crate::persistence::redis_store::RedisConn;
use crate::tenant::TenantId;

/// Redis-backed idempotency backend (survives restarts, shared across replicas).
#[derive(Clone)]
pub struct RedisIdempotencyBackend {
    conn: RedisConn,
    ttl: Duration,
}

impl RedisIdempotencyBackend {
    #[must_use]
    pub fn new(conn: RedisConn, ttl: Duration) -> Self {
        RedisIdempotencyBackend { conn, ttl }
    }

    /// Get a clone of the underlying connection for mutation.
    /// `MultiplexedConnection` is `Clone`, so this is cheap.
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
        let conn = self.conn();
        let stored_hash: Option<String> = match conn {
            RedisConn::Single(mut c) => redis::cmd("GET").arg(hash_key).query_async(&mut c).await?,
            RedisConn::Cluster(arc) => {
                let mut c = arc.lock().await;
                redis::cmd("GET").arg(hash_key).query_async(&mut *c).await?
            }
        };
        match stored_hash {
            None => Ok(IdempotencyOutcome::Fresh),
            Some(stored) => {
                if stored == body_hash {
                    let response: Option<String> = match self.conn() {
                        RedisConn::Single(mut c) => {
                            redis::cmd("GET").arg(val_key).query_async(&mut c).await?
                        }
                        RedisConn::Cluster(arc) => {
                            let mut c = arc.lock().await;
                            redis::cmd("GET").arg(val_key).query_async(&mut *c).await?
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

        let script = r#"
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

        let conn = self.conn();
        let result: (String, String) = match conn {
            RedisConn::Single(mut c) => {
                redis::cmd("EVAL")
                    .arg(script)
                    .arg(2)
                    .arg(hash_key)
                    .arg(val_key)
                    .arg(body_hash)
                    .arg(response)
                    .arg(ttl_secs)
                    .query_async(&mut c)
                    .await?
            }
            RedisConn::Cluster(arc) => {
                let mut c = arc.lock().await;
                redis::cmd("EVAL")
                    .arg(script)
                    .arg(2)
                    .arg(hash_key)
                    .arg(val_key)
                    .arg(body_hash)
                    .arg(response)
                    .arg(ttl_secs)
                    .query_async(&mut *c)
                    .await?
            }
        };
        Ok(match result.0.as_str() {
            "fresh" => IdempotencyOutcome::Fresh,
            "replay" => IdempotencyOutcome::Replay(result.1),
            "conflict" => IdempotencyOutcome::Conflict,
            _ => IdempotencyOutcome::Error,
        })
    }
}

fn build_keys(tenant: TenantId, endpoint: &str, key: &str) -> (String, String) {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut h = DefaultHasher::new();
    endpoint.hash(&mut h);
    let endpoint_hash = format!("{:x}", h.finish());
    let key_trunc = if key.len() > 256 { &key[..256] } else { key };
    let base = format!("propfirm:idem:{tenant}:{endpoint_hash}:{key_trunc}");
    let val = format!("{base}:val");
    (base, val)
}
