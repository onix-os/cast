//! Non-active state repair without exposing a partially rebuilt archive.
//!
//! Verification materializes a candidate in the fixed staging wrapper before
//! entering this module. From the first metadata write onward, the retained
//! identity guard owns publication and failed-candidate preservation. System
//! triggers are deliberately absent: repairing an inactive state must not
//! mutate the live `/usr` or perform boot synchronization.

use std::path::PathBuf;

use thiserror::Error as ThisError;

use super::{
    Client, Error, TriggerScope, archived_repair_materialization::ArchivedRepairCandidate, archived_repair_metadata,
    create_root_links,
};
use crate::{
    State, SystemModel,
    transition_identity::{
        ArchivedStateRepairFailure, ArchivedStateRepairIdentity, ArchivedStateRepairOutcome,
        ArchivedStateRepairPublication,
    },
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum ArchivedRepairCheckpoint {
    IdentityPrepared,
    MetadataRecorded,
    BeforeTransactionTriggers,
    AfterTransactionTriggers,
    BeforePublication,
}

#[derive(Debug, ThisError)]
pub(super) enum RepairError {
    #[error(
        "prepare retained repair identity for inactive state {state}; the candidate database row remains and the candidate is still staged at {staging:?}"
    )]
    Preparation {
        state: crate::state::Id,
        staging: PathBuf,
        #[source]
        source: Box<dyn std::error::Error + Send + Sync + 'static>,
    },
    #[error("revalidate retained inactive-state repair identity for state {state} during {phase}")]
    Identity {
        state: crate::state::Id,
        phase: &'static str,
        #[source]
        source: Box<dyn std::error::Error + Send + Sync + 'static>,
    },
    #[error(
        "inactive-state repair for {state} failed before publication; the whole candidate wrapper is preserved at {quarantine:?}, its database row remains, and transaction-trigger side effects may remain: {primary}"
    )]
    CandidatePreserved {
        state: crate::state::Id,
        quarantine: PathBuf,
        #[source]
        primary: Box<Error>,
    },
    #[error(
        "inactive-state repair for {state} failed before publication ({primary}); whole-wrapper preservation ended with {outcome} ({preservation}), so no namespace should be retried or removed blindly"
    )]
    CandidatePreservationIncomplete {
        state: crate::state::Id,
        primary: Box<Error>,
        outcome: &'static str,
        preservation: Box<ArchivedStateRepairFailure>,
    },
    #[error(
        "inactive-state repair publication for {state} ended with {outcome}; an applied publication is committed and must never be reversed through fixed staging, while an ambiguous result requires manual reconciliation"
    )]
    PublicationIncomplete {
        state: crate::state::Id,
        outcome: &'static str,
        #[source]
        source: Box<ArchivedStateRepairFailure>,
    },
}

impl Client {
    pub(super) fn repair_archived_state(
        &self,
        candidate: ArchivedRepairCandidate,
        state: &State,
        system_snapshot: SystemModel,
    ) -> Result<ArchivedStateRepairPublication, Error> {
        self.repair_archived_state_with_checkpoint(candidate, state, system_snapshot, |_| Ok(()))
    }

