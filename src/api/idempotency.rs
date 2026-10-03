//! Idempotency backend trait + in-memory implementation.
//!
//!
//! ## Why idempotency is still here even without auth
//!
//! The platform backend calls `/internal/v1/evaluate` over the private
//! network. Network retries still happen — TCP RSTs, pod evictions, client
//! timeouts. Without idempotency, a retried `emergency-stop` request could
//! double-apply; a retried `override` could clear an unrelated later breach.
//! Idempotency is a correctness property, not a security property, so it
//! stays even though authentication has been removed.

use crate::sha256_helper::Sha256Hasher;
use crate::tenant::TenantId;
use parking_lot::Mutex;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;

/// Outcome of an idempotency lookup.
#[derive(Debug, Clone)]
pub enum IdempotencyOutcome {
    /// No prior request with this key — caller should execute and then
    /// [`IdempotencyBackend::remember`] the response.
    Fresh,
    /// The same key + same body hash was seen before — replay the stored
    /// response without re-executing.
    Replay(String),
    /// The same key was used with a *different* body — a conflicting
    /// retry. Reject (HTTP 409), never double-apply.
    Conflict,
    /// The backend could not safely determine the outcome.
    Error,
}

/// Backend contract for idempotency storage.
///
/// Implementations must scope results by `tenant_id` so that a retry
/// key from one tenant can never replay or conflict with another tenant's
/// request.
#[async_trait]
pub trait IdempotencyBackend: Send + Sync {
    /// Look up a prior request. Returns [`IdempotencyOutcome`].
    async fn check(
        &self,
        tenant_id: TenantId,
        endpoint: &str,
        key: &str,
        request_body: &str,
    ) -> IdempotencyOutcome;

    /// Record a successful response for later replay.
    async fn remember(
        &self,
        tenant_id: TenantId,
        endpoint: &str,
        key: &str,
        request_body: &str,
        response: &str,
    ) -> IdempotencyOutcome;

    /// Atomically check for an existing entry and, if absent, remember
    /// the response. Default implementation delegates to [`check`] +
    /// [`remember`]; implementations backed by a durable store should
    /// override this with a single conditional upsert to prevent
    /// concurrent double-execution.
    async fn check_and_remember(
        &self,
        tenant_id: TenantId,
        endpoint: &str,
        key: &str,
        request_body: &str,
        response: &str,
    ) -> IdempotencyOutcome {
        match self.check(tenant_id, endpoint, key, request_body).await {
            IdempotencyOutcome::Fresh => {
                self.remember(tenant_id, endpoint, key, request_body, response)
                    .await;
                IdempotencyOutcome::Fresh
            }
            other => other,
        }
    }
}

/// The cached record: request-body hash + serialized first response +
/// insertion time (for TTL).
#[derive(Clone)]
struct Entry {
    body_hash: String,
    response: String,
    inserted_at: Instant,
}

/// Bounded LRU + TTL idempotency store, keyed by `(tenant, endpoint, key)`.
///
/// This is the **default** backend (no I/O, no setup). For production use
/// `PostgresIdempotencyBackend` or `RedisIdempotencyBackend` from
#[derive(Clone)]
pub struct IdempotencyStore {
    inner: Arc<Mutex<Inner>>,
    capacity: usize,
    ttl: Duration,
}

struct Inner {
    /// Insertion-ordered map: `key -> Entry`. The Vec preserves LRU
    /// order (front = least recent).
    entries: HashMap<String, Entry>,
    order: Vec<String>,
}

impl IdempotencyStore {
    /// Creates a store with the given capacity and TTL.
    #[must_use]
    pub fn new(capacity: usize, ttl: Duration) -> Self {
        IdempotencyStore {
            inner: Arc::new(Mutex::new(Inner {
                entries: HashMap::new(),
                order: Vec::new(),
            })),
            capacity,
            ttl,
        }
    }

    /// Default production settings: 10,000 keys, 24h TTL.
    #[must_use]
    pub fn with_defaults() -> Self {
        Self::new(10_000, Duration::from_secs(24 * 60 * 60))
    }

    /// Capacity override.
    #[must_use]
    pub fn with_capacity_ttl(capacity: usize, ttl: Duration) -> Self {
        Self::new(capacity, ttl)
    }

    /// Looks up `(endpoint, key)` against the given request body.
    /// Convenience wrapper for tests — delegates to the trait method
    /// with a default tenant.
    pub fn check_now(&self, endpoint: &str, key: &str, request_body: &str) -> IdempotencyOutcome {
        let ckey = composite_key(None, endpoint, key);
        let body_hash = hash_body(request_body);
        let mut inner = self.inner.lock();
        Self::prune(&mut inner, self.ttl);
        match inner.entries.get(&ckey) {
            Some(entry) => {
                if entry.body_hash == body_hash {
                    IdempotencyOutcome::Replay(entry.response.clone())
                } else {
                    IdempotencyOutcome::Conflict
                }
            }
            None => IdempotencyOutcome::Fresh,
        }
    }

