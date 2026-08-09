impl Client {
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

    #[allow(dead_code)] // reachable only from #[cfg(test)] callers
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
