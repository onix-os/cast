.PHONY: forge-startup-usr-rollback-activate-archived-finalization-test

forge-startup-usr-rollback-activate-archived-finalization-test:
	@set -euo pipefail; \
	timeout 10s mkdir -p "$(TOP_DIR)/target"; \
	listed="$$( timeout 10s mktemp "$(TOP_DIR)/target/activate-archived-finalization-list.XXXXXXXXXXXX" )"; \
	refs="$$( timeout 10s mktemp "$(TOP_DIR)/target/activate-archived-finalization-refs.XXXXXXXXXXXX" )"; \
	executor_code="$$( timeout 10s mktemp "$(TOP_DIR)/target/activate-archived-finalization-code.XXXXXXXXXXXX" )"; \
	trap 'timeout 10s rm -f "$$listed" "$$refs" "$$executor_code"' EXIT; \
	timeout 300s $(CARGO) test -p forge --lib -- --list | timeout 300s tee "$$listed" >/dev/null; \
	timeout 10s grep -q . "$$listed"; \
	gate_prefix='client::startup_gate::usr_rollback_activate_archived::tests::finalization_'; \
	timeout 10s test "$$( timeout 10s grep -c "^$$gate_prefix.*: test$$" "$$listed" )" = 21; \
	for name in \
		authority_binding::startup_activate_archived_finalization_authority_covers_both_epochs_and_rejects_wrong_bindings \
		authority_binding::startup_activate_archived_finalization_admits_root_links_only_at_generation_twelve \
		authority_binding::startup_activate_archived_finalization_binding_rejects_same_bytes_on_a_different_inode \
		authority_binding::startup_activate_archived_finalization_authority_refuses_inexact_terminal_identities \
		boundaries::startup_activate_archived_finalization_authority_excludes_other_operations_and_phases \
		boundaries::startup_activate_archived_finalization_keeps_completion_and_terminal_deletion_on_separate_entries \
		boundaries::startup_activate_archived_finalization_rejects_valid_terminal_lookalike_plan_and_wrong_topology \
		evidence_races::startup_activate_archived_finalization_rejects_capture_and_final_pre_races \
		evidence_races::startup_activate_archived_finalization_rejects_final_pre_and_post_delete_evidence_changes \
		evidence_races::startup_activate_archived_finalization_requires_exact_two_rows_and_candidate_provenance \
		lock_handoff::startup_activate_archived_finalization_hands_the_same_lock_into_clean_startup_until_proof_drop \
		matrix::startup_activate_archived_finalization_covers_all_twenty_four_exact_terminal_cases \
		process_kill::startup_activate_archived_finalization_process_kills_restart_cleanly \
		public_binding_races::startup_activate_archived_finalization_bound_delete_never_unlinks_a_last_seam_replacement \
		public_binding_races::startup_activate_archived_finalization_rejects_hidden_entry_set_substitution \
		public_binding_races::startup_activate_archived_finalization_rejects_public_directory_and_lock_substitution \
		public_binding_races::startup_activate_archived_finalization_rejects_source_recreation_after_delete_and_absence_proof \
		restart::startup_activate_archived_finalization_restarts_from_observed_absence_with_fresh_handles \
		restart::startup_activate_archived_finalization_restarts_from_retained_terminal_source_with_fresh_handles \
		root_link_races::startup_activate_archived_finalization_root_links_rejects_all_five_link_races_at_each_evidence_seam \
		storage_faults::startup_activate_archived_finalization_classifies_both_delete_faults_and_converges; do \
		timeout 10s grep -Fqx "$$gate_prefix$$name: test" "$$listed"; \
	done; \
	executor_prefix='client::startup_recovery::usr_rollback_activate_archived_finalization::tests::reconcile_delete::'; \
	timeout 10s test "$$( timeout 10s grep -c "^$$executor_prefix.*: test$$" "$$listed" )" = 3; \
	for name in \
		activate_archived_finalization_preserves_absent_bound_delete_error \
		activate_archived_finalization_preserves_absent_error_when_post_delete_evidence_also_changes \
		activate_archived_finalization_preserves_exact_source_bound_delete_error; do \
		timeout 10s grep -Fqx "$$executor_prefix$$name: test" "$$listed"; \
	done; \
	authority=crates/forge/src/client/startup_reconciliation/usr_rollback_activate_archived_finalization_authority.rs; \
	proof=crates/forge/src/client/startup_reconciliation/activation_namespace/activate_archived_finalization_proof.rs; \
	topology=crates/forge/src/client/startup_reconciliation/activation_namespace/candidate_preserve_proof.rs; \
	executor=crates/forge/src/client/startup_recovery/usr_rollback_activate_archived_finalization.rs; \
	orchestrator=crates/forge/src/client/startup_gate/usr_rollback_activate_archived.rs; \
	startup_gate=crates/forge/src/client/startup_gate.rs; \
	recovery_root=crates/forge/src/client/startup_recovery.rs; \
	reconciliation_root=crates/forge/src/client/startup_reconciliation.rs; \
	namespace_root=crates/forge/src/client/startup_reconciliation/activation_namespace.rs; \
	gate_tests=crates/forge/src/client/startup_gate/usr_rollback_activate_archived/tests; \
	process_kill="$$gate_tests/finalization_process_kill.rs"; \
	executor_tests=crates/forge/src/client/startup_recovery/usr_rollback_activate_archived_finalization/tests; \
	timeout 10s grep -Fqx 'mod usr_rollback_activate_archived_finalization_authority;' "$$reconciliation_root"; \
	timeout 10s grep -Fqx 'mod activate_archived_finalization_proof;' "$$namespace_root"; \
	timeout 10s grep -Fqx 'mod usr_rollback_activate_archived_finalization;' "$$recovery_root"; \
	for module in finalization_authority_binding finalization_boundaries finalization_evidence_races finalization_lock_handoff finalization_matrix finalization_process_kill finalization_public_binding_races finalization_restart finalization_root_link_races finalization_storage_faults; do \
		timeout 10s grep -Fqx "mod $$module;" "$$gate_tests/mod.rs"; \
	done; \
	for module in reconcile_delete support; do timeout 10s grep -Fqx "mod $$module;" "$$executor_tests/mod.rs"; done; \
	timeout 10s grep -Fqx 'pub(in crate::client) struct UsrRollbackActivateArchivedFinalizationSeal {' "$$orchestrator"; \
	seal_impl="$$( timeout 10s sed -n '/^impl UsrRollbackActivateArchivedFinalizationSeal {/,/^}/p' "$$orchestrator" )"; \
	timeout 10s test "$$( timeout 10s grep -Fc '    fn new() -> Self {' <<<"$$seal_impl" )" = 1; \
	timeout 10s test "$$( timeout 10s grep -Fc '    pub(in crate::client) fn new_for_test() -> Self {' <<<"$$seal_impl" )" = 1; \
	timeout 10s awk '$$0 == "    #[cfg(test)]" { gated = 1; next } gated && $$0 == "    pub(in crate::client) fn new_for_test() -> Self {" { found++; gated = 0; next } gated { gated = 0 } END { exit found != 1 }' <<<"$$seal_impl"; \
	timeout 10s rg -U -q '^pub\(in crate::client\) use usr_rollback_activate_archived::\{\n    UsrRollbackActivateArchivedCompleteRouteSeal, UsrRollbackActivateArchivedFinalizationSeal,\n\};' "$$startup_gate"; \
	for field in \
		'record.operation == Operation::ActivateArchived' \
		'record.phase == Phase::RollbackComplete' \
		'record.candidate.origin == CandidateOrigin::Archived' \
		'record.previous.origin == PreviousOrigin::ActiveState' \
		'record.candidate.id != record.previous.id' \
		'ForwardPhase::UsrExchangeIntent | ForwardPhase::UsrExchanged' \
		'ForwardPhase::RootLinksComplete, 12' \
		'rollback.previous_archive == RollbackAction::NotRequired' \
		'rollback.candidate.disposition == AbortDisposition::Rearchive' \
		'rollback.fresh_db == RollbackAction::NotRequired' \
		'rollback.boot == BootRollback::NotRequired' \
		'!rollback.external_effects_may_remain'; do \
		timeout 10s grep -Fq "$$field" "$$authority"; \
	done; \
	timeout 10s test "$$( timeout 10s grep -Fc 'RollbackAction::Applied | RollbackAction::AlreadySatisfied' "$$authority" )" = 2; \
	for field in 'DatabaseEvidence::ExistingCandidate {' 'provenance: Some(_),' 'previous: Some(previous_existing),' 'existing.ownership == db::state::TransitionOwnership::Cleared' 'previous_existing.ownership == db::state::TransitionOwnership::Cleared'; do \
		timeout 10s grep -Fq "$$field" "$$authority"; \
	done; \
	capture="$$( timeout 10s sed -n '/pub(in crate::client) fn capture(/,/pub(in crate::client) fn revalidate(/p' "$$authority" )"; \
	binding_line="$$( timeout 10s grep -nF 'journal.record_binding(installation.retained_mutable_cast_directory()?, record)?;' <<<"$$capture" | timeout 10s cut -d: -f1 )"; \
	database_before_line="$$( timeout 10s grep -nF 'let database_before = match inspect_current_database(record, state_db)? {' <<<"$$capture" | timeout 10s cut -d: -f1 )"; \
	namespace_line="$$( timeout 10s grep -nF 'UsrRollbackActivateArchivedFinalizationNamespaceInspection::begin' <<<"$$capture" | timeout 10s cut -d: -f1 )"; \
	database_after_line="$$( timeout 10s grep -nF 'let database_after = match inspect_current_database(record, state_db)? {' <<<"$$capture" | timeout 10s cut -d: -f1 )"; \
	timeout 10s test "$$binding_line" -lt "$$database_before_line"; \
	timeout 10s test "$$database_before_line" -lt "$$namespace_line"; \
	timeout 10s test "$$namespace_line" -lt "$$database_after_line"; \
	timeout 10s grep -Fqx '    journal_record_binding: TransitionJournalRecordBinding,' "$$authority"; \
	if timeout 10s rg -n 'journal_record_binding:[[:space:]]+Option|journal_record_binding\.clone\(' "$$authority"; then exit 1; else status="$$?"; timeout 10s test "$$status" = 1; fi; \
	if timeout 10s rg -U -n '#\[derive\([^]]*Clone[^]]*\)\]\n(?:#\[[^\n]*\]\n)*(?:pub\([^)]*\)[[:space:]]+)?(?:struct|enum)[[:space:]]+UsrRollbackActivateArchivedFinalization(?:Authority|DatabaseEvidence|NamespaceProof)' "$$authority" "$$proof"; then exit 1; else status="$$?"; timeout 10s test "$$status" = 1; fi; \
	if timeout 10s rg -n 'impl Clone for UsrRollbackActivateArchivedFinalization(?:Authority|DatabaseEvidence|NamespaceProof)' "$$authority" "$$proof"; then exit 1; else status="$$?"; timeout 10s test "$$status" = 1; fi; \
	timeout 10s rg -n -F 'UsrRollbackActivateArchivedFinalizationAuthority::capture(' crates/forge/src/client --glob '*.rs' --glob '!**/tests/**' --glob '!**/tests.rs' --glob '!**/*_tests.rs' --glob '!**/*_tests/**' --glob '!**/usr_rollback_activate_archived_finalization_authority.rs' > "$$refs"; \
	timeout 10s test "$$( timeout 10s wc -l < "$$refs" )" = 1; \
	timeout 10s test "$$( timeout 10s cut -d: -f1 "$$refs" )" = "$$orchestrator"; \
	timeout 10s grep -Fqx '        Phase::RollbackComplete => {' "$$orchestrator"; \
	timeout 10s test "$$( timeout 10s grep -Fc 'finalize_usr_rollback_activate_archived(journal, authority)?' "$$orchestrator" )" = 1; \
	timeout 10s grep -Fq 'Ok(Dispatch::Finalized { journal })' "$$orchestrator"; \
	timeout 10s grep -Fq 'usr_rollback_activate_archived::Dispatch::Finalized { journal } => {' "$$startup_gate"; \
	timeout 10s grep -Fq 'return Self::admit_clean_after_terminal_finalization(installation, state_db, journal);' "$$startup_gate"; \
	timeout 10s test "$$( timeout 10s grep -Fc '.revalidate(&journal)' "$$executor" )" = 1; \
	timeout 10s test "$$( timeout 10s grep -Fc '.attempt_record_bound_delete(&journal)' "$$executor" )" = 1; \
	timeout 10s test "$$( timeout 10s grep -Fc 'journal.delete_record_binding(' "$$authority" )" = 1; \
	if timeout 10s rg -n 'delete_revalidated_retained_cast|require_exact_public_source|inspect_exact_public_record|journal\.load\(' "$$authority" "$$proof" "$$executor"; then exit 1; else status="$$?"; timeout 10s test "$$status" = 1; fi; \
	timeout 10s grep -Fq '.revalidate_after_journal_delete(&journal)' "$$executor"; \
	for branch in 'Ok(()) => {' 'TransitionJournalRecordDeleteState::Absent' 'Err(source) => Err(UsrRollbackActivateArchivedFinalizationError::Delete(source))' 'DeleteAndPostDeleteAuthority {'; do \
		timeout 10s grep -Fq "$$branch" "$$executor"; \
	done; \
	timeout 10s grep -Fq 'require_exact_activate_archived_rollback_complete_topology' "$$proof" "$$topology"; \
	timeout 10s test "$$( timeout 10s grep -Fc 'require_exact_public_journal_absence(installation, journal)?;' "$$proof" )" = 2; \
	if timeout 10s rg -n 'canonical_journal_reopen|reopen_canonical_journal|TransitionJournalStore::(?:open|try_open)' "$$executor"; then exit 1; else status="$$?"; timeout 10s test "$$status" = 1; fi; \
	timeout 10s sed -E 's,//.*$$,,' "$$executor" > "$$executor_code"; \
	if timeout 10s rg -n '^[[:space:]]*(loop|while|for)[[:space:]]|=[[:space:]]*(loop|while|for)[[:space:]]|retry|cleanup|run_(transaction|system)_triggers|\.advance[[:space:]]*\(|retained_exchange|boot_synchronize|fresh_db_invalidation|candidate_preserve|attempt_move|state_db' "$$executor_code"; then exit 1; else status="$$?"; timeout 10s test "$$status" = 1; fi; \
	for hook in arm_between_usr_rollback_activate_archived_finalization_database_captures arm_before_usr_rollback_activate_archived_finalization_fresh_namespace_capture arm_before_usr_rollback_activate_archived_finalization_final_revalidation arm_after_usr_rollback_activate_archived_finalization_delete; do \
		timeout 10s rg -q "$$hook" "$$authority" "$$proof" "$$executor" "$$reconciliation_root" "$$recovery_root"; \
	done; \
	for loop in 'for epoch in Epoch::ALL {' 'for source in CandidateSource::THROUGH_ROLLBACK_COMPLETE {' 'for usr_outcome in USR_OUTCOMES {' 'for candidate_outcome in CandidateOutcome::ALL {'; do timeout 10s grep -Fq "$$loop" "$$gate_tests/finalization_matrix.rs"; done; \
	for cardinality in 'Epoch::ALL.len(), 2' 'CandidateSource::THROUGH_ROLLBACK_COMPLETE.len(), 3' 'USR_OUTCOMES.len(), 2' 'CandidateOutcome::ALL.len(), 2'; do timeout 10s grep -Fq "$$cardinality" "$$gate_tests/finalization_matrix.rs"; done; \
	timeout 10s grep -Fq 'assert_eq!(cases, 24);' "$$gate_tests/finalization_matrix.rs"; \
	for zero_effect in 'candidate_move_count(), 0' 'retained_exchange_syscall_count(), 0' 'boot_synchronize_attempt_count(), 0' 'fresh_db_invalidation_removal_call_count(), 0' 'active_reblit_candidate_preserve_exchange_attempt_count(), 0'; do \
		timeout 10s grep -Fq "$$zero_effect" "$$gate_tests/finalization_matrix.rs"; \
	done; \
	timeout 10s grep -Fq 'assert_eq!(cases, 15);' "$$gate_tests/finalization_root_link_races.rs"; \
	for fault in arm_next_delete_canonical_unlink_fault arm_next_delete_directory_sync_fault assert_delete_canonical_unlink_fault_consumed assert_delete_directory_sync_fault_consumed; do timeout 10s grep -Fq "$$fault" "$$gate_tests/finalization_storage_faults.rs"; done; \
	timeout 10s test "$$( timeout 10s grep -Fc 'release_route_handles(fixture)' "$$gate_tests/finalization_restart.rs" )" = 2; \
	timeout 10s test "$$( timeout 10s grep -Fc 'assert_fresh_exact_database_pair(' "$$gate_tests/finalization_restart.rs" )" = 4; \
	for disclaimer in 'fresh process-like' 'do not claim SIGKILL' 'reboot' 'power-loss durability'; do timeout 10s grep -Fq "$$disclaimer" "$$gate_tests/finalization_restart.rs"; done; \
	timeout 10s grep -Fq 'StorageError::AcquireLock' "$$gate_tests/finalization_lock_handoff.rs"; \
	timeout 10s grep -Fq 'const ALL: [Self; 3] = [' "$$process_kill"; \
	timeout 10s grep -Fq 'for epoch in Epoch::ALL {' "$$process_kill"; \
	timeout 10s grep -Fq 'for source in CandidateSource::ALL {' "$$process_kill"; \
	timeout 10s grep -Fq 'for boundary in FinalizationKillBoundary::ALL {' "$$process_kill"; \
	timeout 10s grep -Fq '        cases, 12,' "$$process_kill"; \
	timeout 10s test "$$( timeout 10s grep -Fc ') => Self {' "$$process_kill" )" = 4; \
	timeout 10s rg -U -q '\(ProcessEpoch::Current, ProcessSource::Intent\) => Self \{\n[[:space:]]+usr_outcome: RollbackActionOutcome::Applied,\n[[:space:]]+candidate_outcome: CandidateOutcome::Applied,\n[[:space:]]+\},' "$$process_kill"; \
	timeout 10s rg -U -q '\(ProcessEpoch::Current, ProcessSource::Exchanged\) => Self \{\n[[:space:]]+usr_outcome: RollbackActionOutcome::Applied,\n[[:space:]]+candidate_outcome: CandidateOutcome::AlreadySatisfied,\n[[:space:]]+\},' "$$process_kill"; \
	timeout 10s rg -U -q '\(ProcessEpoch::Historical, ProcessSource::Intent\) => Self \{\n[[:space:]]+usr_outcome: RollbackActionOutcome::AlreadySatisfied,\n[[:space:]]+candidate_outcome: CandidateOutcome::Applied,\n[[:space:]]+\},' "$$process_kill"; \
	timeout 10s rg -U -q '\(ProcessEpoch::Historical, ProcessSource::Exchanged\) => Self \{\n[[:space:]]+usr_outcome: RollbackActionOutcome::AlreadySatisfied,\n[[:space:]]+candidate_outcome: CandidateOutcome::AlreadySatisfied,\n[[:space:]]+\},' "$$process_kill"; \
	timeout 10s grep -Fq 'arm_before_usr_rollback_activate_archived_finalization_final_revalidation(kill_self)' "$$process_kill"; \
	timeout 10s test "$$( timeout 10s grep -Fc 'arm_journal_delete_durability_callback(' "$$process_kill" )" = 2; \
	for boundary in CanonicalUnlinked DeleteDirectorySynced; do timeout 10s grep -Fq "JournalDeleteDurabilityBoundary::$$boundary" "$$process_kill"; done; \
	timeout 10s test "$$( timeout 10s grep -Fc 'CleanSystemStartup::enter(' "$$process_kill" )" = 2; \
	timeout 10s grep -Fq 'let installation = Installation::open(&case.root, None).unwrap();' "$$process_kill"; \
	timeout 10s grep -Fq 'let database = open_state_database(&installation);' "$$process_kill"; \
	timeout 10s grep -Fq 'Command::new(env::current_exe().unwrap())' "$$process_kill"; \
	for arg in '.arg(TEST_NAME)' '.arg("--exact")' '.arg("--test-threads=1")'; do timeout 10s grep -Fq "$$arg" "$$process_kill"; done; \
	timeout 10s grep -Fq 'Some(nix::libc::SIGKILL)' "$$process_kill"; \
	timeout 10s grep -Fq 'nix::libc::kill(nix::libc::getpid(), nix::libc::SIGKILL)' "$$process_kill"; \
	timeout 10s grep -Fq 'const CHILD_DEADLINE: Duration = Duration::from_secs(15);' "$$process_kill"; \
	timeout 10s grep -Fq 'struct DeadlineChild {' "$$process_kill"; \
	timeout 10s test "$$( timeout 10s grep -Fc '.wait(CHILD_DEADLINE)' "$$process_kill" )" = 2; \
	timeout 10s grep -Fq 'if let Some(status) = self.child.as_mut().unwrap().try_wait().unwrap() {' "$$process_kill"; \
	timeout 10s grep -Fq 'if Instant::now() >= deadline {' "$$process_kill"; \
	timeout 10s grep -Fq 'impl Drop for DeadlineChild {' "$$process_kill"; \
	timeout 10s grep -Fq 'let control = tempfile::tempdir().unwrap();' "$$process_kill"; \
	timeout 10s grep -Fq 'assert_separate_control_path(&root, &control_path);' "$$process_kill"; \
	timeout 10s grep -Fq 'external ActivateArchived process-kill control does not match the terminal case' "$$process_kill"; \
	timeout 10s grep -Fq 'let terminal_bytes = fs::read(canonical_path(&root)).unwrap();' "$$process_kill"; \
	timeout 10s grep -Fq 'install_persistent_route_database(&mut fixture);' "$$process_kill"; \
	timeout 10s grep -Fq 'let retained_root = release_route_handles(fixture);' "$$process_kill"; \
	timeout 10s grep -Fqx 'struct ExistingArchivedDatabase {' "$$process_kill"; \
	timeout 10s grep -Fq 'assert_ne!(record.candidate.id, record.previous.id);' "$$process_kill"; \
	timeout 10s grep -Fq 'assert_eq!(states.len(), 2);' "$$process_kill"; \
	timeout 10s grep -Fq 'assert_eq!(in_flight, None);' "$$process_kill"; \
	timeout 10s grep -Fq 'assert_eq!(candidate_ownership, db::state::TransitionOwnership::Cleared);' "$$process_kill"; \
	timeout 10s grep -Fq 'assert_eq!(previous_ownership, db::state::TransitionOwnership::Cleared);' "$$process_kill"; \
	timeout 10s grep -Fq '.expect("ActivateArchived candidate provenance must remain present");' "$$process_kill"; \
	timeout 10s test "$$( timeout 10s grep -Fc 'ExistingArchivedDatabase::capture(' "$$process_kill" )" = 6; \
	timeout 10s grep -Fq 'let wrapper = root.join(CAST_NAME).join("root").join(candidate.to_string());' "$$process_kill"; \
	timeout 10s rg -U -q 'assert_eq!\(\n[[:space:]]+\(slot_metadata\.dev\(\), slot_metadata\.ino\(\)\),\n[[:space:]]+\(marker_metadata\.dev\(\), marker_metadata\.ino\(\)\)\n[[:space:]]*\);' "$$process_kill"; \
	timeout 10s test "$$( timeout 10s grep -Fc 'assert_eq!(marker_metadata.nlink(), 2);' "$$process_kill" )" = 1; \
	timeout 10s grep -Fq 'root.join(CAST_NAME).join("root/staging")' "$$process_kill"; \
	timeout 10s rg -U -q '\.join\(record\.quarantine_name\.as_str\(\)\)\n[[:space:]]+\.exists\(\)' "$$process_kill"; \
	timeout 10s grep -Fq 'fs::read_to_string(root.join("usr/.stateID"))' "$$process_kill"; \
	timeout 10s grep -Fq 'for (name, target) in ROOT_ABI {' "$$process_kill"; \
	timeout 10s test "$$( timeout 10s grep -Fc 'snapshot_startup_recovery_namespace(' "$$process_kill" )" = 6; \
	timeout 10s grep -Fqx 'struct PublicJournalIdentity {' "$$process_kill"; \
	timeout 10s grep -Fq 'expected.assert_same_public_anchors(actual);' "$$process_kill"; \
	timeout 10s grep -Fq 'public_before.assert_same_public_anchors(final_public);' "$$process_kill"; \
	timeout 10s grep -Fq 'StorageError::AcquireLock' "$$process_kill"; \
	timeout 10s grep -Fq 'assert_eq!(reopened.load().unwrap(), None);' "$$process_kill"; \
	timeout 10s grep -Fq 'arm_journal_update_durability_callback(JournalUpdateDurabilityBoundary::TemporaryFullySynced' "$$process_kill"; \
	timeout 10s test "$$( timeout 10s grep -Fc 'assert_eq!(candidate_move_count(), 0);' "$$process_kill" )" = 4; \
	for disclaimer in 'real same-boot process death' 'not a reboot or power-loss oracle' 'not a reboot simulation'; do timeout 10s grep -Fq "$$disclaimer" "$$process_kill"; done; \
	if timeout 10s rg -n 'arm_next_|assert_.*fault_consumed|finalize_usr_rollback_activate_archived[[:space:]]*\(|capture_finalization_ready[[:space:]]*\(|enter_clean_(route|fresh_handles)[[:space:]]*\(|FaultPoint|StorageFault' "$$process_kill"; then exit 1; else status="$$?"; timeout 10s test "$$status" = 1; fi; \
	for file in "$$authority" "$$proof" "$$topology" "$$executor" "$$orchestrator" "$$startup_gate" "$$recovery_root" "$$reconciliation_root" "$$namespace_root" "$$gate_tests"/finalization_*.rs "$$gate_tests/support.rs" "$$executor_tests"/*.rs misc/make/startup-rollback-activate-archived-finalization-tests.mk; do \
		timeout 10s test "$$( timeout 10s wc -l < "$$file" )" -le 1000; \
	done; \
	timeout 600s $(CARGO) test -p forge --lib activate_archived_finalization -- --test-threads=1
