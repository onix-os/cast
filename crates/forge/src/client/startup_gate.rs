use thiserror::Error;

use crate::{Installation, db, installation, transition_journal};

/// Exclusive proof that no interrupted system transition predates client
/// construction.
///
/// Keeping the journal store alive serializes repository and registry
/// construction with transition writers. The builder must drop this proof
/// before returning the client because stateful operations acquire their own
/// retained journal store.
pub(super) struct CleanSystemStartup {
    _journal: transition_journal::TransitionJournalStore,
}

impl CleanSystemStartup {
    pub(super) fn enter(installation: &Installation, state_db: &db::state::Database) -> Result<Self, Error> {
        let journal = transition_journal::TransitionJournalStore::open_retained(
            installation.root_directory(),
            &installation.root,
        )?;

        if let Some(record) = journal.load()? {
            return Err(Error::UnresolvedJournal {
                transition: record.transition_id.as_str().to_owned(),
            });
        }

        if let Some(orphan) = state_db.audit_in_flight_transition()? {
            return Err(Error::OrphanTransitionRow {
                state: i32::from(orphan.state_id),
                transition: orphan.transition_id.as_str().to_owned(),
            });
        }

        installation.revalidate_root_directory()?;
        Ok(Self { _journal: journal })
    }
}

#[derive(Debug, Error)]
pub(super) enum Error {
    #[error("inspect retained state-transition journal")]
    Journal(#[from] transition_journal::StorageError),
    #[error("state-transition journal {transition} requires recovery before client startup")]
    UnresolvedJournal { transition: String },
    #[error("audit in-flight state-transition rows")]
    TransitionEvidence(#[from] db::state::TransitionEvidenceError),
    #[error("state {state} retains orphan transition {transition} while the canonical journal is absent")]
    OrphanTransitionRow { state: i32, transition: String },
    #[error("revalidate installation root after startup discovery")]
    Installation(#[from] installation::Error),
}
