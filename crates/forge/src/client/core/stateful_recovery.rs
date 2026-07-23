impl Client {
    fn preserve_unswapped_candidate<F>(
        &self,
        candidate: state::Id,
        previous: Option<state::Id>,
        candidate_origin: StatefulCandidateOrigin,
        primary: Error,
        tree_identity: &StatefulTreeIdentity,
        checkpoint: &mut F,
    ) -> Error
    where
        F: FnMut(StatefulTransitionCheckpoint) -> Result<(), Error>,
    {
        let mut failures = StatefulRecoveryFailures::default();
        self.recover_failed_candidate(
            candidate,
            candidate_origin,
            false,
            tree_identity,
            checkpoint,
            &mut failures,
        );

        if failures.is_empty() {
            Error::StatefulCandidatePreserved {
                candidate,
                previous,
                primary: Box::new(primary),
            }
        } else {
            Error::StatefulTransitionRecoveryFailed {
                candidate,
                previous,
                primary: Box::new(primary),
                previous_archive_cleanup: None,
                restore_previous: None,
                reverse_exchange: None,
                preserve_candidate: failures.preserve_candidate,
                invalidate_candidate: failures.invalidate_candidate,
                repair_boot: None,
            }
        }
    }

    fn recover_swapped_candidate<F>(
        &self,
        candidate: state::Id,
        previous: Option<&State>,
        candidate_origin: StatefulCandidateOrigin,
        previous_location: PreviousUsrLocation,
        previous_archive_cleanup: Option<state::Id>,
        system_triggers_incomplete: bool,
        candidate_boot_synchronization_started: bool,
        primary: Error,
        tree_identity: &StatefulTreeIdentity,
        checkpoint: &mut F,
    ) -> Error
    where
        F: FnMut(StatefulTransitionCheckpoint) -> Result<(), Error>,
    {
        let previous_id = previous.map(|state| state.id);
        let mut failures = StatefulRecoveryFailures::default();

        if let Some(previous) = previous_archive_cleanup
            && let Err(error) = tree_identity.finish_not_applied_previous_archive(&self.installation, previous)
        {
            failures.previous_archive_cleanup = Some(Box::new(error.into()));
        }

        if let PreviousUsrLocation::Archived(previous) = previous_location {
            let restored = tree_identity
                .verify_candidate_for_recovery(&self.installation.root.join("usr"))
                .map_err(Error::from)
                .and_then(|()| {
                    tree_identity
                        .verify_previous_for_recovery(&self.installation.root_path(previous.to_string()).join("usr"))
                        .map_err(Error::from)
                })
                .and_then(|()| checkpoint(StatefulTransitionCheckpoint::BeforeRecoveryPreviousStateRestore))
                .and_then(|()| self.restore_previous_to_staging(tree_identity, previous))
                .and_then(|()| {
                    tree_identity
                        .verify_previous_for_recovery(&self.installation.staging_path("usr"))
                        .map_err(Error::from)
                });
            if let Err(error) = restored {
                failures.restore_previous = Some(Box::new(error));
                return self.stateful_recovery_error(candidate, previous_id, primary, failures);
            }
        }

        let reversed = tree_identity
            .verify_forward_exchange(
                &self.installation.root.join("usr"),
                &self.installation.staging_path("usr"),
            )
            .map_err(Error::from)
            .and_then(|()| checkpoint(StatefulTransitionCheckpoint::BeforeRecoveryUsrExchange))
            .and_then(|()| self.exchange_staging_and_live_usr(tree_identity))
            .and_then(|()| {
                tree_identity
                    .verify_restored(
                        &self.installation.root.join("usr"),
                        &self.installation.staging_path("usr"),
                    )
                    .map_err(Error::from)
            });
        if let Err(error) = reversed {
            failures.reverse_exchange = Some(Box::new(error));
            return self.stateful_recovery_error(candidate, previous_id, primary, failures);
        }

        // Once the reverse exchange succeeds, the failed candidate is safely
        // back in staging. Candidate preservation and restored-state boot repair
        // are independent recovery steps, so attempt both and retain both
        // errors if necessary.
        self.recover_failed_candidate(
            candidate,
            candidate_origin,
            system_triggers_incomplete,
            tree_identity,
            checkpoint,
            &mut failures,
        );

        if candidate_boot_synchronization_started {
            let repair: Result<(), Error> = checkpoint(StatefulTransitionCheckpoint::BeforeRecoveryBootSynchronization)
                .and_then(|()| {
                    let Some(previous) = previous else {
                        return Err(Error::StatefulBootRepairUnverified {
                            candidate,
                            previous: None,
                        });
                    };

                    let authority = tree_identity
                        .authorize_legacy_boot_repair(&self.installation, &self.state_db)
                        .map_err(legacy_boot_repair_authority_error)?;
                    match legacy_boot_repair::synchronize(self, previous, authority) {
                        Ok(()) => Err(Error::StatefulBootRepairUnverified {
                            candidate,
                            previous: Some(previous.id),
                        }),
                        Err(legacy_boot_repair::Error::Boot(error)) => Err(Error::Boot(error)),
                        Err(legacy_boot_repair::Error::Authority(source)) => {
                            Err(legacy_boot_repair_authority_error(source))
                        }
                    }
                });
            if let Err(error) = repair {
                failures.repair_boot = Some(Box::new(error));
            }
        }

        self.stateful_recovery_error(candidate, previous_id, primary, failures)
    }

    fn stateful_recovery_error(
        &self,
        candidate: state::Id,
        previous: Option<state::Id>,
        primary: Error,
        failures: StatefulRecoveryFailures,
    ) -> Error {
        if failures.is_empty() {
            Error::StatefulTransitionUsrRestored {
                candidate,
                previous,
                primary: Box::new(primary),
            }
        } else {
            Error::StatefulTransitionRecoveryFailed {
                candidate,
                previous,
                primary: Box::new(primary),
                previous_archive_cleanup: failures.previous_archive_cleanup,
                restore_previous: failures.restore_previous,
                reverse_exchange: failures.reverse_exchange,
                preserve_candidate: failures.preserve_candidate,
                invalidate_candidate: failures.invalidate_candidate,
                repair_boot: failures.repair_boot,
            }
        }
    }

    fn recover_failed_candidate<F>(
        &self,
        candidate: state::Id,
        candidate_origin: StatefulCandidateOrigin,
        quarantine_archived_candidate: bool,
        tree_identity: &StatefulTreeIdentity,
        checkpoint: &mut F,
        failures: &mut StatefulRecoveryFailures,
    ) where
        F: FnMut(StatefulTransitionCheckpoint) -> Result<(), Error>,
    {
        let preflight = tree_identity
            .verify_candidate_for_recovery(&self.installation.staging_path("usr"))
            .map_err(Error::from)
            .and_then(|()| checkpoint(StatefulTransitionCheckpoint::BeforeRecoveryCandidatePreservation));
        if let Err(error) = preflight {
            failures.preserve_candidate = Some(Box::new(error));
            return;
        }

        let preservation = match self.preserve_failed_candidate(
            candidate,
            candidate_origin,
            quarantine_archived_candidate,
            tree_identity,
        ) {
            Ok(preservation) => preservation,
            Err(first)
                if candidate_origin == StatefulCandidateOrigin::Fresh
                    || (candidate_origin == StatefulCandidateOrigin::Archived && quarantine_archived_candidate) =>
            {
                match self.preserve_failed_candidate(
                    candidate,
                    candidate_origin,
                    quarantine_archived_candidate,
                    tree_identity,
                ) {
                    Ok(preservation) => preservation,
                    Err(retry) => {
                        failures.preserve_candidate = Some(Box::new(Error::StatefulCandidatePreservationRetryFailed {
                            first: Box::new(first),
                            retry: Box::new(retry),
                        }));
                        // Never delete the only database correlation for a
                        // candidate whose retained quarantine publication did
                        // not survive one bounded in-process retry.
                        return;
                    }
                }
            }
            Err(error) => {
                failures.preserve_candidate = Some(Box::new(error));
                // Never delete the only database correlation for a candidate
                // which has not first been durably preserved.
                return;
            }
        };

        self.invalidate_fresh_candidate(
            candidate,
            candidate_origin,
            preservation.as_ref(),
            tree_identity,
            checkpoint,
            failures,
        );
    }

    fn invalidate_fresh_candidate<F>(
        &self,
        candidate: state::Id,
        candidate_origin: StatefulCandidateOrigin,
        preservation: Option<&QuarantinedCandidate>,
        tree_identity: &StatefulTreeIdentity,
        checkpoint: &mut F,
        failures: &mut StatefulRecoveryFailures,
    ) where
        F: FnMut(StatefulTransitionCheckpoint) -> Result<(), Error>,
    {
        if candidate_origin == StatefulCandidateOrigin::Fresh
            && let Err(error) = checkpoint(StatefulTransitionCheckpoint::BeforeRecoveryCandidateInvalidation)
                .and_then(|()| {
                    let preservation = preservation.ok_or_else(|| {
                        Error::Io(io::Error::other(
                            "fresh candidate has no retained quarantine proof before invalidation",
                        ))
                    })?;
                    tree_identity
                        .revalidate_quarantined_candidate(&self.installation, preservation)
                        .map_err(Error::from)
                })
                .and_then(|()| self.state_db.remove(&candidate).map_err(Error::Db))
        {
            failures.invalidate_candidate = Some(Box::new(error));
        }
    }

    fn exchange_staging_and_live_usr(&self, tree_identity: &StatefulTreeIdentity) -> Result<(), Error> {
        match tree_identity.exchange_reverse(&self.installation) {
            Ok(()) => Ok(()),
            Err(failure) if failure.outcome() == RetainedExchangeOutcome::Applied => {
                // The exact previous and candidate trees are already restored.
                // Retry only the idempotent fsync/revalidation suffix; a
                // second RENAME_EXCHANGE would undo the recovery.
                tree_identity
                    .finish_applied_reverse(&self.installation)
                    .map_err(Error::from)
            }
            Err(failure) => Err(Error::from(failure)),
        }
    }

    fn restore_previous_to_staging(&self, tree_identity: &StatefulTreeIdentity, state: state::Id) -> Result<(), Error> {
        match tree_identity.restore_previous(&self.installation, state) {
            Ok(()) => Ok(()),
            Err(failure) if failure.outcome() == RetainedPreviousMoveOutcome::Applied => tree_identity
                .finish_applied_previous_restore(&self.installation, state)
                .map_err(Error::from),
            Err(failure) => Err(failure.into()),
        }
    }

    fn rearchive_archived_candidate(
        &self,
        tree_identity: &StatefulTreeIdentity,
        state: state::Id,
    ) -> Result<(), Error> {
        let mut preparation_retried = false;
        loop {
            match tree_identity.rearchive_archived_candidate(&self.installation, state) {
                Ok(()) => return Ok(()),
                Err(failure) if failure.outcome() == RetainedArchivedCandidateMoveOutcome::Applied => {
                    return tree_identity
                        .finish_applied_archived_candidate_rearchive(&self.installation, state)
                        .map_err(Error::from);
                }
                Err(failure)
                    if failure.outcome() == RetainedArchivedCandidateMoveOutcome::RearchivePreparationApplied
                        && !preparation_retried =>
                {
                    // The exact prerequisite rename already applied. Retry
                    // once so its retained durability suffix can finish, but
                    // never spin on a persistent sync/revalidation failure.
                    preparation_retried = true;
                }
                Err(failure) => return Err(failure.into()),
            }
        }
    }

    fn preserve_failed_candidate(
        &self,
        candidate: state::Id,
        candidate_origin: StatefulCandidateOrigin,
        quarantine_archived_candidate: bool,
        tree_identity: &StatefulTreeIdentity,
    ) -> Result<Option<QuarantinedCandidate>, Error> {
        if candidate_origin == StatefulCandidateOrigin::ActiveReblit
            && tree_identity
                .has_active_reblit_staging_rotation()
                .map_err(Error::from)?
        {
            tree_identity
                .preserve_failed_active_reblit_wrapper(&self.installation, candidate)
                .map_err(Error::from)?;
            return Ok(None);
        }
        if candidate_origin == StatefulCandidateOrigin::Archived && !quarantine_archived_candidate {
            self.rearchive_archived_candidate(tree_identity, candidate)?;
            tree_identity
                .verify_candidate_for_recovery(&self.installation.root_path(candidate.to_string()).join("usr"))?;
            return Ok(None);
        }

        // Fresh candidates may be only partially prepared, an active reblit
        // would duplicate the restored live state identity, and an archived
        // candidate whose system-trigger phase did not complete may have been
        // partially mutated. None is safe in the ordinary bootable/prunable
        // state-root namespace.
        let kind = match candidate_origin {
            StatefulCandidateOrigin::Fresh => FailedCandidateKind::NewState,
            StatefulCandidateOrigin::ActiveReblit => FailedCandidateKind::ActiveReblit,
            StatefulCandidateOrigin::Archived => FailedCandidateKind::ArchivedState,
        };
        let preserved = tree_identity.quarantine_candidate(&self.installation, candidate, kind)?;
        if candidate_origin == StatefulCandidateOrigin::Archived {
            tree_identity.retire_displaced_archived_candidate_slot(&self.installation, candidate)?;
        }
        Ok(Some(preserved))
    }

    /// Acquire the canonical journal lock, reject unresolved journal/database
    /// evidence, and establish permanent marker identities for the staged
    /// candidate and live previous tree. The returned guard retains all three
    /// capabilities through activation and compensating recovery.
    fn prepare_stateful_tree_identity(
        &self,
        candidate_usr: &Path,
        candidate_state: state::Id,
    ) -> Result<StatefulTreeIdentity, crate::transition_identity::Error> {
        StatefulTreeIdentity::prepare(&self.installation, &self.state_db, candidate_usr, candidate_state)
    }

    fn prepare_stateful_tree_identity_retained(
        &self,
        candidate_usr_path: &Path,
        candidate_usr: &std::fs::File,
        candidate_state: state::Id,
    ) -> Result<StatefulTreeIdentity, crate::transition_identity::Error> {
        StatefulTreeIdentity::prepare_retained_candidate(
            &self.installation,
            &self.state_db,
            candidate_usr_path,
            candidate_usr,
            candidate_state,
        )
    }
}

fn legacy_boot_repair_authority_error(
    source: crate::transition_identity::LegacyBootRepairAuthorityError,
) -> Error {
    Error::LegacyBootRepairAuthority {
        source: Box::new(source),
    }
}
