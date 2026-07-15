use super::{
    ArchiveBaseline, ArchivedStateRepairError, ArchivedStateRepairFailure, ArchivedStateRepairIdentity,
    ArchivedStateRepairOutcome, ArchivedStateRepairPublication, STAGING_NAME,
    error::{failure, identity, io_error},
    fault_injection::{
        ArchivedStateRepairFaultPoint, ArchivedStateRepairNamespaceMove, before_cleanup, before_namespace_syscall,
        before_publication, before_suffix_retry, checkpoint,
    },
    layout::RepairLayout,
};
use crate::{Installation, db, linux_fs};

const MAX_EXACT_NOT_APPLIED_RETRIES: usize = 1;

impl ArchivedStateRepairIdentity {
    /// Publish the whole repaired wrapper and restore the fixed staging name
    /// to the pre-reserved exact empty 0700 wrapper.
    ///
    /// All bounded retry and suffix resumption is internal. Callers must never
    /// call this method again after an `Applied` or `Ambiguous` result.
    pub(crate) fn publish(
        &self,
        installation: &Installation,
        state_db: &db::state::Database,
    ) -> Result<ArchivedStateRepairPublication, ArchivedStateRepairFailure> {
        let _operation = self.operation.lock().map_err(|_| {
            failure(
                ArchivedStateRepairOutcome::Ambiguous,
                ArchivedStateRepairError::OperationLockPoisoned,
            )
        })?;
        self.verify_candidate_snapshot(installation, state_db)
            .map_err(|source| {
                let outcome = if source.namespace_is_uncertain() {
                    ArchivedStateRepairOutcome::Ambiguous
                } else {
                    ArchivedStateRepairOutcome::NotApplied
                };
                failure(outcome, source)
            })?;

        match self.archive {
            ArchiveBaseline::Existing(_) => self.publish_over_existing(installation, state_db),
            ArchiveBaseline::Missing => self.publish_missing(installation, state_db),
        }
    }

