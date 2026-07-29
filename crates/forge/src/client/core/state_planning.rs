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
        let local_etc = transaction_root::prepare_local_etc(&self.installation)?;
        let active_state = active_state_authority::ActiveStateAuthority::acquire(&self.installation)?;
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
        let isolation_root = create_root_links(&self.installation.isolation_dir())?;

        let archived_usr = self.installation.root_path(new.id.to_string()).join("usr");
        active_state.revalidate(&self.installation)?;

        // The candidate's *own* recorded system model, loaded from its state
        // directory. The coordinated route does not decorate metadata for an
        // archived candidate — it verifies, checking derived outputs against the
        // provenance the database already holds
        // (`candidate_preparation.rs`, "substitute read-only verification").
        // A snapshot generated from the current installation model would
        // describe different packages and fail that equality check.
        let system_snapshot = self.load_or_create_system_snapshot(
            system_model::snapshot_path(&self.installation.root_path(new.id.to_string())),
            &new,
        )?;

        // The coordinated durable route owns the whole transition. The
        // archived-to-staging move now happens between two durable journal
        // phases rather than before the commit, so a crash mid-move leaves a
        // record recovery can act on instead of an orphaned tree
        // (`plans/future_impl.md` §1.2).
        //
        // This replaces the legacy prologue wholesale — identity preparation,
        // `stage_archived_candidate` with its "already applied" resume branch,
        // and `verify_pre_exchange` — because the coordinator performs and
        // journals that move itself. The legacy path reached
        // `commit_stateful_staging`, whose exchange asserts through
        // `ExchangeJournalGuard::LegacyNoJournal` that no journal exists, so
        // ActivateArchived was never actually durable.
        // `checkpoint` and `isolation_root` are genuinely obsolete here: the
        // coordinated route drives recovery through journal phases, and acquires
        // its own isolation ABI. `skip_triggers` is not — it is a live CLI flag
        // and is threaded through below.
        let _ = (live_root_abi, &mut checkpoint);
        self.apply_activate_archived_candidate(
            &new,
            &old,
            &archived_usr,
            active_state,
            &local_etc,
            &fstree,
            system_snapshot,
            !skip_triggers,
            !skip_boot,
        )
        .map_err(|source| Error::CoordinatedNewState(Box::new(source)))?;

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
                candidate.active_state.revalidate(&self.installation)?;

                match old_state {
                    // Replacing an active state: the journal-coordinated route
                    // owns the whole transition and allocates the state row
                    // inside its durable prefix, so a crash can never orphan the
                    // row from its transition (`plans/future_impl.md` §1.1a).
                    Some(previous) => {
                        let state = self
                            .apply_new_state_candidate(
                                candidate,
                                Some(previous),
                                selections,
                                &summary.to_string(),
                                system_snapshot,
                            )
                            .map_err(|source| Error::CoordinatedNewState(Box::new(source)))?;
                        Ok(Some(state))
                    }
                    // First install, on the same coordinated route. This used
                    // to defer at `CleanupComplete` — the namespace policy
                    // demanded a synthesized-empty previous be `Absent`, which
                    // the cleanup path cannot produce because it never unlinks,
                    // so the record stayed live and startup reported
                    // `RecoveryPending`. D1.5 admits that previous in staging,
                    // which lets the terminal chain finish.
                    None => {
                        let state = self
                            .apply_new_state_candidate(
                                candidate,
                                None,
                                selections,
                                &summary.to_string(),
                                system_snapshot,
                            )
                            .map_err(|source| Error::CoordinatedNewState(Box::new(source)))?;
                        Ok(Some(state))
                    }
                }
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
            TriggerScope::System { .. } => {
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
