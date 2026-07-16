use thiserror::Error;

use crate::{Installation, db, installation, transition_identity, transition_journal};

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
        installation.revalidate_mutable_namespace()?;
        let cast = installation.retained_mutable_cast_directory()?;
        after_mutable_namespace_preflight();
        let journal = transition_journal::TransitionJournalStore::open_in_retained_cast(cast, &installation.root);
        let namespace = installation.revalidate_mutable_namespace();
        namespace?;
        let journal = journal?;

        let record = journal.load();
        let namespace = installation.revalidate_mutable_namespace();
        namespace?;
        let record = record?;
        if let Some(record) = record {
            return Err(Error::UnresolvedJournal {
                transition: record.transition_id.as_str().to_owned(),
            });
        }

        let orphan = state_db.audit_in_flight_transition();
        let namespace = installation.revalidate_mutable_namespace();
        namespace?;
        let orphan = orphan?;
        if let Some(orphan) = orphan {
            return Err(Error::OrphanTransitionRow {
                state: i32::from(orphan.state_id),
                transition: orphan.transition_id.as_str().to_owned(),
            });
        }

        let residue = transition_identity::audit_archived_state_prune_residue(installation, &journal);
        let namespace = installation.revalidate_mutable_namespace();
        namespace?;
        residue?;
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

#[cfg(test)]
std::thread_local! {
    static AFTER_MUTABLE_NAMESPACE_PREFLIGHT: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
pub(super) fn arm_after_mutable_namespace_preflight(hook: impl FnOnce() + 'static) {
    AFTER_MUTABLE_NAMESPACE_PREFLIGHT.with(|slot| {
        assert!(slot.borrow_mut().replace(Box::new(hook)).is_none());
    });
}

#[cfg(test)]
fn after_mutable_namespace_preflight() {
    AFTER_MUTABLE_NAMESPACE_PREFLIGHT.with(|slot| {
        if let Some(hook) = slot.borrow_mut().take() {
            hook();
        }
    });
}

#[cfg(not(test))]
fn after_mutable_namespace_preflight() {}

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
    #[error("audit interrupted archived-state prune evidence")]
    ArchivedStatePruneResidue(#[from] transition_identity::ArchivedStatePruneResidueError),
    #[error("revalidate retained mutable installation namespace during startup")]
    Installation(#[from] installation::Error),
    #[error("load canonical authored system intent after the system startup gate")]
    DefaultSystemIntent(#[source] default_system_intent::Error),
}

#[cfg(test)]
pub(super) use default_system_intent::arm_after_default_directory_retained;