    fn publish_over_existing(
        &self,
        installation: &Installation,
        state_db: &db::state::Database,
    ) -> Result<ArchivedStateRepairPublication, ArchivedStateRepairFailure> {
        let mut retries = 0usize;
        loop {
            match self
                .layout()
                .map_err(|source| failure(ArchivedStateRepairOutcome::Ambiguous, source))?
            {
                RepairLayout::Initial => {}
                RepairLayout::CandidateCanonical => {
                    self.cleanup_existing(installation, state_db)?;
                    return self.finish_existing(installation, state_db);
                }
                RepairLayout::Complete => return self.finish_existing(installation, state_db),
                RepairLayout::Preserved => {
                    return Err(failure(
                        ArchivedStateRepairOutcome::Ambiguous,
                        namespace_error("publication observed a previously preserved candidate"),
                    ));
                }
            }

            let preflight = self.preflight_publication(installation, state_db);
            if let Err(source) = preflight {
                match self.layout() {
                    Ok(RepairLayout::Initial) if retries < MAX_EXACT_NOT_APPLIED_RETRIES => {
                        self.authorize_candidate_retry(
                            RepairLayout::Initial,
                            installation,
                            state_db,
                            "retry existing-archive publication preflight",
                            ArchivedStateRepairOutcome::NotApplied,
                            source,
                        )?;
                        retries += 1;
                        continue;
                    }
                    Ok(RepairLayout::Initial) => {
                        return Err(self.candidate_failure_after_reconciliation(
                            RepairLayout::Initial,
                            installation,
                            state_db,
                            "finish existing-archive publication preflight reconciliation",
                            ArchivedStateRepairOutcome::NotApplied,
                            source,
                        ));
                    }
                    Ok(RepairLayout::CandidateCanonical | RepairLayout::Complete) => {
                        if source.namespace_observation_was_unstable() {
                            return Err(failure(ArchivedStateRepairOutcome::Ambiguous, source));
                        }
                        let finish = self
                            .cleanup_existing(installation, state_db)
                            .and_then(|()| self.finish_existing(installation, state_db).map(|_| ()))
                            .err()
                            .map(|failure| failure.source)
                            .unwrap_or_else(|| namespace_error("publication applied during failed preflight"));
                        return Err(failure(
                            ArchivedStateRepairOutcome::Applied,
                            ArchivedStateRepairError::AppliedAfterPreflightFailure {
                                operation: "replace existing archived wrapper",
                                primary: Box::new(source),
                                finish: Box::new(finish),
                            },
                        ));
                    }
                    Ok(RepairLayout::Preserved) => {
                        return Err(failure(ArchivedStateRepairOutcome::Ambiguous, source));
                    }
                    Err(reconciliation) => {
                        return Err(failure(
                            ArchivedStateRepairOutcome::Ambiguous,
                            ArchivedStateRepairError::PreflightReconciliationFailed {
                                operation: "replace existing archived wrapper",
                                primary: Box::new(source),
                                reconciliation: Box::new(reconciliation),
                            },
                        ));
                    }
                }
            }

            before_namespace_syscall(ArchivedStateRepairNamespaceMove::PublishExisting);
            let namespace_result =
                linux_fs::renameat2_exchange_once(&self.roots.file, STAGING_NAME, &self.roots.file, &self.state_name)
                    .map_err(|source| {
                        io_error(
                            "exchange staged repair with existing archived wrapper",
                            self.roots.path.clone(),
                            source,
                        )
                    });
            let namespace_succeeded = namespace_result.is_ok();
            let operation_result =
                namespace_result.and_then(|()| checkpoint(ArchivedStateRepairFaultPoint::AfterPublication));

            match self
                .layout()
                .map_err(|source| failure(ArchivedStateRepairOutcome::Ambiguous, source))?
            {
                RepairLayout::Initial => {
                    if namespace_succeeded {
                        // An exact original layout after kernel success proves
                        // an inverse raced this call. A retry would be a new,
                        // unauthorized namespace mutation.
                        return Err(failure(
                            ArchivedStateRepairOutcome::Ambiguous,
                            ArchivedStateRepairError::ReportedSuccessWithoutMove {
                                operation: "replace existing archived wrapper",
                            },
                        ));
                    }
                    let source = operation_result.expect_err("a failed namespace syscall must retain its error");
                    if retries < MAX_EXACT_NOT_APPLIED_RETRIES {
                        self.authorize_candidate_retry(
                            RepairLayout::Initial,
                            installation,
                            state_db,
                            "retry existing-archive publication rename",
                            ArchivedStateRepairOutcome::NotApplied,
                            source,
                        )?;
                        retries += 1;
                        continue;
                    }
                    return Err(self.candidate_failure_after_reconciliation(
                        RepairLayout::Initial,
                        installation,
                        state_db,
                        "finish existing-archive publication rename reconciliation",
                        ArchivedStateRepairOutcome::NotApplied,
                        source,
                    ));
                }
                RepairLayout::CandidateCanonical => {
                    self.cleanup_existing(installation, state_db)?;
                    return self.finish_existing(installation, state_db);
                }
                RepairLayout::Complete => return self.finish_existing(installation, state_db),
                RepairLayout::Preserved => {
                    return Err(failure(
                        ArchivedStateRepairOutcome::Ambiguous,
                        namespace_error("publication unexpectedly observed preserved layout"),
                    ));
                }
            }
        }
    }

