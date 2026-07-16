impl Client {
    pub fn apply_stateful_blit(
        &self,
        _fstree: vfs::Tree<PendingFile>,
        _state: &State,
        _old_state: Option<state::Id>,
        _system_snapshot: SystemModel,
    ) -> Result<(), Error> {
        Err(Error::FixedStagingCapabilityRequired {
            operation: "apply a stateful blit",
        })
    }

    fn apply_stateful_candidate(
        &self,
        candidate: fixed_staging::StatefulCandidate,
        state: &State,
        old_state: Option<state::Id>,
        system_snapshot: SystemModel,
    ) -> Result<(), Error> {
        let fixed_staging::StatefulCandidate {
            tree,
            staging,
            candidate_usr,
            local_etc,
            mut active_state,
        } = candidate;
        self.apply_stateful_blit_with_capability(
            tree,
            Some((&staging, &candidate_usr)),
            local_etc,
            state,
            old_state,
            &mut active_state,
            system_snapshot,
            |_| Ok(()),
        )
    }

    #[cfg(test)]
    fn apply_stateful_blit_with_checkpoint<F>(
        &self,
        fstree: vfs::Tree<PendingFile>,
        state: &State,
        old_state: Option<state::Id>,
        system_snapshot: SystemModel,
        checkpoint: F,
    ) -> Result<(), Error>
    where
        F: FnMut(StatefulTransitionCheckpoint) -> Result<(), Error>,
    {
        let local_etc = transaction_root::prepare_local_etc(&self.installation)?;
        let mut active_state = active_state_authority::ActiveStateAuthority::acquire(&self.installation)?;
        self.apply_stateful_blit_with_capability(
            fstree,
            None,
            local_etc,
            state,
            old_state,
            &mut active_state,
            system_snapshot,
            checkpoint,
        )
    }

    fn apply_stateful_blit_with_capability<F>(
        &self,
        fstree: vfs::Tree<PendingFile>,
        retained_staging: Option<(&fixed_staging::RetainedFixedStaging, &std::fs::File)>,
        local_etc: transaction_root::RetainedLocalEtc,
        state: &State,
        old_state: Option<state::Id>,
        active_state: &mut active_state_authority::ActiveStateAuthority,
        system_snapshot: SystemModel,
        mut checkpoint: F,
    ) -> Result<(), Error>
    where
        F: FnMut(StatefulTransitionCheckpoint) -> Result<(), Error>,
    {
        self.require_non_frozen()?;
        // Complete preflight before candidate identity preparation or any
        // trigger. A static conflict therefore leaves the staged candidate and
        // its database row available for inspection or an exact retry.
        let live_root_abi = preflight_root_links(&self.installation.root)?;
        let archive_previous = old_state.is_some();
        let candidate_usr = self.installation.staging_path("usr");
        // Empty package selections deliberately materialize no filesystem
        // root. Record the state identity first so the hardened metadata path
        // creates and authenticates the candidate `/usr` before the tree
        // marker guard attempts to pin it. This also covers remove-last-package
        // and empty active-state verification reblits.
        revalidate_fixed_staging(retained_staging.map(|(staging, _)| staging), &self.installation)?;
        let retained_usr = match retained_staging {
            Some((staging, candidate_usr)) => {
                record_state_id_retained(staging, candidate_usr, state.id)?;
                Some(candidate_usr)
            }
            None => {
                record_state_id(&self.installation.staging_dir(), state.id)?;
                None
            }
        };
        revalidate_fixed_staging(retained_staging.map(|(staging, _)| staging), &self.installation)?;
        active_state.revalidate(&self.installation)?;
        let captured_active_state = active_state.active();
        let prepared_identity = match retained_usr.as_ref() {
            Some(candidate) => self.prepare_stateful_tree_identity_retained(&candidate_usr, candidate, state.id),
            None => self.prepare_stateful_tree_identity(&candidate_usr, state.id),
        };
        let tree_identity = prepared_identity.map_err(|source| Error::StatefulTreeIdentityPreparationFailed {
            candidate: state.id,
            previous: old_state,
            location: candidate_usr,
            source: Box::new(source.into()),
        })?;
        active_state.refresh_after_tree_identity_preparation(&self.installation)?;
        revalidate_fixed_staging(retained_staging.map(|(staging, _)| staging), &self.installation)?;
        let (previous, candidate_origin) = match old_state {
            Some(id) => match self.state_db.get(id) {
                Ok(previous) => (Some(previous), StatefulCandidateOrigin::Fresh),
                Err(error) => {
                    return Err(self.preserve_unswapped_candidate(
                        state.id,
                        Some(id),
                        StatefulCandidateOrigin::Fresh,
                        Error::Db(error),
                        &tree_identity,
                        &mut checkpoint,
                    ));
                }
            },
            // Active-state verification reblits the same state and deliberately
            // does not archive the replaced corrupt tree on success. It still
            // needs the state value for boot repair if recovery reverses the
            // exchange.
            None if captured_active_state == Some(state.id) => {
                (Some(state.clone()), StatefulCandidateOrigin::ActiveReblit)
            }
            None => (None, StatefulCandidateOrigin::Fresh),
        };

        if candidate_origin == StatefulCandidateOrigin::ActiveReblit
            && let Err(primary) = tree_identity
                .prepare_active_reblit_staging_rotation(&self.installation, &self.state_db, state)
                .map_err(Error::from)
        {
            return Err(self.preserve_unswapped_candidate(
                state.id,
                previous.as_ref().map(|state| state.id),
                candidate_origin,
                primary,
                &tree_identity,
                &mut checkpoint,
            ));
        }

        let prepare = (|| {
            #[cfg(test)]
            before_stateful_candidate_metadata();
            revalidate_fixed_staging(retained_staging.map(|(staging, _)| staging), &self.installation)?;
            active_state.revalidate(&self.installation)?;
            tree_identity.verify_candidate_for_activation(&self.installation.staging_path("usr"))?;
            let metadata = candidate_metadata::decorate_stateful(&tree_identity, &system_snapshot)?;
            metadata.revalidate()?;
            tree_identity.verify_candidate_for_activation(&self.installation.staging_path("usr"))?;
            revalidate_fixed_staging(retained_staging.map(|(staging, _)| staging), &self.installation)?;

            let isolation_root = create_root_links(&self.installation.isolation_dir())?;
            #[cfg(test)]
            after_stateful_isolation_root_retention();

            // The container running triggers receives this exact retained
            // local /etc inode rather than resolving its mutable pathname.
            local_etc.revalidate(&self.installation)?;

            // Transaction triggers run before `/usr` is exchanged. Their
            // arbitrary external side effects cannot be undone, but the
            // candidate tree can still be preserved outside the active root.
            active_state.revalidate(&self.installation)?;
            tree_identity.verify_pre_exchange(
                &self.installation.staging_path("usr"),
                &self.installation.root.join("usr"),
            )?;
            live_root_abi.revalidate()?;
            match retained_usr {
                Some(candidate_usr) => Self::apply_triggers(
                    TriggerScope::RetainedTransaction {
                        kind: postblit::RetainedTransactionKind::Stateful,
                        installation: &self.installation,
                        isolation_root: &isolation_root,
                        local_etc: &local_etc,
                        candidate_usr,
                        candidate_usr_path: &self.installation.staging_path("usr"),
                    },
                    &fstree,
                )?,
                // Only unit-test adapters construct a stateful candidate
                // without the production retained fixed-staging capability.
                None => Self::apply_triggers(TriggerScope::Transaction(&self.installation), &fstree)?,
            }
            tree_identity.verify_pre_exchange(
                &self.installation.staging_path("usr"),
                &self.installation.root.join("usr"),
            )?;
            tree_identity.verify_candidate_for_activation(&self.installation.staging_path("usr"))?;
            metadata.revalidate()?;
            revalidate_fixed_staging(retained_staging.map(|(staging, _)| staging), &self.installation)?;
            active_state.revalidate(&self.installation)?;
            if candidate_origin == StatefulCandidateOrigin::ActiveReblit {
                tree_identity.verify_active_reblit_candidate_snapshot(
                    &self.installation,
                    &self.state_db,
                    state,
                    false,
                )?;
            }
            checkpoint(StatefulTransitionCheckpoint::AfterTransactionTriggers)?;
            Ok::<_, Error>(metadata)
        })();

        let metadata = match prepare {
            Ok(metadata) => metadata,
            Err(primary) => {
                return Err(self.preserve_unswapped_candidate(
                    state.id,
                    previous.as_ref().map(|state| state.id),
                    candidate_origin,
                    primary,
                    &tree_identity,
                    &mut checkpoint,
                ));
            }
        };

        self.commit_stateful_staging(
            &fstree,
            state,
            previous.as_ref(),
            candidate_origin,
            archive_previous,
            true,
            true,
            &tree_identity,
            Some(&metadata),
            live_root_abi,
            active_state,
            &mut checkpoint,
        )
    }

    /// Commit a completely prepared staging `/usr` and keep the prior tree
    /// recoverable until system triggers have succeeded.
    ///
    /// The prior state is archived before candidate boot synchronization so
    /// `boot::synchronize` can still enumerate it as an immediate rollback
    /// entry. A failure after that archive first moves it back to staging,
    /// reverses the same atomic exchange, preserves the failed candidate, and
    /// attempts to repair boot metadata for the restored state. A candidate
    /// boot failure remains a structured incomplete recovery because the boot
    /// backend cannot prove that partial candidate metadata was removed. This
    /// does not claim to reverse arbitrary side effects performed by a trigger.
    fn commit_stateful_staging<F>(
        &self,
        fstree: &vfs::Tree<PendingFile>,
        candidate: &State,
        previous: Option<&State>,
        candidate_origin: StatefulCandidateOrigin,
        archive_previous: bool,
        run_system_triggers: bool,
        run_boot_synchronization: bool,
        tree_identity: &StatefulTreeIdentity,
        metadata: Option<&candidate_metadata::CandidateMetadataProof>,
        live_root_abi: RootAbiPreflight,
        active_state: &active_state_authority::ActiveStateAuthority,
        checkpoint: &mut F,
    ) -> Result<(), Error>
    where
        F: FnMut(StatefulTransitionCheckpoint) -> Result<(), Error>,
    {
        // Preserve the production guard that historically lived in
        // `promote_staging`: an ephemeral client must never reach the
        // stateful `/usr` exchange, even if a future caller bypasses the
        // ordinary public entry-point checks.
        if self.scope.is_ephemeral() {
            return Err(Error::EphemeralProhibitedOperation);
        }

        if candidate_origin != StatefulCandidateOrigin::Archived && metadata.is_none() {
            return Err(self.preserve_unswapped_candidate(
                candidate.id,
                previous.map(|state| state.id),
                candidate_origin,
                Error::StatefulCandidateMetadataProofRequired {
                    candidate: candidate.id,
                },
                tree_identity,
                checkpoint,
            ));
        }

        if let Err(primary) = tree_identity
            .verify_pre_exchange(
                &self.installation.staging_path("usr"),
                &self.installation.root.join("usr"),
            )
            .map_err(Error::from)
            .and_then(|()| {
                tree_identity
                    .verify_candidate_for_activation(&self.installation.staging_path("usr"))
                    .map_err(Error::from)
            })
            .and_then(|()| metadata.map_or(Ok(()), |proof| proof.revalidate()))
            .and_then(|()| active_state.revalidate(&self.installation))
            .and_then(|()| live_root_abi.revalidate())
            .and_then(|()| {
                if candidate_origin == StatefulCandidateOrigin::ActiveReblit {
                    tree_identity
                        .verify_active_reblit_candidate_snapshot(&self.installation, &self.state_db, candidate, false)
                        .map_err(Error::from)
                } else {
                    Ok(())
                }
            })
            .and_then(|()| checkpoint(StatefulTransitionCheckpoint::BeforeUsrExchange))
        {
            return Err(self.preserve_unswapped_candidate(
                candidate.id,
                previous.map(|state| state.id),
                candidate_origin,
                primary,
                tree_identity,
                checkpoint,
            ));
        }

        let promotion = tree_identity.exchange_forward_validated(&self.installation, &|| {
            tree_identity.verify_candidate_for_activation(&self.installation.staging_path("usr"))?;
            if let Some(metadata) = metadata {
                metadata
                    .revalidate()
                    .map_err(|source| crate::transition_identity::Error::RetainedExchange {
                        operation: "revalidate retained candidate metadata immediately before forward exchange",
                        path: metadata.diagnostic_path().to_owned(),
                        source: io::Error::other(source),
                    })?;
            }
            active_state.revalidate(&self.installation).map_err(|source| {
                crate::transition_identity::Error::RetainedExchange {
                    operation: "revalidate active-state authority immediately before forward exchange",
                    path: self.installation.root.join("usr/.stateID"),
                    source: io::Error::other(source),
                }
            })?;
            live_root_abi
                .revalidate()
                .map_err(|source| crate::transition_identity::Error::RetainedExchange {
                    operation: "revalidate retained root ABI immediately before forward exchange",
                    path: live_root_abi.path().to_owned(),
                    source: io::Error::other(source),
                })?;
            if candidate_origin == StatefulCandidateOrigin::ActiveReblit {
                tree_identity.verify_active_reblit_candidate_snapshot(
                    &self.installation,
                    &self.state_db,
                    candidate,
                    false,
                )?;
            }
            Ok(())
        });
        if let Err(failure) = promotion {
            let outcome = failure.outcome();
            let primary = Error::from(failure);
            return match outcome {
                RetainedExchangeOutcome::NotApplied => Err(self.preserve_unswapped_candidate(
                    candidate.id,
                    previous.map(|state| state.id),
                    candidate_origin,
                    primary,
                    tree_identity,
                    checkpoint,
                )),
                RetainedExchangeOutcome::Applied => Err(self.recover_swapped_candidate(
                    candidate.id,
                    previous,
                    candidate_origin,
                    PreviousUsrLocation::Staging,
                    None,
                    false,
                    false,
                    primary,
                    tree_identity,
                    checkpoint,
                )),
                // Neither authenticated layout survived reconciliation. Do
                // not guess which tree to move. The candidate row is left in
                // place, but durable mutation fencing remains the pending
                // journal-coordinator work documented in Phase 11.
                RetainedExchangeOutcome::Ambiguous => Err(primary),
            };
        }

        let mut previous_location = PreviousUsrLocation::Staging;
        let mut previous_archive_cleanup_pending = None;
        let mut system_triggers_incomplete = false;
        let mut candidate_boot_synchronization_started = false;
        let primary = (|| {
            tree_identity.verify_forward_exchange(
                &self.installation.root.join("usr"),
                &self.installation.staging_path("usr"),
            )?;
            tree_identity.verify_candidate_for_activation(&self.installation.root.join("usr"))?;
            if let Some(metadata) = metadata {
                metadata.revalidate()?;
            }
            let live_root_abi = live_root_abi.publish()?;
            live_root_abi.revalidate()?;
            if candidate_origin == StatefulCandidateOrigin::ActiveReblit {
                tree_identity.verify_active_reblit_candidate_snapshot(
                    &self.installation,
                    &self.state_db,
                    candidate,
                    true,
                )?;
            }
            checkpoint(StatefulTransitionCheckpoint::AfterUsrExchange)?;

            if run_system_triggers {
                system_triggers_incomplete = true;
                checkpoint(StatefulTransitionCheckpoint::AfterSystemTriggersStarted)?;
                Self::apply_triggers(TriggerScope::System(&self.installation), fstree)?;
                tree_identity.verify_forward_exchange(
                    &self.installation.root.join("usr"),
                    &self.installation.staging_path("usr"),
                )?;
                tree_identity.verify_candidate_for_activation(&self.installation.root.join("usr"))?;
                if let Some(metadata) = metadata {
                    metadata.revalidate()?;
                }
                if candidate_origin == StatefulCandidateOrigin::ActiveReblit {
                    tree_identity.verify_active_reblit_candidate_snapshot(
                        &self.installation,
                        &self.state_db,
                        candidate,
                        true,
                    )?;
                }
                live_root_abi.revalidate()?;
                system_triggers_incomplete = false;
            }
            checkpoint(StatefulTransitionCheckpoint::AfterSystemTriggers)?;
            tree_identity.verify_candidate_for_activation(&self.installation.root.join("usr"))?;
            if let Some(metadata) = metadata {
                metadata.revalidate()?;
            }
            live_root_abi.revalidate()?;

            if archive_previous && let Some(previous) = previous {
                checkpoint(StatefulTransitionCheckpoint::BeforePreviousStateArchive)?;
                tree_identity.verify_candidate_for_activation(&self.installation.root.join("usr"))?;
                if let Some(metadata) = metadata {
                    metadata.revalidate()?;
                }
                tree_identity.verify_previous_for_recovery(&self.installation.staging_path("usr"))?;
                match tree_identity.archive_previous(&self.installation, previous.id) {
                    Ok(()) => {}
                    Err(failure) if failure.outcome() == RetainedPreviousMoveOutcome::Applied => {
                        // Recovery must look in the archive even when the
                        // idempotent durability suffix still reports failure.
                        previous_location = PreviousUsrLocation::Archived(previous.id);
                        tree_identity.finish_applied_previous_archive(&self.installation, previous.id)?;
                    }
                    Err(failure) if failure.outcome() == RetainedPreviousMoveOutcome::NotApplied => {
                        // `archive_previous` already made one exact-slot
                        // retirement attempt. Recovery performs one bounded
                        // suffix retry before reversing /usr, so a transient
                        // post-retirement durability failure cannot poison the
                        // next transaction's canonical state name.
                        previous_archive_cleanup_pending = Some(previous.id);
                        return Err(failure.into());
                    }
                    Err(failure) => return Err(failure.into()),
                }
                previous_location = PreviousUsrLocation::Archived(previous.id);
                tree_identity.verify_forward_exchange(
                    &self.installation.root.join("usr"),
                    &self.installation.root_path(previous.id.to_string()).join("usr"),
                )?;
                tree_identity.verify_candidate_for_activation(&self.installation.root.join("usr"))?;
                if let Some(metadata) = metadata {
                    metadata.revalidate()?;
                }
                checkpoint(StatefulTransitionCheckpoint::AfterPreviousStateArchive)?;
            }

            if run_boot_synchronization {
                checkpoint(StatefulTransitionCheckpoint::BeforeCandidateBootSynchronization)?;
                tree_identity.verify_candidate_for_activation(&self.installation.root.join("usr"))?;
                if let Some(metadata) = metadata {
                    metadata.revalidate()?;
                }
                if candidate_origin == StatefulCandidateOrigin::ActiveReblit {
                    tree_identity.verify_active_reblit_candidate_snapshot(
                        &self.installation,
                        &self.state_db,
                        candidate,
                        true,
                    )?;
                }
                candidate_boot_synchronization_started = true;
                checkpoint(StatefulTransitionCheckpoint::AfterCandidateBootSynchronizationStarted)?;
                tree_identity.verify_candidate_for_activation(&self.installation.root.join("usr"))?;
                if let Some(metadata) = metadata {
                    metadata.revalidate()?;
                }
                if candidate_origin == StatefulCandidateOrigin::ActiveReblit {
                    tree_identity.verify_active_reblit_candidate_snapshot(
                        &self.installation,
                        &self.state_db,
                        candidate,
                        true,
                    )?;
                }
                boot::synchronize(self, candidate, previous)?;
                tree_identity.verify_candidate_for_activation(&self.installation.root.join("usr"))?;
                if let Some(metadata) = metadata {
                    metadata.revalidate()?;
                }
                if candidate_origin == StatefulCandidateOrigin::ActiveReblit {
                    tree_identity.verify_active_reblit_candidate_snapshot(
                        &self.installation,
                        &self.state_db,
                        candidate,
                        true,
                    )?;
                }
            }

            if candidate_origin == StatefulCandidateOrigin::Archived {
                tree_identity.retire_displaced_archived_candidate_slot(&self.installation, candidate.id)?;
            }

            tree_identity.verify_candidate_for_activation(&self.installation.root.join("usr"))?;
            if let Some(metadata) = metadata {
                metadata.revalidate()?;
            }

            Ok(())
        })();

        match primary {
            Ok(()) if candidate_origin == StatefulCandidateOrigin::ActiveReblit => {
                match tree_identity.rotate_active_reblit_staging(&self.installation, &self.state_db, candidate) {
                    Ok(()) => Ok(()),
                    Err(failure) if failure.outcome() == RetainedStagingWrapperRotationOutcome::NotApplied => Err(self
                        .recover_swapped_candidate(
                            candidate.id,
                            previous,
                            candidate_origin,
                            PreviousUsrLocation::Staging,
                            None,
                            false,
                            candidate_boot_synchronization_started,
                            Error::from(failure),
                            tree_identity,
                            checkpoint,
                        )),
                    Err(failure) => {
                        let outcome = match failure.outcome() {
                            RetainedStagingWrapperRotationOutcome::Applied => "applied",
                            RetainedStagingWrapperRotationOutcome::Ambiguous => "ambiguous",
                            RetainedStagingWrapperRotationOutcome::NotApplied => "not-applied",
                        };
                        Err(Error::ActiveReblitCommittedCleanupIncomplete {
                            state: candidate.id,
                            outcome,
                            source: Box::new(failure),
                        })
                    }
                }
            }
            Ok(()) => Ok(()),
            Err(primary) => Err(self.recover_swapped_candidate(
                candidate.id,
                previous,
                candidate_origin,
                previous_location,
                previous_archive_cleanup_pending,
                system_triggers_incomplete,
                candidate_boot_synchronization_started,
                primary,
                tree_identity,
                checkpoint,
            )),
        }
    }
}
