//! Real sha256 hashing for the engine (P0-B fix).
//!
//! Previously, `pure.rs::compute_input_hash` and `rulepack.rs::content_hash`
//! used `std::collections::hash_map::DefaultHasher` (`SipHash`) truncated to
//! 16 hex chars (64 bits), despite being labeled `sha256:`. The README
//! sells "byte-for-byte recomputation… the dispute-resolution mechanism"
//! on a mislabeled 64-bit non-crypto hash.
//!
//! This module uses the real `sha2` crate and produces the full 256-bit
//! (64-hex-char) digest. The `Hash` trait is used to feed canonical bytes
//! into the hasher, so types that implement `Hash` (most of our domain
//! types) can be hashed directly.

use sha2::{Digest, Sha256};
use std::hash::{Hash, Hasher};

/// Wrapper that bridges the `std::hash::Hash` API onto a real `Sha256`
/// hasher. Used by `input_hash()` and `content_hash()` so the existing
/// `foo.hash(&mut h)` calls feed bytes into a real crypto hash rather
/// than `SipHash`.
pub struct Sha256Hasher(Sha256);

impl Sha256Hasher {
    #[must_use]
    pub fn new() -> Self {
        Self(Sha256::new())
    }

    /// Consume the hasher and return the full 64-char lowercase hex digest.
    #[must_use]
    pub fn finalize_hex(self) -> String {
        let bytes = self.0.finalize();
        let mut s = String::with_capacity(64 + 7); // "sha256:" + 64
        s.push_str("sha256:");
        for b in bytes {
            s.push_str(&format!("{b:02x}"));
        }
        s
    }
}

impl Default for Sha256Hasher {
    fn default() -> Self {
        Self::new()
    }
}

impl Hasher for Sha256Hasher {
    fn write(&mut self, bytes: &[u8]) {
        // Feed raw bytes into the sha256 hasher.
        // We use `update` so the bytes are accumulated in order.
        self.0.update(bytes);
    }

    fn finish(&self) -> u64 {
        // We don't actually use `finish()` — callers should use
        // `finalize_hex()` instead. Return 0 as a placeholder; this
        // method exists only to satisfy the `Hasher` trait.
        0
    }
}

/// Convenience: hash any `Hash`-implementing value and return the
/// 64-char hex digest (prefixed with `sha256:`).
pub fn hash_to_hex<T: Hash>(value: &T) -> String {
    let mut h = Sha256Hasher::new();
    value.hash(&mut h);
    h.finalize_hex()
}

/// Convenience: hash raw bytes and return the 64-char hex digest.
#[must_use]
pub fn hash_bytes_to_hex(bytes: &[u8]) -> String {
    let mut h = Sha256Hasher::new();
    h.write(bytes);
    h.finalize_hex()
}