    fn publish_missing(
        &self,
        installation: &Installation,
        state_db: &db::state::Database,
    ) -> Result<ArchivedStateRepairPublication, ArchivedStateRepairFailure> {
        let mut retries = 0usize;
        loop {
            match self
                .layout()
                .map_err(|source| failure(ArchivedStateRepairOutcome::Ambiguous, source))?
            {
                RepairLayout::Initial => {}
                RepairLayout::CandidateCanonical => {
                    self.cleanup_missing(installation, state_db)?;
                    return self.finish_missing(installation, state_db);
                }
                RepairLayout::Complete => return self.finish_missing(installation, state_db),
                RepairLayout::Preserved => {
                    return Err(failure(
                        ArchivedStateRepairOutcome::Ambiguous,
                        namespace_error("missing-state publication observed preserved layout"),
                    ));
                }
            }

            let preflight = self.preflight_publication(installation, state_db);
            if let Err(source) = preflight {
                match self.layout() {
                    Ok(RepairLayout::Initial) if retries < MAX_EXACT_NOT_APPLIED_RETRIES => {
                        self.authorize_candidate_retry(
                            RepairLayout::Initial,
                            installation,
                            state_db,
                            "retry missing-archive publication preflight",
                            ArchivedStateRepairOutcome::NotApplied,
                            source,
                        )?;
                        retries += 1;
                        continue;
                    }
                    Ok(RepairLayout::Initial) => {
                        return Err(self.candidate_failure_after_reconciliation(
                            RepairLayout::Initial,
                            installation,
                            state_db,
                            "finish missing-archive publication preflight reconciliation",
                            ArchivedStateRepairOutcome::NotApplied,
                            source,
                        ));
                    }
                    Ok(RepairLayout::CandidateCanonical | RepairLayout::Complete) => {
                        if source.namespace_observation_was_unstable() {
                            return Err(failure(ArchivedStateRepairOutcome::Ambiguous, source));
                        }
                        let finish = self
                            .cleanup_missing(installation, state_db)
                            .and_then(|()| self.finish_missing(installation, state_db).map(|_| ()))
                            .err()
                            .map(|failure| failure.source)
                            .unwrap_or_else(|| {
                                namespace_error("missing-state publication applied during failed preflight")
                            });
                        return Err(failure(
                            ArchivedStateRepairOutcome::Applied,
                            ArchivedStateRepairError::AppliedAfterPreflightFailure {
                                operation: "publish missing archived wrapper",
                                primary: Box::new(source),
                                finish: Box::new(finish),
                            },
                        ));
                    }
                    Ok(RepairLayout::Preserved) => {
                        return Err(failure(ArchivedStateRepairOutcome::Ambiguous, source));
                    }
                    Err(reconciliation) => {
                        return Err(failure(
                            ArchivedStateRepairOutcome::Ambiguous,
                            ArchivedStateRepairError::PreflightReconciliationFailed {
                                operation: "publish missing archived wrapper",
                                primary: Box::new(source),
                                reconciliation: Box::new(reconciliation),
                            },
                        ));
                    }
                }
            }

            before_namespace_syscall(ArchivedStateRepairNamespaceMove::PublishMissing);
            let namespace_result =
                linux_fs::renameat2_noreplace_once(&self.roots.file, STAGING_NAME, &self.roots.file, &self.state_name)
                    .map_err(|source| {
                        io_error(
                            "publish staged repair at missing archived-state name",
                            self.roots.path.clone(),
                            source,
                        )
                    });
            let namespace_succeeded = namespace_result.is_ok();
            let operation_result =
                namespace_result.and_then(|()| checkpoint(ArchivedStateRepairFaultPoint::AfterPublication));

            match self
                .layout()
                .map_err(|source| failure(ArchivedStateRepairOutcome::Ambiguous, source))?
            {
                RepairLayout::Initial => {
                    if namespace_succeeded {
                        // Treat even an otherwise impossible NOREPLACE success
                        // as uncertain chronology; never turn it into a retry.
                        return Err(failure(
                            ArchivedStateRepairOutcome::Ambiguous,
                            ArchivedStateRepairError::ReportedSuccessWithoutMove {
                                operation: "publish missing archived wrapper",
                            },
                        ));
                    }
                    let source = operation_result.expect_err("a failed namespace syscall must retain its error");
                    if retries < MAX_EXACT_NOT_APPLIED_RETRIES {
                        self.authorize_candidate_retry(
                            RepairLayout::Initial,
                            installation,
                            state_db,
                            "retry missing-archive publication rename",
                            ArchivedStateRepairOutcome::NotApplied,
                            source,
                        )?;
                        retries += 1;
                        continue;
                    }
                    return Err(self.candidate_failure_after_reconciliation(
                        RepairLayout::Initial,
                        installation,
                        state_db,
                        "finish missing-archive publication rename reconciliation",
                        ArchivedStateRepairOutcome::NotApplied,
                        source,
                    ));
                }
                RepairLayout::CandidateCanonical => {
                    self.cleanup_missing(installation, state_db)?;
                    return self.finish_missing(installation, state_db);
                }
                RepairLayout::Complete => return self.finish_missing(installation, state_db),
                RepairLayout::Preserved => {
                    return Err(failure(
                        ArchivedStateRepairOutcome::Ambiguous,
                        namespace_error("missing-state publication unexpectedly observed preserved layout"),
                    ));
                }
            }
        }
    }

