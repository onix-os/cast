use std::{io, path::PathBuf};

use thiserror::Error;

#[cfg(test)]
use super::ArchivedStatePruneFaultPoint;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ArchivedStatePruneMoveOutcome {
    NotApplied,
    Applied,
    Ambiguous,
}

#[derive(Debug, Error)]
pub(crate) enum ArchivedStatePruneError {
    #[error("archived-state pruning requires at least one exact archived state")]
    EmptyBatch,
    #[error("archived-state pruning batch has {actual} states, exceeding the retained limit of {limit}")]
    BatchTooLarge { actual: usize, limit: usize },
    #[error("open or inspect the retained transition journal for archived-state pruning")]
    Journal(#[from] crate::transition_journal::StorageError),
    #[error("archived-state pruning refuses unresolved transition journal {transition}")]
    UnresolvedJournal { transition: String },
    #[error("audit transition-bearing state rows before archived-state pruning")]
    TransitionEvidence(#[from] crate::db::state::TransitionEvidenceError),
    #[error("audit pre-existing archived-state prune residue before preparing a new batch")]
    PruneResidue(#[from] super::super::ArchivedStatePruneResidueError),
    #[error("remove the exact detached archived-state database snapshots")]
    StateDatabase(#[from] crate::db::state::ExactArchivedRemovalError),
    #[error("revalidate the retained installation root during archived-state pruning")]
    Installation(#[from] crate::installation::Error),
    #[error("authenticate archived state {state} at `{}`", path.display())]
    Identity {
        state: i32,
        path: PathBuf,
        #[source]
        source: Box<super::super::Error>,
    },
    #[error("archived state {state} wrapper layout changed at `{}`", path.display())]
    WrapperLayoutChanged { state: i32, path: PathBuf },
    #[error("archived state {state} wrapper and prune quarantine are on different filesystems")]
    CrossDevice { state: i32 },
    #[error("private archived-state prune quarantine already exists at `{}`", path.display())]
    QuarantineCollision { path: PathBuf },
    #[error("archived-state prune move for state {state} ended {outcome:?}; retained evidence is at `{}`", quarantine.display())]
    Move {
        state: i32,
        outcome: ArchivedStatePruneMoveOutcome,
        quarantine: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("archived-state prune namespace became ambiguous for state {state}; retained evidence is at `{}`", quarantine.display())]
    AmbiguousLayout { state: i32, quarantine: PathBuf },
    #[error("archived-state prune operation `{operation}` is invalid while the session is in phase {phase}")]
    InvalidPhase {
        operation: &'static str,
        phase: &'static str,
    },
    #[error("archived-state prune preparation failed and exact reservation cleanup also failed")]
    PreparationCleanup {
        #[source]
        primary: Box<Self>,
        cleanup: Box<Self>,
    },
    #[error("archived-state prune exceeded its {boundary} limit of {limit} while processing `{}`", path.display())]
    Boundary {
        boundary: &'static str,
        limit: usize,
        path: PathBuf,
    },
    #[error("archived-state prune exceeded its time limit while processing `{}`", path.display())]
    Deadline { path: PathBuf },
    #[error("archived-state prune encountered a cross-device or mounted entry at `{}`", path.display())]
    MountedEntry { path: PathBuf },
    #[error("archived-state prune entry changed before unlink at `{}`", path.display())]
    EntryChanged { path: PathBuf },
    #[error("archived-state prune name was reoccupied after exact unlink at `{}`", path.display())]
    NameReoccupied { path: PathBuf },
    #[error("archived-state prune {operation} `{}`", path.display())]
    Io {
        operation: &'static str,
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[cfg(test)]
    #[error("injected archived-state prune fault at {point:?}")]
    InjectedFault { point: ArchivedStatePruneFaultPoint },
}

pub(super) fn prune_io(
    operation: &'static str,
    path: impl Into<PathBuf>,
    source: io::Error,
) -> ArchivedStatePruneError {
    ArchivedStatePruneError::Io {
        operation,
        path: path.into(),
        source,
    }
}
