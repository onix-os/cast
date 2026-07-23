//! Mandatory retained isolation ABI before stateful transaction triggers.
//!
//! Missing merged-/usr links are published only while `CandidatePrepared` is
//! canonical. The resulting descriptor-pinned proof is carried through every
//! later trigger and `/usr` readiness boundary; callers cannot supply or omit
//! that proof.

use thiserror::Error;

use crate::{
    Installation,
    client::{RetainedRootAbi, create_root_links_retained},
    state::TransitionId,
    transition_journal::{Operation, Phase},
};

use super::super::{Error as IdentityError, RetainedDirectory, StatefulTreeIdentity};
use super::{
    PreparedTransactionIsolationCoordinator, PreparedTransactionTriggerCoordinator, StatefulTransitionCoordinator,
    StatefulTransitionCoordinatorError, TransactionTriggerOperationReadiness, TransactionTriggerReadiness,
};

const PREPARE_TRANSACTION_ISOLATION: &str = "publish retained transaction isolation ABI";
const ISOLATION_RELATIVE: &std::ffi::CStr = c".cast/root/isolation";
const ISOLATION_ABI_NAMES: [&[u8]; 5] = [b"sbin", b"bin", b"lib", b"lib64", b"lib32"];
const ISOLATION_MOUNT_TARGETS: [&std::ffi::CStr; 6] = [c"etc", c"usr", c"proc", c"tmp", c"sys", c"dev"];

/// Exact controlled isolation directory and all five pinned merged-/usr links.
/// The retained Installation keeps the original writer lease and root
/// descriptor alive; the path is diagnostic and used only for same-inode name
/// revalidation.
#[derive(Debug)]
pub(super) struct RetainedTransactionIsolationAbi {
    installation: Installation,
    directory: RetainedDirectory,
    root_abi: RetainedRootAbi,
}

/// Fail-stop isolation preparation. Every error drops the coordinator while
/// `CandidatePrepared` remains canonical; exact partial links are startup-safe
/// and retryable, and no variant returns a reusable authority.
#[derive(Debug, Error)]
pub(super) enum TransactionIsolationAbiFailure {
    #[error("transition {transition_id} failed transaction-isolation preflight")]
    Preflight {
        transition_id: TransitionId,
        #[source]
        source: StatefulTransitionCoordinatorError,
    },
    #[error(
        "transition {transition_id} could not publish the retained transaction isolation ABI; CandidatePrepared remains canonical"
    )]
    Publication {
        transition_id: TransitionId,
        #[source]
        source: crate::client::Error,
    },
    #[error("transition {transition_id} failed final retained transaction-isolation evidence")]
    FinalEvidence {
        transition_id: TransitionId,
        #[source]
        source: StatefulTransitionCoordinatorError,
    },
}

impl PreparedTransactionIsolationCoordinator {
    /// Publish missing exact isolation links and return the only typestate with
    /// a transaction-trigger runner. The journal is not advanced here.
    pub(super) fn prepare_for_transaction_triggers(
        self,
        installation: &Installation,
    ) -> Result<PreparedTransactionTriggerCoordinator, TransactionIsolationAbiFailure> {
        let Self {
            coordinator,
            metadata,
            provenance,
            operation,
        } = self;
        let transition_id = coordinator.record.transition_id.clone();
        let preflight = |source| TransactionIsolationAbiFailure::Preflight {
            transition_id: transition_id.clone(),
            source,
        };
        let expected_operation = operation.operation();
        coordinator
            .require_operation(expected_operation, PREPARE_TRANSACTION_ISOLATION)
            .map_err(preflight)?;
        coordinator
            .require_phase(Phase::CandidatePrepared, PREPARE_TRANSACTION_ISOLATION)
            .map_err(preflight)?;
        let candidate = coordinator.candidate_state().map_err(preflight)?;

        // ActiveReblit already retained the exact Installation used for its
        // namespace reservation. Never let a later caller redirect isolation
        // publication through a substitute capability.
        let installation = operation.installation().unwrap_or(installation);
        require_isolation_preflight(
            &coordinator,
            candidate,
            &metadata,
            &provenance,
            &operation,
            installation,
        )
        .map_err(preflight)?;

        let directory = RetainedDirectory::open_beneath(
            installation.root_directory(),
            ISOLATION_RELATIVE,
            installation.isolation_dir(),
        )
        .map_err(StatefulTransitionCoordinatorError::Identity)
        .map_err(preflight)?;
        require_no_unexpected_isolation_entries(&directory).map_err(preflight)?;

        // Opening and ABI preflight may take time. Rebind every semantic and
        // namespace witness immediately before the monotonic publication.
        require_isolation_preflight(
            &coordinator,
            candidate,
            &metadata,
            &provenance,
            &operation,
            installation,
        )
        .map_err(preflight)?;
        directory
            .revalidate_beneath(installation.root_directory(), ISOLATION_RELATIVE)
            .map_err(StatefulTransitionCoordinatorError::Identity)
            .map_err(preflight)?;
        require_no_unexpected_isolation_entries(&directory).map_err(preflight)?;

        let root_abi =
            create_root_links_retained(&installation.isolation_dir(), &directory.file).map_err(|source| {
                TransactionIsolationAbiFailure::Publication {
                    transition_id: transition_id.clone(),
                    source,
                }
            })?;
        let isolation = RetainedTransactionIsolationAbi {
            installation: installation.clone(),
            directory,
            root_abi,
        };
        let readiness = TransactionTriggerReadiness { operation, isolation };

        require_isolation_final_evidence(&coordinator, candidate, &metadata, &provenance, &readiness)
            .map_err(|source| TransactionIsolationAbiFailure::FinalEvidence { transition_id, source })?;
        Ok(PreparedTransactionTriggerCoordinator {
            coordinator,
            metadata,
            provenance,
            readiness,
        })
    }
}