    fn preflight_publication(
        &self,
        installation: &Installation,
        state_db: &db::state::Database,
    ) -> Result<(), ArchivedStateRepairError> {
        checkpoint(ArchivedStateRepairFaultPoint::CandidatePreSync)?;
        self.candidate
            .store
            .sync_retained_tree()
            .map_err(super::super::Error::from)
            .map_err(|source| identity("sync archived-repair candidate before publication", source))?;
        checkpoint(ArchivedStateRepairFaultPoint::StagingPreSync)?;
        self.staging
            .sync("sync archived-repair staging wrapper before publication")
            .map_err(|source| identity("sync archived-repair staging wrapper before publication", source))?;
        if let ArchiveBaseline::Existing(old) = &self.archive {
            checkpoint(ArchivedStateRepairFaultPoint::CanonicalPreSync)?;
            old.sync("sync opaque archived wrapper before replacement")
                .map_err(|source| identity("sync opaque archived wrapper before replacement", source))?;
        }
        checkpoint(ArchivedStateRepairFaultPoint::ReplacementPreSync)?;
        self.replacement
            .sync("sync empty archived-repair replacement before publication")
            .map_err(|source| identity("sync empty archived-repair replacement before publication", source))?;
        self.roots
            .sync("sync roots before archived-state repair publication")
            .map_err(|source| identity("sync roots before archived-state repair publication", source))?;
        self.quarantine
            .sync("sync quarantine before archived-state repair publication")
            .map_err(|source| identity("sync quarantine before archived-state repair publication", source))?;

        before_publication();
        self.require_candidate_boundary(RepairLayout::Initial, installation, state_db)?;
        checkpoint(ArchivedStateRepairFaultPoint::BeforePublication)
    }

