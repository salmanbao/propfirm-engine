//! Tenant isolation (P1-9 fix).
//!
//! The platform this engine is meant to serve treats tenant isolation as
//! a correctness property with its own test suite, not an afterthought.
//! This module threads `TenantId` through the account aggregate, the rule
//! pack, and every persisted record so a single-engine deployment can
//! serve multiple prop firms (FunderBlu, future tenants) without
//! cross-tenant data leakage.

use uuid::Uuid;

/// Strongly-typed identifier for a tenant (prop firm) on the platform.
/// UUIDv4-based; same newtype pattern as other IDs in the crate.
#[cfg_attr(feature = "serialization", derive(serde::Serialize, serde::Deserialize))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TenantId(pub Uuid);

impl TenantId {
    /// Generates a fresh random tenant identifier.
    pub fn new() -> Self { TenantId(Uuid::new_v4()) }

    /// Constructs a tenant identifier from a raw UUID.
    pub const fn from_uuid(u: Uuid) -> Self { TenantId(u) }

    /// Returns the inner UUID.
    pub fn raw(self) -> Uuid { self.0 }

    /// Constructs a deterministic tenant id from a string name (stable
    /// across runs — useful for fixtures and tests).
    pub fn named(name: &str) -> Self {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        name.hash(&mut h);
        let bits = h.finish();
        let bytes = bits.to_be_bytes();
        let mut arr = [0u8; 16];
        arr[..8].copy_from_slice(&bytes);
        arr[8..].copy_from_slice(&bytes);
        arr[6] = (arr[6] & 0x0F) | 0x50;
        arr[8] = (arr[8] & 0x3F) | 0x80;
        TenantId(Uuid::from_bytes(arr))
    }
}

impl Default for TenantId {
    fn default() -> Self { TenantId::new() }
}

impl std::fmt::Display for TenantId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::str::FromStr for TenantId {
    type Err = uuid::Error;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Uuid::parse_str(s).map(TenantId)
    }
}
