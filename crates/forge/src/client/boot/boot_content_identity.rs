//! Cryptographic identity of the exact bytes retained for one boot output.
//!
//! This SHA-256 value is intentionally a distinct type. The Stone XXH3 digest
//! remains the non-cryptographic checksum used by checksum-addressed paths, and
//! future manifest fingerprints must not be interchangeable with either value.
//! Collision resistance binds exact bytes; it does not establish publisher
//! authenticity, signatures, or ownership provenance.

use sha2::{Digest as _, Sha256};

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub(in crate::client) struct BootContentIdentity([u8; 32]);

impl BootContentIdentity {
    pub(in crate::client) const EMPTY: Self = Self([
        0xe3, 0xb0, 0xc4, 0x42, 0x98, 0xfc, 0x1c, 0x14, 0x9a, 0xfb, 0xf4, 0xc8, 0x99, 0x6f, 0xb9, 0x24, 0x27, 0xae,
        0x41, 0xe4, 0x64, 0x9b, 0x93, 0x4c, 0xa4, 0x95, 0x99, 0x1b, 0x78, 0x52, 0xb8, 0x55,
    ]);

    pub(in crate::client) fn hash(bytes: &[u8]) -> Self {
        Self(Sha256::digest(bytes).into())
    }

    pub(in crate::client) const fn from_sha256(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    pub(in crate::client) const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}