    fn cleanup_existing(
        &self,
        installation: &Installation,
        state_db: &db::state::Database,
    ) -> Result<(), ArchivedStateRepairFailure> {
        let mut retries = 0usize;
        loop {
            match self
                .layout()
                .map_err(|source| failure(ArchivedStateRepairOutcome::Ambiguous, source))?
            {
                RepairLayout::Complete => return Ok(()),
                RepairLayout::CandidateCanonical => {}
                _ => {
                    return Err(failure(
                        ArchivedStateRepairOutcome::Ambiguous,
                        namespace_error("existing-state cleanup lost sticky canonical candidate"),
                    ));
                }
            }
            if let Err(source) = self.preflight_cleanup(installation, state_db) {
                match self.layout() {
                    Ok(RepairLayout::CandidateCanonical) if retries < MAX_EXACT_NOT_APPLIED_RETRIES => {
                        self.authorize_candidate_retry(
                            RepairLayout::CandidateCanonical,
                            installation,
                            state_db,
                            "retry existing-archive cleanup preflight",
                            ArchivedStateRepairOutcome::Applied,
                            source,
                        )?;
                        retries += 1;
                        continue;
                    }
                    Ok(RepairLayout::CandidateCanonical) => {
                        return Err(self.candidate_failure_after_reconciliation(
                            RepairLayout::CandidateCanonical,
                            installation,
                            state_db,
                            "finish existing-archive cleanup preflight reconciliation",
                            ArchivedStateRepairOutcome::Applied,
                            source,
                        ));
                    }
                    Ok(RepairLayout::Complete) => {
                        if source.namespace_observation_was_unstable() {
                            return Err(failure(ArchivedStateRepairOutcome::Ambiguous, source));
                        }
                        self.require_candidate_boundary(RepairLayout::Complete, installation, state_db)
                            .map_err(|reconciliation| {
                                failure(
                                    if reconciliation.namespace_is_uncertain() {
                                        ArchivedStateRepairOutcome::Ambiguous
                                    } else {
                                        ArchivedStateRepairOutcome::Applied
                                    },
                                    ArchivedStateRepairError::PreflightReconciliationFailed {
                                        operation: "adopt externally completed existing-archive cleanup",
                                        primary: Box::new(source),
                                        reconciliation: Box::new(reconciliation),
                                    },
                                )
                            })?;
                        return Ok(());
                    }
                    Ok(_) => return Err(failure(ArchivedStateRepairOutcome::Ambiguous, source)),
                    Err(reconciliation) => {
                        return Err(failure(
                            ArchivedStateRepairOutcome::Ambiguous,
                            ArchivedStateRepairError::PreflightReconciliationFailed {
                                operation: "quarantine displaced archived wrapper",
                                primary: Box::new(source),
                                reconciliation: Box::new(reconciliation),
                            },
                        ));
                    }
                }
            }

            before_namespace_syscall(ArchivedStateRepairNamespaceMove::CleanupExisting);
            let namespace_result = linux_fs::renameat2_exchange_once(
                &self.roots.file,
                STAGING_NAME,
                &self.quarantine.file,
                &self.quarantine_name,
            )
            .map_err(|source| {
                io_error(
                    "exchange displaced archived wrapper with empty replacement",
                    self.quarantine_path.clone(),
                    source,
                )
            });
            let namespace_succeeded = namespace_result.is_ok();
            let operation_result =
                namespace_result.and_then(|()| checkpoint(ArchivedStateRepairFaultPoint::AfterCleanup));

            match self
                .layout()
                .map_err(|source| failure(ArchivedStateRepairOutcome::Ambiguous, source))?
            {
                RepairLayout::CandidateCanonical => {
                    if namespace_succeeded {
                        // The cleanup was reversed after kernel success. Do not
                        // exchange the names a second time based on stale intent.
                        return Err(failure(
                            ArchivedStateRepairOutcome::Ambiguous,
                            ArchivedStateRepairError::ReportedSuccessWithoutMove {
                                operation: "quarantine displaced archived wrapper",
                            },
                        ));
                    }
                    let source = operation_result.expect_err("a failed namespace syscall must retain its error");
                    if retries < MAX_EXACT_NOT_APPLIED_RETRIES {
                        self.authorize_candidate_retry(
                            RepairLayout::CandidateCanonical,
                            installation,
                            state_db,
                            "retry existing-archive cleanup rename",
                            ArchivedStateRepairOutcome::Applied,
                            source,
                        )?;
                        retries += 1;
                        continue;
                    }
                    return Err(self.candidate_failure_after_reconciliation(
                        RepairLayout::CandidateCanonical,
                        installation,
                        state_db,
                        "finish existing-archive cleanup rename reconciliation",
                        ArchivedStateRepairOutcome::Applied,
                        source,
                    ));
                }
                RepairLayout::Complete => return Ok(()),
                _ => {
                    return Err(failure(
                        ArchivedStateRepairOutcome::Ambiguous,
                        namespace_error("existing-state cleanup produced an unrecognized layout"),
                    ));
                }
            }
        }
    }

