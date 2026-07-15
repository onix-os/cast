use thiserror::Error;

use crate::{Installation, db, installation, transition_journal};

use super::active_state_snapshot::ActiveStateLease;

mod default_system_intent;

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

    /// Evaluate the canonical system intent only after strict active-state
    /// discovery, while this retained journal guard and the active lease's
    /// cooperating-writer coordinator are both still alive.
    pub(super) fn load_default_system_intent(
        &self,
        installation: &Installation,
        _active_state: &ActiveStateLease,
    ) -> Result<Option<crate::system_model::LoadedSystemModel>, Error> {
        default_system_intent::load(installation).map_err(Error::DefaultSystemIntent)
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
    #[error("load canonical authored system intent after the system startup gate")]
    DefaultSystemIntent(#[source] default_system_intent::Error),
}

#[cfg(test)]
pub(super) use default_system_intent::arm_after_default_directory_retained;
