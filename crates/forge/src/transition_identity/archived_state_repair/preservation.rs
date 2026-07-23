use super::{
    ArchivedStateRepairError, ArchivedStateRepairFailure, ArchivedStateRepairIdentity, ArchivedStateRepairOutcome,
    STAGING_NAME,
    error::{failure, identity, io_error},
    fault_injection::{
        ArchivedStateRepairFaultPoint, ArchivedStateRepairNamespaceMove, before_namespace_syscall, before_preservation,
        before_suffix_retry, checkpoint,
    },
    layout::RepairLayout,
};
use crate::{Installation, db, linux_fs};

const MAX_EXACT_NOT_APPLIED_RETRIES: usize = 1;

impl ArchivedStateRepairIdentity {
    /// Preserve the whole failed candidate wrapper under its pre-reserved
    /// private name and restore fixed staging to the exact empty 0700 wrapper.
    ///
    /// Movement depends only on retained wrapper inodes. A trigger may corrupt
    /// or replace `.stateID` and the wrapper can still be moved out of the
    /// ordinary state namespace. The strict DB/inactive proof is deliberately
    /// performed after movement so semantic corruption cannot strand bytes.
    pub(crate) fn preserve_failed_candidate(
        &self,
        installation: &Installation,
        state_db: &db::state::Database,
    ) -> Result<std::path::PathBuf, ArchivedStateRepairFailure> {
        let _operation = self.operation.lock().map_err(|_| {
            failure(
                ArchivedStateRepairOutcome::Ambiguous,
                ArchivedStateRepairError::OperationLockPoisoned,
            )
        })?;
        let mut retries = 0usize;
        let mut semantic_failure = self.require_semantic_snapshot(installation, state_db).err();

        loop {
            match self
                .layout()
                .map_err(|source| failure(ArchivedStateRepairOutcome::Ambiguous, source))?
            {
                RepairLayout::Preserved => {
                    return self.finish_preservation_with_prior(installation, state_db, semantic_failure);
                }
                RepairLayout::Initial => {}
                RepairLayout::CandidateCanonical | RepairLayout::Complete => {
                    return Err(failure(
                        ArchivedStateRepairOutcome::Applied,
                        namespace_error("refusing to reverse a sticky canonical repair candidate"),
                    ));
                }
            }

            let preflight = self.preflight_preservation(installation);
            if semantic_failure.is_none() {
                semantic_failure = self.require_semantic_snapshot(installation, state_db).err();
            }
            if let Err(source) = preflight {
                match self.layout() {
                    Ok(RepairLayout::Initial) if retries < MAX_EXACT_NOT_APPLIED_RETRIES => {
                        self.authorize_preservation_retry(installation, source)?;
                        retries += 1;
                        continue;
                    }
                    Ok(RepairLayout::Initial) => {
                        return Err(self.preservation_failure_after_reconciliation(
                            installation,
                            "finish failed-wrapper preservation preflight reconciliation",
                            ArchivedStateRepairOutcome::NotApplied,
                            source,
                        ));
                    }
                    Ok(RepairLayout::Preserved) => {
                        if source.namespace_observation_was_unstable() {
                            return Err(failure(ArchivedStateRepairOutcome::Ambiguous, source));
                        }
                        return match self.finish_preservation_with_prior(installation, state_db, semantic_failure) {
                            Ok(_) => Err(failure(ArchivedStateRepairOutcome::Applied, source)),
                            Err(finish) => Err(failure(
                                ArchivedStateRepairOutcome::Applied,
                                ArchivedStateRepairError::AppliedAfterPreflightFailure {
                                    operation: "preserve failed archived-repair wrapper",
                                    primary: Box::new(source),
                                    finish: Box::new(finish.source),
                                },
                            )),
                        };
                    }
                    Ok(RepairLayout::CandidateCanonical | RepairLayout::Complete) => {
                        return Err(failure(
                            if source.namespace_observation_was_unstable() {
                                ArchivedStateRepairOutcome::Ambiguous
                            } else {
                                ArchivedStateRepairOutcome::Applied
                            },
                            source,
                        ));
                    }
                    Err(reconciliation) => {
                        return Err(failure(
                            ArchivedStateRepairOutcome::Ambiguous,
                            ArchivedStateRepairError::PreflightReconciliationFailed {
                                operation: "preserve failed archived-repair wrapper",
                                primary: Box::new(source),
                                reconciliation: Box::new(reconciliation),
                            },
                        ));
                    }
                }
            }

            before_namespace_syscall(ArchivedStateRepairNamespaceMove::PreserveFailedCandidate);
            let namespace_result = linux_fs::renameat2_exchange_once(
                &self.roots.file,
                STAGING_NAME,
                &self.quarantine.file,
                &self.quarantine_name,
            )
            .map_err(|source| {
                io_error(
                    "exchange failed archived-repair wrapper with empty replacement",
                    self.quarantine_path.clone(),
                    source,
                )
            });
            let namespace_succeeded = namespace_result.is_ok();
            let operation_result =
                namespace_result.and_then(|()| checkpoint(ArchivedStateRepairFaultPoint::AfterPreservation));

            match self
                .layout()
                .map_err(|source| failure(ArchivedStateRepairOutcome::Ambiguous, source))?
            {
                RepairLayout::Initial => {
                    if namespace_succeeded {
                        // The preservation exchange was externally reversed.
                        // A retry could move an actor's newer layout instead.
                        return Err(failure(
                            ArchivedStateRepairOutcome::Ambiguous,
                            ArchivedStateRepairError::ReportedSuccessWithoutMove {
                                operation: "preserve failed archived-repair wrapper",
                            },
                        ));
                    }
                    let source = operation_result.expect_err("a failed namespace syscall must retain its error");
                    if retries < MAX_EXACT_NOT_APPLIED_RETRIES {
                        self.authorize_preservation_retry(installation, source)?;
                        retries += 1;
                        continue;
                    }
                    return Err(self.preservation_failure_after_reconciliation(
                        installation,
                        "finish failed-wrapper preservation rename reconciliation",
                        ArchivedStateRepairOutcome::NotApplied,
                        source,
                    ));
                }
                RepairLayout::Preserved => {
                    return self.finish_preservation_with_prior(installation, state_db, semantic_failure);
                }
                RepairLayout::CandidateCanonical | RepairLayout::Complete => {
                    return Err(failure(
                        ArchivedStateRepairOutcome::Ambiguous,
                        namespace_error("failed-wrapper preservation produced canonical candidate layout"),
                    ));
                }
            }
        }
    }