    fn cleanup_missing(
        &self,
        installation: &Installation,
        state_db: &db::state::Database,
    ) -> Result<(), ArchivedStateRepairFailure> {
        let mut retries = 0usize;
        loop {
            match self
                .layout()
                .map_err(|source| failure(ArchivedStateRepairOutcome::Ambiguous, source))?
            {
                RepairLayout::Complete => return Ok(()),
                RepairLayout::CandidateCanonical => {}
                _ => {
                    return Err(failure(
                        ArchivedStateRepairOutcome::Ambiguous,
                        namespace_error("missing-state cleanup lost sticky canonical candidate"),
                    ));
                }
            }
            if let Err(source) = self.preflight_cleanup(installation, state_db) {
                match self.layout() {
                    Ok(RepairLayout::CandidateCanonical) if retries < MAX_EXACT_NOT_APPLIED_RETRIES => {
                        self.authorize_candidate_retry(
                            RepairLayout::CandidateCanonical,
                            installation,
                            state_db,
                            "retry missing-archive cleanup preflight",
                            ArchivedStateRepairOutcome::Applied,
                            source,
                        )?;
                        retries += 1;
                        continue;
                    }
                    Ok(RepairLayout::CandidateCanonical) => {
                        return Err(self.candidate_failure_after_reconciliation(
                            RepairLayout::CandidateCanonical,
                            installation,
                            state_db,
                            "finish missing-archive cleanup preflight reconciliation",
                            ArchivedStateRepairOutcome::Applied,
                            source,
                        ));
                    }
                    Ok(RepairLayout::Complete) => {
                        if source.namespace_observation_was_unstable() {
                            return Err(failure(ArchivedStateRepairOutcome::Ambiguous, source));
                        }
                        self.require_candidate_boundary(RepairLayout::Complete, installation, state_db)
                            .map_err(|reconciliation| {
                                failure(
                                    if reconciliation.namespace_is_uncertain() {
                                        ArchivedStateRepairOutcome::Ambiguous
                                    } else {
                                        ArchivedStateRepairOutcome::Applied
                                    },
                                    ArchivedStateRepairError::PreflightReconciliationFailed {
                                        operation: "adopt externally completed missing-archive cleanup",
                                        primary: Box::new(source),
                                        reconciliation: Box::new(reconciliation),
                                    },
                                )
                            })?;
                        return Ok(());
                    }
                    Ok(_) => return Err(failure(ArchivedStateRepairOutcome::Ambiguous, source)),
                    Err(reconciliation) => {
                        return Err(failure(
                            ArchivedStateRepairOutcome::Ambiguous,
                            ArchivedStateRepairError::PreflightReconciliationFailed {
                                operation: "restore fixed staging wrapper",
                                primary: Box::new(source),
                                reconciliation: Box::new(reconciliation),
                            },
                        ));
                    }
                }
            }

            before_namespace_syscall(ArchivedStateRepairNamespaceMove::CleanupMissing);
            let namespace_result = linux_fs::renameat2_noreplace_once(
                &self.quarantine.file,
                &self.quarantine_name,
                &self.roots.file,
                STAGING_NAME,
            )
            .map_err(|source| {
                io_error(
                    "restore fixed staging wrapper after missing-state publication",
                    self.staging.path.clone(),
                    source,
                )
            });
            let namespace_succeeded = namespace_result.is_ok();
            let operation_result =
                namespace_result.and_then(|()| checkpoint(ArchivedStateRepairFaultPoint::AfterCleanup));

            match self
                .layout()
                .map_err(|source| failure(ArchivedStateRepairOutcome::Ambiguous, source))?
            {
                RepairLayout::CandidateCanonical => {
                    if namespace_succeeded {
                        // A successful restore cannot legitimately reconcile
                        // to the pre-call layout. Fail closed without retrying.
                        return Err(failure(
                            ArchivedStateRepairOutcome::Ambiguous,
                            ArchivedStateRepairError::ReportedSuccessWithoutMove {
                                operation: "restore fixed staging wrapper",
                            },
                        ));
                    }
                    let source = operation_result.expect_err("a failed namespace syscall must retain its error");
                    if retries < MAX_EXACT_NOT_APPLIED_RETRIES {
                        self.authorize_candidate_retry(
                            RepairLayout::CandidateCanonical,
                            installation,
                            state_db,
                            "retry missing-archive cleanup rename",
                            ArchivedStateRepairOutcome::Applied,
                            source,
                        )?;
                        retries += 1;
                        continue;
                    }
                    return Err(self.candidate_failure_after_reconciliation(
                        RepairLayout::CandidateCanonical,
                        installation,
                        state_db,
                        "finish missing-archive cleanup rename reconciliation",
                        ArchivedStateRepairOutcome::Applied,
                        source,
                    ));
                }
                RepairLayout::Complete => return Ok(()),
                _ => {
                    return Err(failure(
                        ArchivedStateRepairOutcome::Ambiguous,
                        namespace_error("missing-state cleanup produced an unrecognized layout"),
                    ));
                }
            }
        }
    }

