//! Strongly-typed identifiers used throughout the engine.
//!
//! All IDs are UUIDv4-based newtypes to prevent mixing up an `OrderId` with a
//! `TradeId`, etc. They are cheap to clone (16 bytes) and implement
//! `Display`, `Hash`, `Eq`, `PartialEq`.

use uuid::Uuid;

macro_rules! id_newtype {
    ($(#[$meta:meta])* $name:ident) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
        pub struct $name(pub Uuid);

        impl $name {
            /// Generates a fresh random identifier.
            pub fn new() -> Self { $name(Uuid::new_v4()) }

            /// Constructs an identifier from a raw UUID.
            pub const fn from_uuid(u: Uuid) -> Self { $name(u) }

            /// Returns the inner UUID.
            pub fn raw(self) -> Uuid { self.0 }
        }

        impl Default for $name {
            fn default() -> Self { $name::new() }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                write!(f, "{}", self.0)
            }
        }

        impl std::str::FromStr for $name {
            type Err = uuid::Error;
            fn from_str(s: &str) -> Result<Self, Self::Err> {
                Uuid::parse_str(s).map($name)
            }
        }

        impl From<Uuid> for $name {
            fn from(u: Uuid) -> Self { $name(u) }
        }

        impl From<$name> for Uuid {
            fn from(id: $name) -> Self { id.0 }
        }

        impl serde::Serialize for $name {
            fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
                self.0.serialize(s)
            }
        }

        impl<'de> serde::Deserialize<'de> for $name {
            fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
                Uuid::deserialize(d).map($name)
            }
        }
    };
}

id_newtype!(
    /// Uniquely identifies a trader account.
    AccountId
);
id_newtype!(
    /// Uniquely identifies an order submitted to the market.
    OrderId
);
id_newtype!(
    /// Uniquely identifies a fill (a single execution against an order).
    TradeId
);
id_newtype!(
    /// Uniquely identifies an open or closed position.
    PositionId
);
id_newtype!(
    /// Uniquely identifies a rule (useful for custom rule extensions).
    RuleId
);
id_newtype!(
    /// Uniquely identifies a challenge plan definition.
    ChallengeId
);
id_newtype!(
    /// Uniquely identifies an event in the audit log.
    EventId
);
id_newtype!(
    /// Uniquely identifies a rule violation record.
    ViolationId
);
id_newtype!(
    /// Uniquely identifies a session (used by the API server).
    SessionId
);

impl RuleId {
    /// Constructs a deterministic rule id from a string name (stable across
    /// runs, useful for configuration references).
    pub fn named(name: &str) -> Self {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        name.hash(&mut h);
        let bits = h.finish();
        let bytes = bits.to_be_bytes();
        let mut arr = [0u8; 16];
        arr[..8].copy_from_slice(&bytes);
        arr[8..].copy_from_slice(&bytes);
        // Set RFC 4122 v5-ish bits for stability
        arr[6] = (arr[6] & 0x0F) | 0x50;
        arr[8] = (arr[8] & 0x3F) | 0x80;
        RuleId(Uuid::from_bytes(arr))
    }
}