    /// Records the first response for `(endpoint, key)`.
    pub fn remember_now(&self, endpoint: &str, key: &str, request_body: &str, response: &str) {
        let ckey = composite_key(None, endpoint, key);
        let body_hash = hash_body(request_body);
        let mut inner = self.inner.lock();
        inner.order.retain(|k| k != &ckey);
        inner.order.push(ckey.clone());
        inner.entries.insert(
            ckey,
            Entry {
                body_hash,
                response: response.to_string(),
                inserted_at: Instant::now(),
            },
        );
        while inner.order.len() > self.capacity {
            let evicted = inner.order.remove(0);
            inner.entries.remove(&evicted);
        }
    }

    fn prune(inner: &mut Inner, ttl: Duration) {
        let now = Instant::now();
        let expired: Vec<String> = inner
            .entries
            .iter()
            .filter(|(_, e)| now.duration_since(e.inserted_at) > ttl)
            .map(|(k, _)| k.clone())
            .collect();
        for k in expired {
            inner.entries.remove(&k);
            inner.order.retain(|o| o != &k);
        }
    }

    /// Number of live (non-expired) entries — used by tests and metrics.
    #[must_use]
    pub fn len(&self) -> usize {
        let mut inner = self.inner.lock();
        Self::prune(&mut inner, self.ttl);
        inner.entries.len()
    }

    /// True when the store holds no live entries.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[async_trait]
impl IdempotencyBackend for IdempotencyStore {
    async fn check(
        &self,
        tenant_id: TenantId,
        endpoint: &str,
        key: &str,
        request_body: &str,
    ) -> IdempotencyOutcome {
        let ckey = composite_key(Some(tenant_id), endpoint, key);
        let body_hash = hash_body(request_body);
        let mut inner = self.inner.lock();
        Self::prune(&mut inner, self.ttl);
        match inner.entries.get(&ckey) {
            Some(entry) => {
                if entry.body_hash == body_hash {
                    IdempotencyOutcome::Replay(entry.response.clone())
                } else {
                    IdempotencyOutcome::Conflict
                }
            }
            None => IdempotencyOutcome::Fresh,
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
        let ckey = composite_key(Some(tenant_id), endpoint, key);
        let body_hash = hash_body(request_body);
        let mut inner = self.inner.lock();
        inner.order.retain(|k| k != &ckey);
        inner.order.push(ckey.clone());
        inner.entries.insert(
            ckey,
            Entry {
                body_hash,
                response: response.to_string(),
                inserted_at: Instant::now(),
            },
        );
        while inner.order.len() > self.capacity {
            let evicted = inner.order.remove(0);
            inner.entries.remove(&evicted);
        }
        IdempotencyOutcome::Fresh
    }

    /// Override the default to be truly atomic under the in-process lock.
    async fn check_and_remember(
        &self,
        tenant_id: TenantId,
        endpoint: &str,
        key: &str,
        request_body: &str,
        response: &str,
    ) -> IdempotencyOutcome {
        let ckey = composite_key(Some(tenant_id), endpoint, key);
        let body_hash = hash_body(request_body);
        let mut inner = self.inner.lock();
        Self::prune(&mut inner, self.ttl);
        match inner.entries.get(&ckey) {
            Some(entry) => {
                if entry.body_hash == body_hash {
                    IdempotencyOutcome::Replay(entry.response.clone())
                } else {
                    IdempotencyOutcome::Conflict
                }
            }
            None => {
                inner.order.retain(|k| k != &ckey);
                inner.order.push(ckey.clone());
                inner.entries.insert(
                    ckey,
                    Entry {
                        body_hash,
                        response: response.to_string(),
                        inserted_at: Instant::now(),
                    },
                );
                while inner.order.len() > self.capacity {
                    let evicted = inner.order.remove(0);
                    inner.entries.remove(&evicted);
                }
                IdempotencyOutcome::Fresh
            }
        }
    }
}

/// Build the composite key with optional tenant scoping.
fn composite_key(tenant: Option<TenantId>, endpoint: &str, key: &str) -> String {
    match tenant {
        Some(t) => format!("{t}\u{0}{endpoint}\u{0}{key}"),
        None => format!("{endpoint}\u{0}{key}"),
    }
}

/// sha256 of the request body (hex) — used to detect conflicting replays.
pub fn hash_body(body: &str) -> String {
    use std::hash::Hash;
    let mut h = Sha256Hasher::new();
    body.hash(&mut h);
    h.finalize_hex()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::idempotency::IdempotencyBackend;
    use crate::tenant::TenantId;

    #[tokio::test]
    async fn fresh_then_replay_then_conflict() {
        let store = IdempotencyStore::with_defaults();
        let tenant = TenantId::named("test");
        let endpoint = "POST /internal/v1/evaluate";
        let key = "abc-123";
        let body_a = "{\"x\":1}";
        let body_b = "{\"x\":2}";
        let resp = "{\"ok\":true}";

        // First call — Fresh.
        let outcome = store
            .check_and_remember(tenant, endpoint, key, body_a, resp)
            .await;
        assert!(matches!(outcome, IdempotencyOutcome::Fresh));

        // Replay same body — Replay.
        let outcome = IdempotencyBackend::check(&store, tenant, endpoint, key, body_a).await;
        match outcome {
            IdempotencyOutcome::Replay(cached) => assert_eq!(cached, resp),
            _ => panic!("expected Replay"),
        }

        // Same key, different body — Conflict.
        let outcome = IdempotencyBackend::check(&store, tenant, endpoint, key, body_b).await;
        assert!(matches!(outcome, IdempotencyOutcome::Conflict));
    }
}
