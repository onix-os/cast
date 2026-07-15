//! Retained whole-wrapper publication for one repaired archived state.
//!
//! The guard is intentionally process-local. It does not claim crash-reopen
//! recovery without a durable journal phase, but every namespace syscall is
//! one-shot and reconciled against retained inode witnesses before another
//! mutation is considered.

mod error;
mod fault_injection;
mod layout;
mod live_active;
mod preparation;
mod preparation_cleanup;
mod preservation;
mod publication;
mod validation;

use std::{
    ffi::CString,
    path::{Path, PathBuf},
    sync::Mutex,
};

use super::{RetainedDirectory, RetainedIdentity};
use crate::{state, transition_journal::TransitionJournalStore};

pub(crate) use error::{ArchivedStateRepairError, ArchivedStateRepairFailure, ArchivedStateRepairOutcome};
#[cfg(test)]
#[allow(unused_imports)]
pub(crate) use fault_injection::{
    ArchivedStateRepairFaultPoint, ArchivedStateRepairNamespaceMove, archived_state_repair_namespace_syscall_count,
    arm_archived_state_repair_faults, arm_before_archived_state_repair_cleanup,
    arm_before_archived_state_repair_namespace_syscall, arm_before_archived_state_repair_preservation,
    arm_before_archived_state_repair_publication, arm_before_archived_state_repair_suffix_retry,
    arm_between_archived_state_repair_layout_reads,
};

const STAGING_NAME: &std::ffi::CStr = c"staging";
const USR_NAME: &std::ffi::CStr = c"usr";
const MAX_QUARANTINE_NAMES: usize = 256;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum ArchivedStateRepairPublication {
    /// The repaired wrapper is canonical and the displaced wrapper is retained
    /// whole under the returned private path.
    Replaced { displaced_wrapper: PathBuf },
    /// The canonical name was absent and now denotes the repaired wrapper.
    Published,
}

#[derive(Debug)]
enum ArchiveBaseline {
    Existing(RetainedDirectory),
    Missing,
}

/// Process-local authority for one archived-state repair.
///
/// `staging`, `candidate`, `archive`, and `replacement` retain the exact
/// inodes established before triggers. Paths are diagnostics only. The
/// operation mutex prevents two in-process callers from racing the same
/// one-shot namespace state machine.
#[derive(Debug)]
pub(crate) struct ArchivedStateRepairIdentity {
    journal: TransitionJournalStore,
    expected: state::State,
    active_expected: Option<state::State>,
    state_name: CString,
    roots: RetainedDirectory,
    staging: RetainedDirectory,
    candidate: RetainedIdentity,
    live_active: live_active::LiveActiveBaseline,
    archive: ArchiveBaseline,
    quarantine: RetainedDirectory,
    replacement: RetainedDirectory,
    quarantine_name: CString,
    quarantine_path: PathBuf,
    operation: Mutex<()>,
}

impl ArchivedStateRepairIdentity {
    /// Exact candidate `/usr` capability retained before metadata decoration.
    ///
    /// The path is diagnostic only. Callers must perform every traversal from
    /// the descriptor and sandwich their work between strict guard proofs.
    pub(crate) fn retained_candidate_usr(&self) -> (&std::fs::File, &Path) {
        (
            self.candidate.store.retained_directory(),
            self.candidate.store.display_path(),
        )
    }
}
