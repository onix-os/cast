use thiserror::Error;

use crate::{Installation, db, installation, transition_identity, transition_journal};

use super::{active_state_snapshot::ActiveStateLease, startup_reconciliation};

mod default_system_intent;

/// Exclusive proof that no interrupted system transition predates client
/// construction.
///
/// Keeping the journal store alive serializes repository and registry
/// construction with transition writers. The builder must drop this proof
/// before returning the client because stateful operations acquire their own
/// retained journal store.
pub(super) struct CleanSystemStartup {
    _authority: startup_reconciliation::StartupRecoveryAuthority,
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
        let in_flight = state_db.audit_in_flight_transition();
        let namespace = installation.revalidate_mutable_namespace();
        namespace?;
        let in_flight = in_flight?;
        if let Some(record) = record {
            let pending = startup_reconciliation::PendingSystemTransition::inspect(
                installation,
                state_db,
                journal,
                record,
                in_flight,
            )
            .map_err(|source| match source {
                startup_reconciliation::InspectionError::Database(source) => Error::TransitionEvidence(source),
                startup_reconciliation::InspectionError::MetadataProvenance(source) => {
                    Error::MetadataProvenance(source)
                }
                startup_reconciliation::InspectionError::Installation(source) => Error::Installation(source),
            })?;
            return Err(Error::RecoveryPending(pending));
        }
        if let Some(orphan) = in_flight {
            return Err(Error::OrphanTransitionRow {
                state: i32::from(orphan.state_id),
                transition: orphan.transition_id.as_str().to_owned(),
            });
        }

        let authority = startup_reconciliation::StartupRecoveryAuthority::new(installation, journal, state_db);
        let residue = transition_identity::audit_archived_state_prune_residue(installation, authority.journal());
        let namespace = installation.revalidate_mutable_namespace();
        namespace?;
        residue?;
        Ok(Self { _authority: authority })
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
    #[error(transparent)]
    RecoveryPending(#[from] startup_reconciliation::PendingSystemTransition),
    #[error("audit in-flight state-transition rows")]
    TransitionEvidence(#[from] db::state::TransitionEvidenceError),
    #[error("inspect immutable generated-metadata provenance during startup reconciliation")]
    MetadataProvenance(#[from] db::state::MetadataProvenanceError),
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
