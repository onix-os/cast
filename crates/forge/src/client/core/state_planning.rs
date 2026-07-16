impl Client {
    /// Activates the provided state and runs system triggers once applied.
    ///
    /// The current state gets archived only after system triggers complete.
    /// If a later archive or boot synchronization step fails, Cast restores
    /// the previous `/usr`, preserves the failed candidate, and attempts to
    /// repair boot metadata for the restored state. Once candidate boot
    /// synchronization has begun, recovery remains explicitly unverified even
    /// when that compensating synchronization appears to succeed. Arbitrary
    /// side effects already performed by a system trigger are outside that
    /// filesystem recovery.
    ///
    /// Returns the old state that was archived.
    pub fn activate_state(&self, id: state::Id, skip_triggers: bool, skip_boot: bool) -> Result<state::Id, Error> {
        self.require_stateful_scope()?;
        let _guard = signal::ignore([Signal::SIGINT])?;
        let _inhibitor = signal::inhibit(
            vec!["shutdown", "sleep", "idle", "handle-lid-switch"],
            "cast".into(),
            "Activating state".into(),
            "block".into(),
        )?;

        self.activate_state_with_checkpoint(id, skip_triggers, skip_boot, |_| Ok(()))
    }

    fn activate_state_with_checkpoint<F>(
        &self,
        id: state::Id,
        skip_triggers: bool,
        skip_boot: bool,
        mut checkpoint: F,
    ) -> Result<state::Id, Error>
    where
        F: FnMut(StatefulTransitionCheckpoint) -> Result<(), Error>,
    {
        self.require_stateful_scope()?;
        let _local_etc = transaction_root::prepare_local_etc(&self.installation)?;
        let mut active_state = active_state_authority::ActiveStateAuthority::acquire(&self.installation)?;
        // Fetch the new state
        let new = self.state_db.get(id).map_err(|_| Error::StateDoesntExist(id))?;

        // Get old (current) state
        let Some(old_id) = active_state.active() else {
            return Err(Error::NoActiveState);
        };

        if new.id == old_id {
            return Err(Error::StateAlreadyActive(id));
        }
        let old = self.state_db.get(old_id)?;

        // Resolve the trigger view before moving either filesystem tree. A
        // database or VFS failure must leave the archived candidate untouched.
        let fstree = self.vfs(new.selections.iter().map(|selection| &selection.package))?;

        // Root ABI conflicts are immutable preflight failures, not reasons to
        // move the archived candidate or exchange the live /usr first. Retain
        // the read-only proof so the same names can be revalidated at the
        // exchange boundary without reopening mutable path authority.
        let live_root_abi = preflight_root_links(&self.installation.root)?;

        let archived_usr = self.installation.root_path(new.id.to_string()).join("usr");
        active_state.revalidate(&self.installation)?;
        let tree_identity = self
            .prepare_stateful_tree_identity(&archived_usr, new.id)
            .map_err(|source| Error::StatefulTreeIdentityPreparationFailed {
                candidate: new.id,
                previous: Some(old.id),
                location: archived_usr.clone(),
                source: Box::new(source.into()),
            })?;
        active_state.refresh_after_tree_identity_preparation(&self.installation)?;
        live_root_abi.revalidate()?;

        // Exchange the exact archived-state wrapper with the fixed staging
        // wrapper. Both inodes remain retained so a racing path replacement
        // is classified instead of overwritten or adopted.
        match tree_identity.stage_archived_candidate(&self.installation, new.id) {
            Ok(()) => {}
            Err(failure) if failure.outcome() == RetainedArchivedCandidateMoveOutcome::Applied => {
                // The exchange already happened. Resume only its idempotent
                // durability suffix; repeating the exchange would undo it.
                if let Err(primary) = tree_identity
                    .finish_applied_archived_candidate_stage(&self.installation, new.id)
                    .map_err(Error::from)
                {
                    return Err(self.preserve_unswapped_candidate(
                        new.id,
                        Some(old.id),
                        StatefulCandidateOrigin::Archived,
                        primary,
                        &tree_identity,
                        &mut checkpoint,
                    ));
                }
            }
            Err(failure) => return Err(failure.into()),
        }
        if let Err(primary) = tree_identity.verify_pre_exchange(
            &self.installation.staging_path("usr"),
            &self.installation.root.join("usr"),
        ) {
            return Err(self.preserve_unswapped_candidate(
                new.id,
                Some(old.id),
                StatefulCandidateOrigin::Archived,
                primary.into(),
                &tree_identity,
                &mut checkpoint,
            ));
        }

        self.commit_stateful_staging(
            &fstree,
            &new,
            Some(&old),
            StatefulCandidateOrigin::Archived,
            true,
            !skip_triggers,
            !skip_boot,
            &tree_identity,
            None,
            live_root_abi,
            &active_state,
            &mut checkpoint,
        )?;

        Ok(old_id)
    }

    /// Create a new recorded state from the provided packages
    /// provided packages and write that state ID to the installation
    /// Then blit the filesystem, promote it, finally archiving the active ID
    ///
    /// Returns `None` if the client is ephemeral
    pub fn new_state(&self, selections: &[Selection], summary: impl ToString) -> Result<Option<State>, Error> {
        self.require_non_frozen()?;
        let _guard = signal::ignore([Signal::SIGINT])?;
        let _inhibitor = signal::inhibit(
            vec!["shutdown", "sleep", "idle", "handle-lid-switch"],
            "cast".into(),
            "Applying new state".into(),
            "block".into(),
        )?;

        let explicit_packages =
            self.resolve_packages(selections.iter().filter_map(|s| s.explicit.then_some(&s.package)))?;
        let system_snapshot = generate_system_snapshot(
            self.installation.system_model.clone(),
            &self.repositories,
            &explicit_packages,
        )?;

        let timer = Instant::now();

        let state_span = info_span!(
            "progress",
            phase = summary.to_string().to_lowercase(),
            event_type = "progress"
        );
        let _state_guard = state_span.enter();
        info!(
            total_items = selections.len(),
            progress = 0.0,
            event_type = "progress_start",
        );

        let result = match &self.scope {
            Scope::Stateful => {
                // The non-cloneable candidate retains the authenticated
                // staging wrapper and the sole cooperating-writer lease from
                // its first possible mutation through row allocation and
                // durable tree-identity preparation.
                let candidate = self.materialize_stateful_candidate(selections.iter().map(|s| &s.package))?;
                let old_state = candidate.active_state.active();

                // Add to db
                candidate.active_state.revalidate(&self.installation)?;
                let state = self.state_db.add(selections, Some(&summary.to_string()), None)?;

                self.apply_stateful_candidate(candidate, &state, old_state, system_snapshot)?;

                Ok(Some(state))
            }
            Scope::Ephemeral { destination } => {
                let candidate = self.materialize_ephemeral_candidate(selections.iter().map(|s| &s.package))?;
                debug_assert_eq!(candidate.root, destination.path());
                self.apply_ephemeral_candidate(candidate, system_snapshot)?;

                Ok(None)
            }
            Scope::Frozen { .. } => unreachable!("frozen scope rejected before state creation"),
        };

        info!(
            duration_ms = timer.elapsed().as_millis(),
            items_processed = selections.len(),
            progress = 1.0,
            event_type = "progress_completed",
        );

        result
    }

    /// Apply all triggers with the given scope, wrapping with a progressbar.
    fn apply_triggers(scope: TriggerScope<'_>, fstree: &vfs::Tree<PendingFile>) -> Result<(), postblit::Error> {
        #[cfg(test)]
        observe_trigger_scope(&scope);
        let triggers = postblit::triggers(scope, fstree)?;

        let progress = ProgressBar::new(triggers.len() as u64).with_style(
            ProgressStyle::with_template("\n|{bar:20.green/blue}| {pos}/{len} {msg}")
                .unwrap()
                .progress_chars("■≡=- "),
        );

        let phase_name = match &scope {
            TriggerScope::Transaction(..) => {
                progress.set_message("Running transaction-scope triggers");
                "transaction-scope-triggers"
            }
            TriggerScope::RetainedTransaction {
                kind: postblit::RetainedTransactionKind::Stateful,
                ..
            } => {
                progress.set_message("Running transaction-scope triggers");
                "transaction-scope-triggers"
            }
            TriggerScope::RetainedTransaction {
                kind: postblit::RetainedTransactionKind::ArchivedRepair,
                ..
            } => {
                progress.set_message("Running retained transaction-scope triggers");
                "retained-transaction-scope-triggers"
            }
            TriggerScope::RetainedEphemeral {
                phase: postblit::RetainedEphemeralPhase::Transaction,
                ..
            } => {
                progress.set_message("Running retained ephemeral transaction-scope triggers");
                "retained-ephemeral-transaction-scope-triggers"
            }
            TriggerScope::RetainedEphemeral {
                phase: postblit::RetainedEphemeralPhase::System,
                ..
            } => {
                progress.set_message("Running retained ephemeral system-scope triggers");
                "retained-ephemeral-system-scope-triggers"
            }
            TriggerScope::System(..) => {
                progress.set_message("Running system-scope triggers");
                "system-scope-triggers"
            }
        };

        let timer = Instant::now();

        info!(
            phase = phase_name,
            total_items = triggers.len(),
            progress = 0.0,
            event_type = "progress_start",
        );

        for (i, trigger) in progress.wrap_iter(triggers.iter()).enumerate() {
            trigger.execute()?;

            info!(
                progress = (i + 1) as f32 / triggers.len() as f32,
                current = i + 1,
                total = triggers.len(),
                event_type = "progress_update",
                "Executing `{}`",
                trigger.handler()
            );
        }

        info!(
            phase = phase_name,
            duration_ms = timer.elapsed().as_millis(),
            items_processed = triggers.len(),
            progress = 1.0,
            event_type = "progress_completed",
        );

        progress.finish_and_clear();

        Ok(())
    }
}