fn require_isolation_preflight(
    coordinator: &StatefulTransitionCoordinator,
    candidate: crate::state::Id,
    metadata: &super::super::CandidateMetadataProof,
    provenance: &crate::db::state::MetadataProvenance,
    operation: &TransactionTriggerOperationReadiness,
    installation: &Installation,
) -> Result<(), StatefulTransitionCoordinatorError> {
    coordinator.require_prepared_metadata_sandwich(candidate, metadata, provenance)?;
    operation.require_staged(&coordinator.identity)?;
    require_pre_exchange_installation(&coordinator.identity, installation)?;
    coordinator.require_prepared_metadata_sandwich(candidate, metadata, provenance)?;
    operation.require_staged(&coordinator.identity)?;
    require_pre_exchange_installation(&coordinator.identity, installation)
}

fn require_isolation_final_evidence(
    coordinator: &StatefulTransitionCoordinator,
    candidate: crate::state::Id,
    metadata: &super::super::CandidateMetadataProof,
    provenance: &crate::db::state::MetadataProvenance,
    readiness: &TransactionTriggerReadiness,
) -> Result<(), StatefulTransitionCoordinatorError> {
    coordinator.require_prepared_metadata_sandwich(candidate, metadata, provenance)?;
    readiness.require_staged(&coordinator.identity)?;
    coordinator.require_prepared_metadata_sandwich(candidate, metadata, provenance)?;
    readiness.require_staged(&coordinator.identity)
}

pub(super) fn require_pre_journal_active_reblit_installation(
    identity: &StatefulTreeIdentity,
    installation: &Installation,
    state: crate::state::Id,
) -> Result<(), StatefulTransitionCoordinatorError> {
    installation
        .revalidate_mutable_namespace()
        .map_err(IdentityError::from)
        .map_err(StatefulTransitionCoordinatorError::Identity)?;
    identity.require_known_id_absent(Operation::ActiveReblit, state)?;
    identity
        .candidate
        .verify_named_read_only(&installation.staging_path("usr"))
        .map_err(StatefulTransitionCoordinatorError::Identity)?;
    identity
        .previous
        .verify_named_read_only(&installation.root.join("usr"))
        .map_err(StatefulTransitionCoordinatorError::Identity)?;
    identity.require_known_id_absent(Operation::ActiveReblit, state)?;
    installation
        .revalidate_mutable_namespace()
        .map_err(IdentityError::from)
        .map_err(StatefulTransitionCoordinatorError::Identity)
}

fn require_pre_exchange_installation(
    identity: &StatefulTreeIdentity,
    installation: &Installation,
) -> Result<(), StatefulTransitionCoordinatorError> {
    installation
        .revalidate_mutable_namespace()
        .map_err(IdentityError::from)
        .map_err(StatefulTransitionCoordinatorError::Identity)?;
    identity
        .verify_candidate_named_with_state_id(&installation.staging_path("usr"))
        .map_err(StatefulTransitionCoordinatorError::Identity)?;
    identity
        .previous
        .verify_named_read_only(&installation.root.join("usr"))
        .map_err(StatefulTransitionCoordinatorError::Identity)?;
    installation
        .revalidate_mutable_namespace()
        .map_err(IdentityError::from)
        .map_err(StatefulTransitionCoordinatorError::Identity)
}

