use std::io;

use super::{parser::SelectedEntry, reader::Operation};

/// Exact bounded bytes and semantics retained from one accepted GPT pass.
///
/// This type never leaves the pure parser module.  Callers receive only the
/// selected scalar facts and a fingerprint after two snapshots compare equal.
#[derive(Debug, Eq, PartialEq)]
pub(super) struct AuthenticatedGptTableSnapshot {
    image_bytes: u64,
    logical_block_size: u32,
    pmbr_block: Vec<u8>,
    primary_header_block: Vec<u8>,
    backup_header_block: Vec<u8>,
    entry_array: Vec<u8>,
    selected: SelectedEntry,
}

impl AuthenticatedGptTableSnapshot {
    #[allow(clippy::too_many_arguments)]
    pub(super) fn new(
        image_bytes: u64,
        logical_block_size: u32,
        pmbr_block: Vec<u8>,
        primary_header_block: Vec<u8>,
        backup_header_block: Vec<u8>,
        entry_array: Vec<u8>,
        selected: SelectedEntry,
    ) -> Self {
        Self {
            image_bytes,
            logical_block_size,
            pmbr_block,
            primary_header_block,
            backup_header_block,
            entry_array,
            selected,
        }
    }

    pub(super) const fn image_bytes(&self) -> u64 {
        self.image_bytes
    }

    pub(super) const fn logical_block_size(&self) -> u32 {
        self.logical_block_size
    }

    pub(super) fn pmbr_block(&self) -> &[u8] {
        &self.pmbr_block
    }

    pub(super) fn primary_header_block(&self) -> &[u8] {
        &self.primary_header_block
    }

    pub(super) fn backup_header_block(&self) -> &[u8] {
        &self.backup_header_block
    }

    pub(super) fn entry_array(&self) -> &[u8] {
        &self.entry_array
    }

    pub(super) const fn selected(&self) -> SelectedEntry {
        self.selected
    }

    /// Compare every retained scalar and byte under the shared operation
    /// ledger.  A selected entry remaining unchanged is deliberately not
    /// enough: unrelated GPT metadata must also be stable.
    pub(super) fn require_exact_match(&self, other: &Self, operation: &mut Operation<'_>) -> io::Result<()> {
        operation.charge_work(1, "comparing GPT snapshot scalar fields")?;
        if self.image_bytes != other.image_bytes
            || self.logical_block_size != other.logical_block_size
            || self.selected != other.selected
        {
            return Err(changed());
        }

        for (first, second) in [
            (self.pmbr_block(), other.pmbr_block()),
            (self.primary_header_block(), other.primary_header_block()),
            (self.backup_header_block(), other.backup_header_block()),
            (self.entry_array(), other.entry_array()),
        ] {
            if first.len() != second.len() {
                return Err(changed());
            }
            for (left, right) in first.chunks(64 * 1024).zip(second.chunks(64 * 1024)) {
                operation.charge_work(left.len(), "comparing complete GPT table snapshots")?;
                if left != right {
                    return Err(changed());
                }
            }
        }
        operation.checkpoint()
    }
}

fn changed() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        "GPT table changed between authenticated passes",
    )
}
