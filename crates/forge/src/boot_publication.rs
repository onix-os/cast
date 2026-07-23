//! Shared durable identities for authenticated boot publication.
//!
//! A fingerprint identifies a complete receipt stored elsewhere. It carries no
//! filesystem descriptor, mutation capability, or deletion authority. The
//! journal stores only the committed/pending correlation pair; the state
//! database remains responsible for the corresponding durable receipt state.

use std::fmt;

use serde::{Deserialize, Deserializer, Serialize, Serializer, de};
use thiserror::Error;

#[path = "boot_publication/receipt_body.rs"]
mod receipt_body;
#[path = "boot_publication/receipt_codec.rs"]
mod receipt_codec;

#[allow(unused_imports)] // shared with the client receipt mapper
pub(crate) use receipt_body::{
    BootPublicationDestination, BootPublicationDestinations, BootPublicationHistoricalRuntimeWitness,
    BootPublicationOutput, BootPublicationOutputProvenanceClaim, BootPublicationOutputRole,
    BootPublicationPublicationPhase, BootPublicationReceiptBody, BootPublicationReceiptBodyError,
    BootPublicationRoot, BootPublicationSha256, BootPublicationXxh3,
    MAX_BOOT_PUBLICATION_RECEIPT_OUTPUTS,
};
#[allow(unused_imports)] // shared with durable receipt storage
pub(crate) use receipt_codec::{
    BootPublicationReceiptCodecError, CanonicalBootPublicationReceipt, decode_boot_publication_receipt,
    prepare_boot_publication_receipt, MAX_CANONICAL_BOOT_PUBLICATION_RECEIPT_BODY_BYTES,
};

const FINGERPRINT_BYTES: usize = 32;
const FINGERPRINT_TEXT_BYTES: usize = FINGERPRINT_BYTES * 2;

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub(crate) struct BootPublicationReceiptFingerprint([u8; FINGERPRINT_BYTES]);

impl BootPublicationReceiptFingerprint {
    pub(crate) const fn from_bytes(bytes: [u8; FINGERPRINT_BYTES]) -> Self {
        Self(bytes)
    }

    pub(crate) const fn as_bytes(&self) -> &[u8; FINGERPRINT_BYTES] {
        &self.0
    }

    pub(crate) fn from_slice(bytes: &[u8]) -> Result<Self, BootPublicationReceiptFingerprintError> {
        let bytes: [u8; FINGERPRINT_BYTES] = bytes
            .try_into()
            .map_err(|_| BootPublicationReceiptFingerprintError::InvalidBinaryLength(bytes.len()))?;
        Ok(Self(bytes))
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct BootPublicationReceiptPair {
    pub(crate) committed: Option<BootPublicationReceiptFingerprint>,
    pub(crate) pending: BootPublicationReceiptFingerprint,
}

impl Serialize for BootPublicationReceiptFingerprint {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut encoded = [0_u8; FINGERPRINT_TEXT_BYTES];
        for (index, byte) in self.0.iter().copied().enumerate() {
            encoded[index * 2] = encode_nibble(byte >> 4);
            encoded[index * 2 + 1] = encode_nibble(byte & 0x0f);
        }
        let encoded = std::str::from_utf8(&encoded).expect("lowercase hexadecimal is valid UTF-8");
        serializer.serialize_str(encoded)
    }
}

impl<'de> Deserialize<'de> for BootPublicationReceiptFingerprint {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct FingerprintVisitor;

        impl<'de> de::Visitor<'de> for FingerprintVisitor {
            type Value = BootPublicationReceiptFingerprint;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("exactly 64 lowercase hexadecimal characters")
            }

            fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
            where
                E: de::Error,
            {
                decode_fingerprint(value).map_err(E::custom)
            }
        }

        deserializer.deserialize_str(FingerprintVisitor)
    }
}

fn encode_nibble(nibble: u8) -> u8 {
    match nibble {
        0..=9 => b'0' + nibble,
        10..=15 => b'a' + (nibble - 10),
        _ => unreachable!("a four-bit nibble is at most fifteen"),
    }
}

fn decode_fingerprint(
    value: &str,
) -> Result<BootPublicationReceiptFingerprint, BootPublicationReceiptFingerprintError> {
    let encoded = value.as_bytes();
    if encoded.len() != FINGERPRINT_TEXT_BYTES {
        return Err(BootPublicationReceiptFingerprintError::InvalidText);
    }
    let mut bytes = [0_u8; FINGERPRINT_BYTES];
    for (index, pair) in encoded.chunks_exact(2).enumerate() {
        let high = decode_nibble(pair[0]).ok_or(BootPublicationReceiptFingerprintError::InvalidText)?;
        let low = decode_nibble(pair[1]).ok_or(BootPublicationReceiptFingerprintError::InvalidText)?;
        bytes[index] = (high << 4) | low;
    }
    Ok(BootPublicationReceiptFingerprint::from_bytes(bytes))
}

fn decode_nibble(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        _ => None,
    }
}

#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub(crate) enum BootPublicationReceiptFingerprintError {
    #[error("boot-publication receipt fingerprint must contain 32 bytes, got {0}")]
    InvalidBinaryLength(usize),
    #[error("boot-publication receipt fingerprint must be exactly 64 lowercase hexadecimal characters")]
    InvalidText,
}

#[cfg(test)]
mod tests;
