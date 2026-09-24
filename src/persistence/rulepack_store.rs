//! Rule-pack store (P0.7 fix).
//!
//! The rule-pack endpoints were previously stubs (404 / 501) while the
//! README advertised them. This module provides the storage trait and an
//! in-memory implementation, shaped like
//! [`crate::persistence::traits::AccountStore`], honouring the
//! [`PackLifecycle`](crate::rulepack::PackLifecycle) state machine:
//!
//! ```text
//! draft ──activate──▶ active ──supersede──▶ superseded
//!   ▲                      │
//!   └── update (draft only)
//! ```
//!
//! Illegal transitions (activating a superseded pack, updating an active
//! pack, re-activating an already-active pack) are rejected with a
//! conflict error the HTTP layer maps to **409**.

use crate::core::Error;
use crate::rulepack::RulePack;
use crate::tenant::TenantId;
use parking_lot::RwLock;
use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;

/// Storage trait for versioned rule packs (P0.7). Same structural shape
/// as [`crate::persistence::traits::AccountStore`]: `Send + Sync`,
/// tenant-scoped reads, typed errors.
#[async_trait]
pub trait RulePackStore: Send + Sync {
    /// Reads a pack by id. Tenant-scoped (P1-9): a pack belonging to a
    /// different tenant reads as not-found.
    async fn get_pack(&self, tenant_id: TenantId, id: &str) -> Result<Option<RulePack>, Error>;

    /// Inserts a pack. Fails with a conflict if the id already exists.
    async fn insert_pack(&self, pack: RulePack) -> Result<(), Error>;

    /// Replaces an existing pack (used for draft updates). Fails if the
    /// pack does not exist.
    async fn put_pack(&self, pack: RulePack) -> Result<(), Error>;

    /// Returns all packs for a tenant (newest version first).
    async fn list_packs(&self, tenant_id: TenantId) -> Result<Vec<RulePack>, Error>;
}

/// In-memory [`RulePackStore`] implementation (P0.7).
#[derive(Default, Clone)]
pub struct InMemoryRulePackStore {
    packs: Arc<RwLock<HashMap<String, RulePack>>>,
}

impl InMemoryRulePackStore {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl RulePackStore for InMemoryRulePackStore {
    async fn get_pack(&self, tenant_id: TenantId, id: &str) -> Result<Option<RulePack>, Error> {
        Ok(self
            .packs
            .read()
            .get(id)
            .filter(|p| p.tenant_id == tenant_id)
            .cloned())
    }

    async fn insert_pack(&self, pack: RulePack) -> Result<(), Error> {
        let mut w = self.packs.write();
        if w.contains_key(&pack.id) {
            return Err(Error::InvalidState(format!(
                "rule pack {} already exists",
                pack.id
            )));
        }
        w.insert(pack.id.clone(), pack);
        Ok(())
    }

    async fn put_pack(&self, pack: RulePack) -> Result<(), Error> {
        let mut w = self.packs.write();
        if !w.contains_key(&pack.id) {
            return Err(Error::NotFound(format!("rule pack {}", pack.id)));
        }
        w.insert(pack.id.clone(), pack);
        Ok(())
    }

    async fn list_packs(&self, tenant_id: TenantId) -> Result<Vec<RulePack>, Error> {
        let mut packs: Vec<RulePack> = self
            .packs
            .read()
            .values()
            .filter(|p| p.tenant_id == tenant_id)
            .cloned()
            .collect();
        packs.sort_by(|a, b| b.version.cmp(&a.version).then(a.id.cmp(&b.id)));
        Ok(packs)
    }
}

/// Lifecycle transition guard shared by the HTTP handlers (P0.7).
/// Returns the transition error that maps to HTTP 409.
pub fn check_transition(
    current: crate::rulepack::PackLifecycle,
    target: crate::rulepack::PackLifecycle,
) -> Result<(), Error> {
    use crate::rulepack::PackLifecycle::{Active, Draft, Superseded};
    let legal = match (current, target) {
        (Draft, Active) => true,          // activate
        (Active, Superseded) => true,     // supersede
        (Superseded, Superseded) => true, // idempotent re-supersede is a no-op
        _ => false,
    };
    if legal {
        Ok(())
    } else {
        Err(Error::InvalidState(format!(
            "illegal rule-pack lifecycle transition {current} → {target} \
             (allowed: draft→active, active→superseded)"
        )))
    }
}

/// Guard for draft edits (P0.7): only a pack still in `Draft` may be
/// updated. Any other state is an illegal transition → 409 upstream.
pub fn ensure_draft(current: crate::rulepack::PackLifecycle) -> Result<(), Error> {
    if current == crate::rulepack::PackLifecycle::Draft {
        Ok(())
    } else {
        Err(Error::InvalidState(format!(
            "rule pack may only be edited while draft (current state: {current})"
        )))
    }
}