    fn preflight_preservation(&self, installation: &Installation) -> Result<(), ArchivedStateRepairError> {
        self.require_retained_base(installation)?;
        self.require_initial_layout()?;
        self.staging
            .sync("sync failed archived-repair wrapper before preservation")
            .map_err(|source| identity("sync failed archived-repair wrapper before preservation", source))?;
        self.replacement
            .sync("sync empty staging replacement before failed-wrapper preservation")
            .map_err(|source| identity("sync empty replacement before failed-wrapper preservation", source))?;
        self.roots
            .sync("sync roots before failed archived-repair preservation")
            .map_err(|source| identity("sync roots before failed archived-repair preservation", source))?;
        self.quarantine
            .sync("sync quarantine before failed archived-repair preservation")
            .map_err(|source| identity("sync quarantine before failed archived-repair preservation", source))?;

        before_preservation();
        self.require_retained_base(installation)?;
        self.require_initial_layout()?;
        checkpoint(ArchivedStateRepairFaultPoint::BeforePreservation)
    }

    fn finish_preservation(
        &self,
        installation: &Installation,
        state_db: &db::state::Database,
    ) -> Result<std::path::PathBuf, ArchivedStateRepairError> {
        checkpoint(ArchivedStateRepairFaultPoint::PreservedWrapperPostSync)?;
        self.staging
            .sync("sync preserved opaque archived-repair wrapper")
            .map_err(|source| identity("sync preserved opaque archived-repair wrapper", source))?;
        checkpoint(ArchivedStateRepairFaultPoint::PreservedReplacementPostSync)?;
        self.replacement
            .sync("sync fixed empty staging wrapper after failed repair")
            .map_err(|source| identity("sync fixed empty staging wrapper after failed repair", source))?;
        checkpoint(ArchivedStateRepairFaultPoint::PreservationRootsSync)?;
        self.roots
            .sync("sync roots after failed archived-repair preservation")
            .map_err(|source| identity("sync roots after failed archived-repair preservation", source))?;
        checkpoint(ArchivedStateRepairFaultPoint::PreservationQuarantineSync)?;
        self.quarantine
            .sync("sync quarantine after failed archived-repair preservation")
            .map_err(|source| identity("sync quarantine after failed archived-repair preservation", source))?;
        checkpoint(ArchivedStateRepairFaultPoint::FinalPreservationRevalidation)?;
        self.require_preserved_boundary(installation, state_db)?;
        Ok(self.quarantine_path.clone())
    }

