//! Idempotency store (P0.8 fix).
//!
//! The API previously read the `Idempotency-Key` header and discarded it
//! (`let _ = headers;`) while the routes module documented deduplication
//! as non-negotiable. This module provides a bounded LRU + TTL store:
//! the first response for `(endpoint, key)` is cached and replayed on
//! retries; a replay with a *conflicting* request body is rejected
//! (409) so a retry can never double-apply a different mutation.
//!
//! Implementation notes:
//! - bounded: at most `capacity` entries; the least-recently-used entry
//!   is evicted when full (classic LRU via an order-tracking `Vec`).
//! - TTL: entries older than `ttl` are treated as absent and pruned.
//! - the request body hash (sha256, via the crate's helper) is stored
//!   alongside the response so conflicting replays are detectable.

use crate::sha256_helper::Sha256Hasher;
use crate::tenant::TenantId;
use parking_lot::Mutex;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;

/// Outcome of an idempotency lookup.
pub enum IdempotencyOutcome {
    /// No prior request with this key — caller should execute and then
    /// [`IdempotencyStore::remember`] the response.
    Fresh,
    /// The same key + same body hash was seen before — replay the
    /// stored response without re-executing.
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
    /// [`remember`]; Postgres overrides this with a single upsert
    /// statement to prevent concurrent double-execution.
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

/// Bounded LRU + TTL idempotency store, keyed by `(endpoint, key)`.
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

    fn composite_key(endpoint: &str, key: &str) -> String {
        format!("{endpoint}\u{0}{key}")
    }

    /// Looks up `(endpoint, key)` against the given request body.
    pub fn check(&self, endpoint: &str, key: &str, request_body: &str) -> IdempotencyOutcome {
        let ckey = Self::composite_key(endpoint, key);
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

    /// Records the first response for `(endpoint, key)`. Bumps the key
    /// to most-recently-used and evicts the LRU entry when over capacity.
    pub fn remember(&self, endpoint: &str, key: &str, request_body: &str, response: &str) {
        let ckey = Self::composite_key(endpoint, key);
        let body_hash = hash_body(request_body);
        let mut inner = self.inner.lock();
        // LRU bump: remove from current position, push to back.
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
        // Evict while over capacity.
        while inner.order.len() > self.capacity {
            let evicted = inner.order.remove(0);
            inner.entries.remove(&evicted);
        }
    }

    /// Removes expired entries. Called lazily on `check`.
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
        let ckey = format!("{tenant_id}\u{0}{endpoint}\u{0}{key}");
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
        let ckey = format!("{tenant_id}\u{0}{endpoint}\u{0}{key}");
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
}

/// sha256 of the request body (hex) — used to detect conflicting replays.
fn hash_body(body: &str) -> String {
    use std::hash::Hash;
    let mut h = Sha256Hasher::new();
    body.hash(&mut h);
    h.finalize_hex()
}
