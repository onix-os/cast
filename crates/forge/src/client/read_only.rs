//! Public, non-mutating queries over one retained installation snapshot.

use std::{io, path::PathBuf};

use stone::StonePayloadLayoutRecord;
use thiserror::Error;

use crate::{Installation, State, db, installation, package, state, transition_identity, transition_journal};

use super::active_state_snapshot::ReadOnlyActiveStateSnapshot;

#[cfg(test)]
mod tests;

/// A stable, explicitly read-only view of one existing Cast installation.
///
/// Construction retains the installation and journal shared locks, images the
/// three SQLite databases into private read-only memory, and authenticates the
/// live active-state selection. It deliberately has no repository, registry,
/// search, network, configuration, cache, or mutation surface.
///
/// The retained locks serialize cooperating Forge writers. Namespace anchors
/// are revalidated around every query, but an uncooperative same-UID process
/// that bypasses those locks can still modify an already-open source inode;
/// the private database image itself remains immutable.
pub struct ReadOnlyClient {
    // Field order is intentional: drop every derived proof and query handle
    // before releasing the Installation's global shared snapshot lock.
    journal: transition_journal::CleanReadOnlyJournal,
    state_database: db::state::ReadOnlyDatabase,
    metadata_database: db::meta::ReadOnlyDatabase,
    layout_database: db::layout::ReadOnlyDatabase,
    active_state: ReadOnlyActiveStateSnapshot,
    installation: Installation,
}

impl ReadOnlyClient {
    /// Construct a query-only client from an explicit retained snapshot.
    ///
    /// Callers must use [`Installation::open_read_only`]. Naturally read-only,
    /// mutable-system, and frozen-cache Installation values are rejected before
    /// journal inspection or database imaging.
    pub fn new(installation: Installation) -> Result<Self, ReadOnlyClientError> {
        if !installation.is_read_only_snapshot() {
            return Err(ReadOnlyClientError::ReadOnlyInstallationRequired);
        }
        installation.revalidate_read_only_snapshot()?;

        // Recovery evidence has strict precedence over database and live-state
        // parsing. The retained journal proof remains alive for the client.
        let journal = transition_journal::CleanReadOnlyJournal::inspect(&installation).map_err(map_journal_error)?;
        journal.revalidate(&installation).map_err(map_journal_error)?;

        let state_database = db::state::ReadOnlyDatabase::open(&installation).map_err(map_state_error)?;
        journal.revalidate(&installation).map_err(map_journal_error)?;
        state_database.revalidate(&installation).map_err(map_state_error)?;
        require_no_orphan_transition(&state_database)?;

        transition_identity::audit_archived_state_prune_residue_read_only(&installation, &journal).map_err(
            |source| ReadOnlyClientError::ArchivedPruneResidue {
                source: Box::new(source),
            },
        )?;
        journal.revalidate(&installation).map_err(map_journal_error)?;
        state_database.revalidate(&installation).map_err(map_state_error)?;
        installation.revalidate_read_only_snapshot()?;

        // This proof does not acquire the mutable-client coordinator. The
        // retained global and journal shared locks already serialize every
        // cooperating writer while preserving the required recovery order.
        let active_state = ReadOnlyActiveStateSnapshot::capture(&installation).map_err(map_active_state_error)?;
        require_active_state_row(&state_database, &active_state)?;
        active_state.revalidate(&installation).map_err(map_active_state_error)?;
        journal.revalidate(&installation).map_err(map_journal_error)?;

        // Metadata and layout images are opened only after all recovery and
        // strict live-state evidence has passed.
        let metadata_database = db::meta::ReadOnlyDatabase::open(&installation).map_err(map_metadata_error)?;
        active_state.revalidate(&installation).map_err(map_active_state_error)?;
        journal.revalidate(&installation).map_err(map_journal_error)?;
        let layout_database = db::layout::ReadOnlyDatabase::open(&installation).map_err(map_layout_error)?;

        let client = Self {
            journal,
            state_database,
            metadata_database,
            layout_database,
            active_state,
            installation,
        };
        client.revalidate_snapshot()?;
        Ok(client)
    }