    fn preflight_cleanup(
        &self,
        installation: &Installation,
        state_db: &db::state::Database,
    ) -> Result<(), ArchivedStateRepairError> {
        checkpoint(ArchivedStateRepairFaultPoint::StagingPreSync)?;
        if let ArchiveBaseline::Existing(old) = &self.archive {
            old.sync("sync displaced archived wrapper before cleanup")
                .map_err(|source| identity("sync displaced archived wrapper before cleanup", source))?;
        }
        checkpoint(ArchivedStateRepairFaultPoint::ReplacementPreSync)?;
        self.replacement
            .sync("sync empty replacement before archived-repair cleanup")
            .map_err(|source| identity("sync empty replacement before archived-repair cleanup", source))?;
        self.roots
            .sync("sync roots before archived-repair cleanup")
            .map_err(|source| identity("sync roots before archived-repair cleanup", source))?;
        self.quarantine
            .sync("sync quarantine before archived-repair cleanup")
            .map_err(|source| identity("sync quarantine before archived-repair cleanup", source))?;
        before_cleanup();
        self.require_candidate_boundary(RepairLayout::CandidateCanonical, installation, state_db)?;
        checkpoint(ArchivedStateRepairFaultPoint::BeforeCleanup)
    }

    fn authorize_candidate_retry(
        &self,
        expected: RepairLayout,
        installation: &Installation,
        state_db: &db::state::Database,
        operation: &'static str,
        exact_outcome: ArchivedStateRepairOutcome,
        primary: ArchivedStateRepairError,
    ) -> Result<(), ArchivedStateRepairFailure> {
        let primary_namespace_uncertain = primary.namespace_is_uncertain();
        match self.require_candidate_boundary(expected, installation, state_db) {
            Ok(()) if !primary_namespace_uncertain => {
                // The hook runs only after a complete proof. The retry loop
                // must sample and prove the layout again before any mutation,
                // so a substitution here cannot make the proof stale.
                before_suffix_retry();
                Ok(())
            }
            Ok(()) => Err(failure(ArchivedStateRepairOutcome::Ambiguous, primary)),
            Err(reconciliation) => {
                let outcome = if primary_namespace_uncertain || reconciliation.namespace_is_uncertain() {
                    ArchivedStateRepairOutcome::Ambiguous
                } else {
                    exact_outcome
                };
                Err(failure(
                    outcome,
                    ArchivedStateRepairError::PreflightReconciliationFailed {
                        operation,
                        primary: Box::new(primary),
                        reconciliation: Box::new(reconciliation),
                    },
                ))
            }
        }
    }

    fn candidate_failure_after_reconciliation(
        &self,
        expected: RepairLayout,
        installation: &Installation,
        state_db: &db::state::Database,
        operation: &'static str,
        exact_outcome: ArchivedStateRepairOutcome,
        primary: ArchivedStateRepairError,
    ) -> ArchivedStateRepairFailure {
        let primary_namespace_uncertain = primary.namespace_is_uncertain();
        match self.require_candidate_boundary(expected, installation, state_db) {
            Ok(()) => failure(
                if primary_namespace_uncertain {
                    ArchivedStateRepairOutcome::Ambiguous
                } else {
                    exact_outcome
                },
                primary,
            ),
            Err(reconciliation) => failure(
                if primary_namespace_uncertain || reconciliation.namespace_is_uncertain() {
                    ArchivedStateRepairOutcome::Ambiguous
                } else {
                    exact_outcome
                },
                ArchivedStateRepairError::PreflightReconciliationFailed {
                    operation,
                    primary: Box::new(primary),
                    reconciliation: Box::new(reconciliation),
                },
            ),
        }
    }

