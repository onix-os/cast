//! Bounded durability proof for a retained candidate tree before journaling.
//!
//! The walk is Linux-only, iterative, raw-byte preserving, and rooted in one
//! retained directory descriptor. It never follows a symlink or crosses a
//! mount. A successful seal proves that every ordinary inode and directory
//! entry survived a bottom-up durability barrier and a second whole-tree
//! comparison before the tree marker is published.
//!
//! This is a stable-inventory proof for a private wrapper with cooperating
//! writers, not a kernel freeze against an uncooperative same-UID process
//! performing inode-reuse or create/delete ABA. The retained root is exact,
//! but its parent name is proved by the surrounding transition namespace.
//! When a marker is newly published, only root size/mtime/ctime may differ;
//! strong retained marker and state-ID checks must sandwich that comparison.
//!
//! Root, regular-file, directory, and canonical-marker descriptors must carry
//! no xattrs. Symlinks remain opaque `O_PATH | O_NOFOLLOW` inodes: Linux 5.6
//! cannot `flistxattr` those descriptors, while reopening a procfs or mutable
//! pathname would violate the no-follow authority model. Linux user and file-
//! capability xattrs are inapplicable to symlinks; security-label symlink
//! xattrs are unsupported by and outside the canonical package model rather
//! than proven absent here. Raw targets and ctime remain witnessed, and this
//! module deliberately provides no unsafe fallback.

mod durability;
mod error;
mod filesystem;
mod inventory;

#[cfg(test)]
mod tests;

use std::{
    fs::File,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

pub(crate) use error::{CandidateInventoryBoundary, CandidateInventoryError};

use durability::{sync_baseline, sync_marker_delta};
use inventory::{Inventory, MarkerPolicy, collect_inventory};

#[derive(Clone, Copy, Debug)]
pub(crate) struct CandidateInventoryLimits {
    /// Non-marker descendants; the retained root and fixed root marker are exempt.
    pub(crate) entries: usize,
    /// Root-relative descendant depth; the retained root has depth zero.
    pub(crate) depth: usize,
    /// Non-marker entry names plus raw symlink targets; nested marker-like names count.
    pub(crate) name_bytes: usize,
    /// Regular payload bytes excluding the root marker's authenticated frame.
    pub(crate) regular_bytes: u64,
    pub(crate) operations: usize,
    pub(crate) time: Duration,
}

impl Default for CandidateInventoryLimits {
    fn default() -> Self {
        Self {
            entries: 1_000_000,
            depth: 128,
            name_bytes: 64 * 1024 * 1024,
            regular_bytes: 64_u64 * 1024 * 1024 * 1024,
            operations: 16_000_000,
            time: Duration::from_secs(120),
        }
    }
}

/// Exact retained candidate proof captured before tree-marker publication.
///
/// The retained root remains the sole traversal authority. The diagnostic
/// path is never reopened and may become stale without weakening the seal.
#[derive(Debug)]
pub(crate) struct RetainedCandidateDurabilitySeal {
    root: File,
    display_path: PathBuf,
    baseline: Inventory,
    limits: CandidateInventoryLimits,
}

impl RetainedCandidateDurabilitySeal {
    /// Inventory and durably seal a candidate before creating or adopting its marker.
    pub(crate) fn seal_before_marker(
        root: &File,
        display_path: impl Into<PathBuf>,
        limits: CandidateInventoryLimits,
    ) -> Result<Self, CandidateInventoryError> {
        let display_path = display_path.into();
        let mut budget = WorkBudget::new(limits, &display_path)?;
        budget.operation(&display_path)?;
        let root = root
            .try_clone()
            .map_err(|source| error::inventory_io("retain exact candidate root", &display_path, source))?;

        let baseline = collect_inventory(&root, &display_path, limits, MarkerPolicy::Classify, &mut budget)?;
        sync_baseline(&root, &display_path, &baseline, limits, &mut budget)?;
        let after_sync = collect_inventory(&root, &display_path, limits, MarkerPolicy::Classify, &mut budget)?;
        baseline.require_exact(&after_sync, &display_path, &mut budget)?;

        Ok(Self {
            root,
            display_path,
            baseline,
            limits,
        })
    }

    /// Validate the sole canonical marker delta after marker publication.
    ///
    /// This method authenticates the delta's namespace shape and exact inode
    /// stability, but marker framing and its state-ID correlation belong to
    /// the retained marker/identity guard. The coordinator must sandwich this
    /// call between strong revalidations of that exact retained marker and
    /// state ID. That ordering binds the sole scanned marker to the marker
    /// returned by preparation without exposing marker internals here.
    pub(crate) fn validate_after_marker(&self) -> Result<(), CandidateInventoryError> {
        let mut budget = WorkBudget::new(self.limits, &self.display_path)?;
        let after_publication = collect_inventory(
            &self.root,
            &self.display_path,
            self.limits,
            MarkerPolicy::MustBePresent,
            &mut budget,
        )?;
        self.baseline
            .require_marker_delta(&after_publication, &self.display_path, &mut budget)?;

        sync_marker_delta(
            &self.root,
            &self.display_path,
            &after_publication,
            self.limits,
            &mut budget,
        )?;
        let after_resync = collect_inventory(
            &self.root,
            &self.display_path,
            self.limits,
            MarkerPolicy::MustBePresent,
            &mut budget,
        )?;
        after_publication.require_exact(&after_resync, &self.display_path, &mut budget)?;
        self.baseline
            .require_marker_delta(&after_resync, &self.display_path, &mut budget)
    }

    #[cfg(test)]
    fn baseline_entry_count(&self) -> usize {
        self.baseline.entry_count()
    }
}

/// One aggregate cooperative budget per public phase.
///
/// Syscalls such as `readdir` and `fsync` cannot be safely cancelled, so the
/// deadline is checked before and after each operation rather than advertised
/// as a hard kernel wall-clock deadline.
#[derive(Debug)]
pub(super) struct WorkBudget {
    operation_limit: usize,
    operations: usize,
    deadline: Instant,
}

impl WorkBudget {
    fn new(limits: CandidateInventoryLimits, path: &Path) -> Result<Self, CandidateInventoryError> {
        let deadline = Instant::now()
            .checked_add(limits.time)
            .ok_or(CandidateInventoryError::InvalidDeadline)?;
        let budget = Self {
            operation_limit: limits.operations,
            operations: 0,
            deadline,
        };
        budget.check(path)?;
        Ok(budget)
    }

    pub(super) fn check(&self, path: &Path) -> Result<(), CandidateInventoryError> {
        if Instant::now() >= self.deadline {
            Err(CandidateInventoryError::Deadline { path: path.to_owned() })
        } else {
            Ok(())
        }
    }

    pub(super) fn operation(&mut self, path: &Path) -> Result<(), CandidateInventoryError> {
        self.check(path)?;
        if self.operations >= self.operation_limit {
            return Err(CandidateInventoryError::Boundary {
                boundary: CandidateInventoryBoundary::OperationCount,
                limit: u64::try_from(self.operation_limit).unwrap_or(u64::MAX),
                path: path.to_owned(),
            });
        }
        self.operations += 1;
        Ok(())
    }

    pub(super) fn deadline(&self) -> Instant {
        self.deadline
    }
}