    /// List canonical state identifiers without multiplying the per-query
    /// deadline by loading every state's selections individually.
    pub fn list_state_ids(&self) -> Result<Vec<state::Id>, ReadOnlyClientError> {
        self.query(|client| client.state_database.list_ids().map_err(map_state_error))
    }

    /// Return one exact state from the captured database image.
    pub fn get_state(&self, id: state::Id) -> Result<Option<State>, ReadOnlyClientError> {
        self.query(|client| client.state_database.get(id).map_err(map_state_error))
    }

    /// Return the exact state selected by the retained live `/usr/.stateID`.
    pub fn get_active_state(&self) -> Result<Option<State>, ReadOnlyClientError> {
        self.query(|client| {
            let Some(id) = client.active_state.active() else {
                return Ok(None);
            };
            let state = client
                .state_database
                .get(id)
                .map_err(map_state_error)?
                .ok_or(ReadOnlyClientError::ActiveStateMissing { state: id })?;
            Ok(Some(state))
        })
    }

    /// Return metadata only when the exact package identifier exists and
    /// reconstructs to that same identifier.
    pub fn get_package_meta(&self, package: &package::Id) -> Result<Option<package::Meta>, ReadOnlyClientError> {
        self.query(|client| client.metadata_database.get(package).map_err(map_metadata_error))
    }

    /// Return layouts only for the bounded, canonicalized package selection.
    pub fn selected_layouts(
        &self,
        packages: &[package::Id],
    ) -> Result<Vec<(package::Id, StonePayloadLayoutRecord)>, ReadOnlyClientError> {
        self.query(|client| client.layout_database.selected(packages).map_err(map_layout_error))
    }

    fn query<T>(
        &self,
        operation: impl FnOnce(&Self) -> Result<T, ReadOnlyClientError>,
    ) -> Result<T, ReadOnlyClientError> {
        self.revalidate_snapshot()?;
        let result = operation(self);
        // Always perform the trailing proof. A namespace change supersedes an
        // otherwise successful or failed image query.
        self.revalidate_snapshot()?;
        result
    }

    fn revalidate_snapshot(&self) -> Result<(), ReadOnlyClientError> {
        self.installation.revalidate_read_only_snapshot()?;
        self.journal.revalidate(&self.installation).map_err(map_journal_error)?;
        self.state_database
            .revalidate(&self.installation)
            .map_err(map_state_error)?;
        self.metadata_database
            .revalidate(&self.installation)
            .map_err(map_metadata_error)?;
        self.layout_database
            .revalidate(&self.installation)
            .map_err(map_layout_error)?;
        self.active_state
            .revalidate(&self.installation)
            .map_err(map_active_state_error)?;
        self.layout_database
            .revalidate(&self.installation)
            .map_err(map_layout_error)?;
        self.metadata_database
            .revalidate(&self.installation)
            .map_err(map_metadata_error)?;
        self.state_database
            .revalidate(&self.installation)
            .map_err(map_state_error)?;
        self.journal.revalidate(&self.installation).map_err(map_journal_error)?;
        self.installation.revalidate_read_only_snapshot()?;
        Ok(())
    }
}

fn require_no_orphan_transition(state_database: &db::state::ReadOnlyDatabase) -> Result<(), ReadOnlyClientError> {
    if let Some(orphan) = state_database.audit_in_flight_transition().map_err(map_state_error)? {
        return Err(ReadOnlyClientError::OrphanTransitionRow {
            state: orphan.state_id,
            transition: orphan.transition_id.to_string(),
        });
    }
    Ok(())
}

