//! Descriptor-rooted reopen of the canonical startup transition journal.
//!
//! Callers must drop the previous lock-bearing store before entering this
//! boundary. The returned store is opened only below the retained mutable
//! `.cast` descriptor and is surrounded by complete installation revalidation.

use thiserror::Error;

use crate::{
    Installation, installation,
    transition_journal::{StorageError, TransitionJournalStore, TransitionRecord},
};

pub(super) fn reopen_canonical_journal(
    installation: &Installation,
) -> Result<(TransitionJournalStore, Option<TransitionRecord>), CanonicalJournalReopenError> {
    installation.revalidate_mutable_namespace()?;
    let cast = installation.retained_mutable_cast_directory()?;
    let journal = TransitionJournalStore::open_in_retained_cast(cast, &installation.root)?;
    installation.revalidate_mutable_namespace()?;
    let record = journal.load_revalidated_retained_cast(cast)?;
    installation.revalidate_mutable_namespace()?;
    Ok((journal, record))
}

/// Reopen without waiting behind a journal holder which may itself be waiting
/// for the caller's continuously retained active-state writer reservation.
pub(super) fn try_reopen_canonical_journal(
    installation: &Installation,
) -> Result<(TransitionJournalStore, Option<TransitionRecord>), CanonicalJournalReopenError> {
    installation.revalidate_mutable_namespace()?;
    let cast = installation.retained_mutable_cast_directory()?;
    let journal = TransitionJournalStore::try_open_in_retained_cast(cast, &installation.root)?;
    installation.revalidate_mutable_namespace()?;
    let record = journal.load_revalidated_retained_cast(cast)?;
    installation.revalidate_mutable_namespace()?;
    Ok((journal, record))
}

#[derive(Debug, Error)]
pub(super) enum CanonicalJournalReopenError {
    #[error("revalidate retained installation around canonical journal reopen")]
    Installation(#[from] installation::Error),
    #[error("open or load the descriptor-rooted canonical journal")]
    Journal(#[from] StorageError),
}