    fn finish_existing(
        &self,
        installation: &Installation,
        state_db: &db::state::Database,
    ) -> Result<ArchivedStateRepairPublication, ArchivedStateRepairFailure> {
        self.finish_complete_bounded(installation, state_db)?;
        Ok(ArchivedStateRepairPublication::Replaced {
            displaced_wrapper: self.quarantine_path.clone(),
        })
    }

    fn finish_missing(
        &self,
        installation: &Installation,
        state_db: &db::state::Database,
    ) -> Result<ArchivedStateRepairPublication, ArchivedStateRepairFailure> {
        self.finish_complete_bounded(installation, state_db)?;
        Ok(ArchivedStateRepairPublication::Published)
    }

    fn finish_complete_bounded(
        &self,
        installation: &Installation,
        state_db: &db::state::Database,
    ) -> Result<(), ArchivedStateRepairFailure> {
        let mut first = None;
        loop {
            match self.finish_complete(installation, state_db) {
                Ok(()) => return Ok(()),
                Err(source) => {
                    let source_namespace_uncertain = source.namespace_is_uncertain();
                    let reconciliation =
                        self.require_candidate_boundary(RepairLayout::Complete, installation, state_db);
                    if let Err(reconciliation) = reconciliation {
                        let outcome = if source_namespace_uncertain || reconciliation.namespace_is_uncertain() {
                            ArchivedStateRepairOutcome::Ambiguous
                        } else {
                            ArchivedStateRepairOutcome::Applied
                        };
                        return Err(failure(
                            outcome,
                            ArchivedStateRepairError::PreflightReconciliationFailed {
                                operation: "reconcile committed archived-state repair suffix",
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
                            operation: "finish committed archived-state repair",
                            primary: Box::new(primary),
                            finish: Box::new(source),
                        },
                    ));
                }
            }
        }
    }

    fn finish_complete(
        &self,
        installation: &Installation,
        state_db: &db::state::Database,
    ) -> Result<(), ArchivedStateRepairError> {
        checkpoint(ArchivedStateRepairFaultPoint::CandidatePostSync)?;
        self.candidate
            .store
            .sync_retained_tree()
            .map_err(super::super::Error::from)
            .map_err(|source| identity("sync published archived-repair candidate", source))?;
        checkpoint(ArchivedStateRepairFaultPoint::StagingPostSync)?;
        self.replacement
            .sync("sync fixed empty staging wrapper after archived repair")
            .map_err(|source| identity("sync fixed empty staging wrapper after archived repair", source))?;
        checkpoint(ArchivedStateRepairFaultPoint::RetainedPayloadPostSync)?;
        match &self.archive {
            ArchiveBaseline::Existing(old) => old
                .sync("sync quarantined displaced archived wrapper")
                .map_err(|source| identity("sync quarantined displaced archived wrapper", source))?,
            ArchiveBaseline::Missing => self
                .replacement
                .sync("resync restored empty staging wrapper")
                .map_err(|source| identity("resync restored empty staging wrapper", source))?,
        }
        checkpoint(ArchivedStateRepairFaultPoint::RootsParentSync)?;
        self.roots
            .sync("sync roots after archived-state repair")
            .map_err(|source| identity("sync roots after archived-state repair", source))?;
        checkpoint(ArchivedStateRepairFaultPoint::QuarantineParentSync)?;
        self.quarantine
            .sync("sync quarantine after archived-state repair")
            .map_err(|source| identity("sync quarantine after archived-state repair", source))?;
        checkpoint(ArchivedStateRepairFaultPoint::FinalRevalidation)?;
        self.require_candidate_boundary(RepairLayout::Complete, installation, state_db)
    }
}

fn namespace_error(message: &'static str) -> ArchivedStateRepairError {
    ArchivedStateRepairError::Io {
        operation: message,
        path: std::path::PathBuf::from("<retained-archived-state-repair-namespace>"),
        source: std::io::Error::other(message),
    }
}