    pub(super) fn repair_archived_state_with_checkpoint<F>(
        &self,
        candidate: ArchivedRepairCandidate,
        state: &State,
        system_snapshot: SystemModel,
        mut checkpoint: F,
    ) -> Result<ArchivedStateRepairPublication, Error>
    where
        F: FnMut(ArchivedRepairCheckpoint) -> Result<(), Error>,
    {
        self.require_non_frozen()?;
        if !matches!(&self.scope, super::Scope::Stateful) {
            return Err(Error::EphemeralProhibitedOperation);
        }
        let ArchivedRepairCandidate {
            tree: fstree,
            _coordinator,
        } = candidate;
        let staging = self.installation.staging_dir();

        // Materialization created the wrapper under `_coordinator` with
        // independent package inodes from byte zero. This is the only mutation
        // before the retained wrapper identity exists; the coordinator remains
        // owned here through metadata, triggers, and publication.
        super::record_state_id(&staging, state.id).map_err(|source| Error::ArchivedStateRepair {
            source: Box::new(RepairError::Preparation {
                state: state.id,
                staging: staging.clone(),
                source: Box::new(source),
            }),
        })?;
        let identity =
            ArchivedStateRepairIdentity::prepare(&self.installation, &self.state_db, state).map_err(|source| {
                Error::ArchivedStateRepair {
                    source: Box::new(RepairError::Preparation {
                        state: state.id,
                        staging: staging.clone(),
                        source: Box::new(source),
                    }),
                }
            })?;

        let prepare = (|| {
            checkpoint(ArchivedRepairCheckpoint::IdentityPrepared)?;
            identity
                .verify_candidate_snapshot(&self.installation, &self.state_db)
                .map_err(|source| archived_repair_identity_error(state, "before metadata decoration", source))?;
            archived_repair_metadata::decorate(&identity, &system_snapshot)?;

            // Root ABI links belong to the container scratch root, never to
            // the candidate wrapper. Therefore staging remains exactly
            // `{usr}`, which is part of the guard's retained identity.
            create_root_links(&self.installation.isolation_dir())?;

            identity
                .verify_candidate_snapshot(&self.installation, &self.state_db)
                .map_err(|source| archived_repair_identity_error(state, "after metadata decoration", source))?;
            checkpoint(ArchivedRepairCheckpoint::MetadataRecorded)?;
            checkpoint(ArchivedRepairCheckpoint::BeforeTransactionTriggers)?;

            // Transaction trigger execution pins the scratch container root,
            // installation `/etc`, and this exact retained candidate `/usr`
            // before activation. The fixed staging pathname is never a bind
            // authority. System triggers would target the live root and are
            // intentionally deferred until a later activation.
            let (candidate_usr, candidate_usr_path) = identity.retained_candidate_usr();
            Self::apply_triggers(
                TriggerScope::RetainedTransaction {
                    installation: &self.installation,
                    candidate_usr,
                    candidate_usr_path,
                },
                &fstree,
            )?;

            identity
                .verify_candidate_snapshot(&self.installation, &self.state_db)
                .map_err(|source| archived_repair_identity_error(state, "after transaction triggers", source))?;
            checkpoint(ArchivedRepairCheckpoint::AfterTransactionTriggers)?;
            checkpoint(ArchivedRepairCheckpoint::BeforePublication)?;
            identity
                .verify_candidate_snapshot(&self.installation, &self.state_db)
                .map_err(|source| archived_repair_identity_error(state, "immediately before publication", source))?;
            Ok::<(), Error>(())
        })();

        if let Err(primary) = prepare {
            return Err(self.preserve_failed_archived_repair(state, &identity, primary));
        }

        match identity.publish(&self.installation, &self.state_db) {
            Ok(publication) => Ok(publication),
            Err(failure) if failure.outcome() == ArchivedStateRepairOutcome::NotApplied => {
                let primary = archived_repair_publication_error(state, failure);
                Err(self.preserve_failed_archived_repair(state, &identity, primary))
            }
            Err(failure) => Err(archived_repair_publication_error(state, failure)),
        }
    }

    fn preserve_failed_archived_repair(
        &self,
        state: &State,
        identity: &ArchivedStateRepairIdentity,
        primary: Error,
    ) -> Error {
        match identity.preserve_failed_candidate(&self.installation, &self.state_db) {
            Ok(quarantine) => Error::ArchivedStateRepair {
                source: Box::new(RepairError::CandidatePreserved {
                    state: state.id,
                    quarantine,
                    primary: Box::new(primary),
                }),
            },
            Err(preservation) => {
                let outcome = outcome_name(preservation.outcome());
                Error::ArchivedStateRepair {
                    source: Box::new(RepairError::CandidatePreservationIncomplete {
                        state: state.id,
                        primary: Box::new(primary),
                        outcome,
                        preservation: Box::new(preservation),
                    }),
                }
            }
        }
    }
}

fn archived_repair_identity_error(
    state: &State,
    phase: &'static str,
    source: impl std::error::Error + Send + Sync + 'static,
) -> Error {
    Error::ArchivedStateRepair {
        source: Box::new(RepairError::Identity {
            state: state.id,
            phase,
            source: Box::new(source),
        }),
    }
}

fn archived_repair_publication_error(state: &State, failure: ArchivedStateRepairFailure) -> Error {
    let outcome = outcome_name(failure.outcome());
    Error::ArchivedStateRepair {
        source: Box::new(RepairError::PublicationIncomplete {
            state: state.id,
            outcome,
            source: Box::new(failure),
        }),
    }
}

fn outcome_name(outcome: ArchivedStateRepairOutcome) -> &'static str {
    match outcome {
        ArchivedStateRepairOutcome::NotApplied => "not-applied",
        ArchivedStateRepairOutcome::Applied => "applied",
        ArchivedStateRepairOutcome::Ambiguous => "ambiguous",
    }
}