fn require_active_state_row(
    state_database: &db::state::ReadOnlyDatabase,
    active_state: &ReadOnlyActiveStateSnapshot,
) -> Result<(), ReadOnlyClientError> {
    let Some(id) = active_state.active() else {
        return Ok(());
    };
    if state_database.get(id).map_err(map_state_error)?.is_none() {
        return Err(ReadOnlyClientError::ActiveStateMissing { state: id });
    }
    Ok(())
}

fn map_journal_error(source: transition_journal::ReadOnlyJournalError) -> ReadOnlyClientError {
    match source {
        transition_journal::ReadOnlyJournalError::UnresolvedTransition { transition } => {
            ReadOnlyClientError::UnresolvedJournal {
                transition: transition.to_string(),
            }
        }
        source => ReadOnlyClientError::Journal {
            source: Box::new(source),
        },
    }
}

fn map_state_error(source: db::state::ReadOnlyStateError) -> ReadOnlyClientError {
    ReadOnlyClientError::StateSnapshot {
        source: Box::new(source),
    }
}

fn map_metadata_error(source: db::meta::ReadOnlyMetaError) -> ReadOnlyClientError {
    ReadOnlyClientError::MetadataSnapshot {
        source: Box::new(source),
    }
}

fn map_layout_error(source: db::layout::ReadOnlyLayoutError) -> ReadOnlyClientError {
    ReadOnlyClientError::LayoutSnapshot {
        source: Box::new(source),
    }
}

fn map_active_state_error(source: super::Error) -> ReadOnlyClientError {
    match source {
        super::Error::LiveActiveStateProof {
            operation,
            path,
            source,
        } => ReadOnlyClientError::LiveActiveStateProof {
            operation,
            path,
            source,
        },
        super::Error::ActiveStateSnapshotChanged { expected, actual } => {
            ReadOnlyClientError::ActiveStateSnapshotChanged { expected, actual }
        }
        super::Error::Installation(source) => ReadOnlyClientError::Installation(source),
        source => ReadOnlyClientError::ActiveState {
            source: Box::new(source),
        },
    }
}

/// Fail-closed errors from read-only client construction and queries.
#[derive(Debug, Error)]
pub enum ReadOnlyClientError {
    #[error("read-only clients require Installation::open_read_only")]
    ReadOnlyInstallationRequired,
    #[error("revalidate explicit read-only installation snapshot")]
    Installation(#[from] installation::Error),
    #[error("state-transition journal {transition} requires recovery before read-only queries")]
    UnresolvedJournal { transition: String },
    #[error("inspect or revalidate clean read-only transition journal")]
    Journal {
        #[source]
        source: Box<dyn std::error::Error + Send + Sync + 'static>,
    },
    #[error("open or query captured state database")]
    StateSnapshot {
        #[source]
        source: Box<dyn std::error::Error + Send + Sync + 'static>,
    },
    #[error("state {state} retains orphan transition {transition} while the canonical journal is absent")]
    OrphanTransitionRow { state: state::Id, transition: String },
    #[error("audit interrupted archived-state prune evidence")]
    ArchivedPruneResidue {
        #[source]
        source: Box<dyn std::error::Error + Send + Sync + 'static>,
    },
    #[error("{operation} at {path:?} while proving the live active-state selection")]
    LiveActiveStateProof {
        operation: &'static str,
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("active-state snapshot changed since installation discovery: expected {expected:?}, found {actual:?}")]
    ActiveStateSnapshotChanged {
        expected: Option<state::Id>,
        actual: Option<state::Id>,
    },
    #[error("capture or revalidate strict live active-state evidence")]
    ActiveState {
        #[source]
        source: Box<super::Error>,
    },
    #[error("live active state {state} is missing from the captured state database")]
    ActiveStateMissing { state: state::Id },
    #[error("open or query exact package metadata snapshot")]
    MetadataSnapshot {
        #[source]
        source: Box<dyn std::error::Error + Send + Sync + 'static>,
    },
    #[error("open or query selected package-layout snapshot")]
    LayoutSnapshot {
        #[source]
        source: Box<dyn std::error::Error + Send + Sync + 'static>,
    },
}