impl RetainedTransactionIsolationAbi {
    fn require_staged(&self, identity: &StatefulTreeIdentity) -> Result<(), StatefulTransitionCoordinatorError> {
        self.require(identity, false)
    }

    fn require_live(&self, identity: &StatefulTreeIdentity) -> Result<(), StatefulTransitionCoordinatorError> {
        self.require(identity, true)
    }

    fn require(&self, identity: &StatefulTreeIdentity, live: bool) -> Result<(), StatefulTransitionCoordinatorError> {
        self.installation
            .revalidate_mutable_namespace()
            .map_err(IdentityError::from)
            .map_err(StatefulTransitionCoordinatorError::Identity)?;
        self.directory
            .revalidate_beneath(self.installation.root_directory(), ISOLATION_RELATIVE)
            .map_err(StatefulTransitionCoordinatorError::Identity)?;
        self.root_abi
            .revalidate()
            .map_err(StatefulTransitionCoordinatorError::IsolationAbi)?;
        require_no_unexpected_isolation_entries(&self.directory)?;

        let candidate = if live {
            self.installation.root.join("usr")
        } else {
            self.installation.staging_path("usr")
        };
        let previous = if live {
            self.installation.staging_path("usr")
        } else {
            self.installation.root.join("usr")
        };
        identity
            .verify_candidate_named_with_state_id(&candidate)
            .map_err(StatefulTransitionCoordinatorError::Identity)?;
        identity
            .previous
            .verify_named_read_only(&previous)
            .map_err(StatefulTransitionCoordinatorError::Identity)?;

        self.root_abi
            .revalidate()
            .map_err(StatefulTransitionCoordinatorError::IsolationAbi)?;
        require_no_unexpected_isolation_entries(&self.directory)?;
        self.directory
            .revalidate_beneath(self.installation.root_directory(), ISOLATION_RELATIVE)
            .map_err(StatefulTransitionCoordinatorError::Identity)?;
        self.installation
            .revalidate_mutable_namespace()
            .map_err(IdentityError::from)
            .map_err(StatefulTransitionCoordinatorError::Identity)
    }

    pub(super) fn view(&self) -> (&Installation, &RetainedRootAbi) {
        (&self.installation, &self.root_abi)
    }
}

fn require_no_unexpected_isolation_entries(
    directory: &RetainedDirectory,
) -> Result<(), StatefulTransitionCoordinatorError> {
    let entries = directory
        .entries(ISOLATION_ABI_NAMES.len() + ISOLATION_MOUNT_TARGETS.len() + 1)
        .map_err(StatefulTransitionCoordinatorError::Identity)?;
    let mut unexpected = Vec::new();
    for entry in entries {
        if ISOLATION_ABI_NAMES.contains(&entry.as_slice()) {
            continue;
        }
        let Some(target) = ISOLATION_MOUNT_TARGETS
            .iter()
            .copied()
            .find(|target| target.to_bytes() == entry.as_slice())
        else {
            unexpected.push(String::from_utf8_lossy(&entry).into_owned());
            continue;
        };
        let child = directory
            .open_child(target, directory.path.join(target.to_string_lossy().as_ref()))
            .map_err(StatefulTransitionCoordinatorError::Identity)?;
        child
            .require_exact_entries(&[])
            .map_err(StatefulTransitionCoordinatorError::Identity)?;
    }
    unexpected.sort();
    if unexpected.is_empty() {
        Ok(())
    } else {
        Err(StatefulTransitionCoordinatorError::UnexpectedIsolationEntries {
            path: directory.path.clone(),
            entries: unexpected,
        })
    }
}

impl TransactionTriggerReadiness {
    pub(super) fn require_staged(
        &self,
        identity: &StatefulTreeIdentity,
    ) -> Result<(), StatefulTransitionCoordinatorError> {
        self.isolation.require_staged(identity)?;
        self.operation.require_staged(identity)?;
        self.isolation.require_staged(identity)
    }

    pub(super) fn require_live(
        &self,
        identity: &StatefulTreeIdentity,
    ) -> Result<(), StatefulTransitionCoordinatorError> {
        self.isolation.require_live(identity)?;
        self.operation.require_live(identity)?;
        self.isolation.require_live(identity)
    }

    pub(super) fn isolation_view(&self) -> (&Installation, &RetainedRootAbi) {
        self.isolation.view()
    }
}

impl TransactionTriggerOperationReadiness {
    fn operation(&self) -> Operation {
        match self {
            Self::NewState => Operation::NewState,
            Self::ActiveReblit(_) => Operation::ActiveReblit,
        }
    }
}
