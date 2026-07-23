use std::io;

use sha2::{Digest as _, Sha256};

use super::{reader::Operation, snapshot::AuthenticatedGptTableSnapshot};

/// Domain for the canonical, role-independent accepted-table encoding.
const TABLE_FINGERPRINT_DOMAIN: &[u8] = b"os-tools/forge/authenticated-gpt-table/v1\0";
const HASH_CHUNK_BYTES: usize = 64 * 1024;

/// Fingerprint the complete accepted GPT table without retaining its bytes in
/// the scalar evidence.  Selected-role semantics are intentionally excluded:
/// two roles selected from one exact table must share this table identity.
pub(super) fn table_sha256(
    snapshot: &AuthenticatedGptTableSnapshot,
    operation: &mut Operation<'_>,
) -> io::Result<[u8; 32]> {
    let mut hasher = Sha256::new();
    update_field(&mut hasher, b"domain", TABLE_FINGERPRINT_DOMAIN, operation)?;
    update_field(
        &mut hasher,
        b"image-bytes",
        &snapshot.image_bytes().to_le_bytes(),
        operation,
    )?;
    update_field(
        &mut hasher,
        b"logical-block-size",
        &snapshot.logical_block_size().to_le_bytes(),
        operation,
    )?;
    update_field(&mut hasher, b"pmbr-block", snapshot.pmbr_block(), operation)?;
    update_field(
        &mut hasher,
        b"primary-header-block",
        snapshot.primary_header_block(),
        operation,
    )?;
    update_field(
        &mut hasher,
        b"backup-header-block",
        snapshot.backup_header_block(),
        operation,
    )?;
    update_field(&mut hasher, b"entry-array", snapshot.entry_array(), operation)?;
    operation.checkpoint()?;
    Ok(hasher.finalize().into())
}

fn update_field(hasher: &mut Sha256, name: &[u8], bytes: &[u8], operation: &mut Operation<'_>) -> io::Result<()> {
    let name_length: u64 = name
        .len()
        .try_into()
        .map_err(|_| invalid_data("GPT fingerprint field-name length is not representable"))?;
    let value_length: u64 = bytes
        .len()
        .try_into()
        .map_err(|_| invalid_data("GPT fingerprint field length is not representable"))?;
    operation.charge_work(
        name.len()
            .checked_add(16)
            .ok_or_else(|| invalid_data("GPT fingerprint framing work overflowed"))?,
        "framing the GPT table fingerprint",
    )?;
    hasher.update(name_length.to_le_bytes());
    hasher.update(name);
    hasher.update(value_length.to_le_bytes());
    for chunk in bytes.chunks(HASH_CHUNK_BYTES) {
        operation.charge_work(chunk.len(), "hashing the complete GPT table snapshot")?;
        hasher.update(chunk);
    }
    Ok(())
}

fn invalid_data(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}
