//! Strict canonical codec for an authority-free boot-publication receipt body.

use sha2::{Digest as _, Sha256};
use thiserror::Error;

use super::{BootPublicationReceiptFingerprint, receipt_body::BootPublicationReceiptBody};

const BOOT_PUBLICATION_RECEIPT_FINGERPRINT_DOMAIN: &[u8] =
    b"os-tools/forge/boot-publication-receipt-body/v1\0";
pub(crate) const MAX_CANONICAL_BOOT_PUBLICATION_RECEIPT_BODY_BYTES: usize = 16 * 1024 * 1024;

/// One validated body together with its exact canonical bytes and identity.
///
/// This is data, not a publication proof or filesystem capability. Decoding a
/// body can establish byte-level self-consistency only.
#[derive(Debug, Eq, PartialEq)]
pub(crate) struct CanonicalBootPublicationReceipt {
    body: BootPublicationReceiptBody,
    canonical_body: Box<[u8]>,
    fingerprint: BootPublicationReceiptFingerprint,
}

impl CanonicalBootPublicationReceipt {
    pub(crate) const fn body(&self) -> &BootPublicationReceiptBody {
        &self.body
    }

    pub(crate) const fn canonical_body(&self) -> &[u8] {
        &self.canonical_body
    }

    pub(crate) const fn fingerprint(&self) -> BootPublicationReceiptFingerprint {
        self.fingerprint
    }
}

pub(crate) fn prepare_boot_publication_receipt(
    body: BootPublicationReceiptBody,
) -> Result<CanonicalBootPublicationReceipt, BootPublicationReceiptCodecError> {
    body.validate()?;
    let canonical_body = serde_json::to_vec(&body)?;
    enforce_body_size(canonical_body.len())?;
    Ok(canonical_receipt(body, canonical_body))
}

pub(crate) fn decode_boot_publication_receipt(
    canonical_body: &[u8],
) -> Result<CanonicalBootPublicationReceipt, BootPublicationReceiptCodecError> {
    enforce_body_size(canonical_body.len())?;
    let body: BootPublicationReceiptBody = serde_json::from_slice(canonical_body)?;
    body.validate()?;
    let reencoded = serde_json::to_vec(&body)?;
    if reencoded != canonical_body {
        return Err(BootPublicationReceiptCodecError::NonCanonicalBody);
    }
    Ok(canonical_receipt(body, reencoded))
}

fn canonical_receipt(body: BootPublicationReceiptBody, canonical_body: Vec<u8>) -> CanonicalBootPublicationReceipt {
    let fingerprint = fingerprint(&canonical_body);
    CanonicalBootPublicationReceipt {
        body,
        canonical_body: canonical_body.into_boxed_slice(),
        fingerprint,
    }
}

fn fingerprint(canonical_body: &[u8]) -> BootPublicationReceiptFingerprint {
    let mut hasher = Sha256::new();
    hasher.update(BOOT_PUBLICATION_RECEIPT_FINGERPRINT_DOMAIN);
    hasher.update(
        u64::try_from(canonical_body.len())
            .expect("the canonical receipt body bound fits in u64")
            .to_be_bytes(),
    );
    hasher.update(canonical_body);
    BootPublicationReceiptFingerprint::from_bytes(hasher.finalize().into())
}

fn enforce_body_size(size: usize) -> Result<(), BootPublicationReceiptCodecError> {
    if size > MAX_CANONICAL_BOOT_PUBLICATION_RECEIPT_BODY_BYTES {
        Err(BootPublicationReceiptCodecError::BodyTooLarge { actual: size })
    } else {
        Ok(())
    }
}

#[derive(Debug, Error)]
pub(crate) enum BootPublicationReceiptCodecError {
    #[error(
        "canonical boot-publication receipt body has {actual} bytes, exceeding limit {MAX_CANONICAL_BOOT_PUBLICATION_RECEIPT_BODY_BYTES}"
    )]
    BodyTooLarge { actual: usize },
    #[error("boot-publication receipt body is not strict JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Body(#[from] super::receipt_body::BootPublicationReceiptBodyError),
    #[error("boot-publication receipt body is not in its canonical byte encoding")]
    NonCanonicalBody,
}

#[cfg(test)]
#[path = "receipt_codec_tests.rs"]
mod tests;