    fn finish_preservation_with_prior(
        &self,
        installation: &Installation,
        state_db: &db::state::Database,
        prior: Option<ArchivedStateRepairError>,
    ) -> Result<std::path::PathBuf, ArchivedStateRepairFailure> {
        let mut first = None;
        loop {
            match self.finish_preservation(installation, state_db) {
                Ok(path) => {
                    return if let Some(source) = prior {
                        Err(failure(ArchivedStateRepairOutcome::Applied, source))
                    } else {
                        Ok(path)
                    };
                }
                Err(source) => {
                    let source_namespace_uncertain = source.namespace_is_uncertain();
                    let reconciliation = self.require_preserved_boundary(installation, state_db);
                    if let Err(reconciliation) = reconciliation {
                        let outcome = if source_namespace_uncertain || reconciliation.namespace_is_uncertain() {
                            ArchivedStateRepairOutcome::Ambiguous
                        } else {
                            ArchivedStateRepairOutcome::Applied
                        };
                        return Err(failure(
                            outcome,
                            ArchivedStateRepairError::PreflightReconciliationFailed {
                                operation: "reconcile failed-wrapper preservation suffix",
                                primary: Box::new(source),
                                reconciliation: Box::new(reconciliation),
                            },
                        ));
                    }
                    if source_namespace_uncertain {
                        return Err(failure(ArchivedStateRepairOutcome::Ambiguous, source));
                    }
                    let Some(primary) = first.take() else {
                        first = Some(source);
                        before_suffix_retry();
                        continue;
                    };
                    return Err(failure(
                        ArchivedStateRepairOutcome::Applied,
                        ArchivedStateRepairError::AppliedAfterPreflightFailure {
                            operation: "finish failed-wrapper preservation",
                            primary: Box::new(primary),
                            finish: Box::new(source),
                        },
                    ));
                }
            }
        }
    }

    fn authorize_preservation_retry(
        &self,
        installation: &Installation,
        primary: ArchivedStateRepairError,
    ) -> Result<(), ArchivedStateRepairFailure> {
        let unstable = primary.namespace_observation_was_unstable();
        match self.require_opaque_namespace(RepairLayout::Initial, installation) {
            Ok(()) if !unstable => {
                before_suffix_retry();
                Ok(())
            }
            Ok(()) => Err(failure(ArchivedStateRepairOutcome::Ambiguous, primary)),
            Err(reconciliation) => Err(failure(
                ArchivedStateRepairOutcome::Ambiguous,
                ArchivedStateRepairError::PreflightReconciliationFailed {
                    operation: "authorize failed-wrapper preservation retry",
                    primary: Box::new(primary),
                    reconciliation: Box::new(reconciliation),
                },
            )),
        }
    }

    fn preservation_failure_after_reconciliation(
        &self,
        installation: &Installation,
        operation: &'static str,
        exact_outcome: ArchivedStateRepairOutcome,
        primary: ArchivedStateRepairError,
    ) -> ArchivedStateRepairFailure {
        let unstable = primary.namespace_observation_was_unstable();
        match self.require_opaque_namespace(RepairLayout::Initial, installation) {
            Ok(()) => failure(
                if unstable {
                    ArchivedStateRepairOutcome::Ambiguous
                } else {
                    exact_outcome
                },
                primary,
            ),
            Err(reconciliation) => failure(
                ArchivedStateRepairOutcome::Ambiguous,
                ArchivedStateRepairError::PreflightReconciliationFailed {
                    operation,
                    primary: Box::new(primary),
                    reconciliation: Box::new(reconciliation),
                },
            ),
        }
    }
}

fn namespace_error(message: &'static str) -> ArchivedStateRepairError {
    ArchivedStateRepairError::Io {
        operation: message,
        path: std::path::PathBuf::from("<retained-archived-state-repair-namespace>"),
        source: std::io::Error::other(message),
    }
}
