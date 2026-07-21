.PHONY: forge-startup-usr-rollback-active-reblit-finalization-test

forge-startup-usr-rollback-active-reblit-finalization-test:
	@set -euo pipefail; \
	mkdir -p "$(TOP_DIR)/target"; \
	listed="$$( mktemp "$(TOP_DIR)/target/active-reblit-finalization-list.XXXXXXXXXXXX" )"; \
	refs="$$( mktemp "$(TOP_DIR)/target/active-reblit-finalization-refs.XXXXXXXXXXXX" )"; \
	executor_code="$$( mktemp "$(TOP_DIR)/target/active-reblit-finalization-code.XXXXXXXXXXXX" )"; \
	trap 'rm -f "$$listed" "$$refs" "$$executor_code"' EXIT; \
	$(CARGO) test -p forge --lib -- --list | tee "$$listed" >/dev/null; \
	grep -q . "$$listed"; \
	gate_prefix='client::startup_gate::usr_rollback_active_reblit::tests::finalization_'; \
	test "$$( grep -c "^$$gate_prefix.*: test$$" "$$listed" )" = 21; \
	for name in \
		authority_binding::startup_active_reblit_finalization_authority_covers_both_indices_in_both_epochs_and_rejects_wrong_bindings \
		authority_binding::startup_active_reblit_finalization_admits_root_links_only_at_generation_fourteen \
		authority_binding::startup_active_reblit_finalization_binding_rejects_same_bytes_on_a_different_inode \
		authority_binding::startup_active_reblit_finalization_authority_refuses_terminal_candidate_without_exact_previous_identity \
		boundaries::startup_active_reblit_finalization_keeps_candidate_preserved_and_terminal_deletion_on_separate_entries \
		boundaries::startup_active_reblit_finalization_keeps_new_state_and_archived_in_their_own_routes \
		boundaries::startup_active_reblit_finalization_rejects_a_valid_terminal_lookalike_plan_and_wrong_topology \
		evidence_races::startup_active_reblit_finalization_refuses_wrong_database_and_provenance_evidence \
		evidence_races::startup_active_reblit_finalization_rejects_capture_and_final_pre_evidence_races \
		evidence_races::startup_active_reblit_finalization_rejects_database_and_provenance_changes_at_final_pre_and_after_delete \
		lock_handoff::startup_active_reblit_finalization_hands_the_same_lock_into_clean_startup_until_proof_drop \
		matrix::startup_active_reblit_finalization_covers_all_twenty_four_exact_terminal_cases_and_both_wrapper_indices \
		process_kill::startup_active_reblit_finalization_process_kills_restart_cleanly \
		public_binding_races::startup_active_reblit_finalization_bound_delete_never_unlinks_a_last_seam_replacement \
		public_binding_races::startup_active_reblit_finalization_rejects_hidden_entry_set_substitution \
		public_binding_races::startup_active_reblit_finalization_rejects_public_directory_and_lock_substitution \
		public_binding_races::startup_active_reblit_finalization_rejects_source_recreation_after_delete_and_absence_proof \
		restart::startup_active_reblit_finalization_restarts_from_observed_absence_with_fresh_handles \
		restart::startup_active_reblit_finalization_restarts_from_retained_terminal_source_with_fresh_handles \
		root_link_races::startup_active_reblit_finalization_root_links_rejects_all_five_link_races_at_each_evidence_seam \
		storage_faults::startup_active_reblit_finalization_preserves_both_bound_delete_faults_and_converges_on_restart; do \
		grep -Fqx "$$gate_prefix$$name: test" "$$listed"; \
	done; \
	executor_prefix='client::startup_recovery::usr_rollback_active_reblit_finalization::tests::reconcile_delete::'; \
	test "$$( grep -c "^$$executor_prefix.*: test$$" "$$listed" )" = 3; \
	for name in \
		active_reblit_finalization_preserves_absent_bound_delete_error \
		active_reblit_finalization_preserves_absent_error_when_post_delete_evidence_also_changes \
		active_reblit_finalization_preserves_exact_source_bound_delete_error; do \
		grep -Fqx "$$executor_prefix$$name: test" "$$listed"; \
	done; \
	authority=crates/forge/src/client/startup_reconciliation/usr_rollback_active_reblit_finalization_authority.rs; \
	proof=crates/forge/src/client/startup_reconciliation/activation_namespace/active_reblit_finalization_proof.rs; \
	topology=crates/forge/src/client/startup_reconciliation/activation_namespace/candidate_preserve_proof.rs; \
	executor=crates/forge/src/client/startup_recovery/usr_rollback_active_reblit_finalization.rs; \
	orchestrator=crates/forge/src/client/startup_gate/usr_rollback_active_reblit.rs; \
	startup_gate=crates/forge/src/client/startup_gate.rs; \
	recovery_root=crates/forge/src/client/startup_recovery.rs; \
	reconciliation_root=crates/forge/src/client/startup_reconciliation.rs; \
	namespace_root=crates/forge/src/client/startup_reconciliation/activation_namespace.rs; \
	gate_tests=crates/forge/src/client/startup_gate/usr_rollback_active_reblit/tests; \
	process_kill="$$gate_tests/finalization_process_kill.rs"; \
	executor_tests=crates/forge/src/client/startup_recovery/usr_rollback_active_reblit_finalization/tests; \
	grep -Fqx 'mod usr_rollback_active_reblit_finalization_authority;' "$$reconciliation_root"; \
	grep -Fqx 'mod active_reblit_finalization_proof;' "$$namespace_root"; \
	grep -Fqx 'mod usr_rollback_active_reblit_finalization;' "$$recovery_root"; \
	for symbol in UsrRollbackActiveReblitFinalizationAdmission UsrRollbackActiveReblitFinalizationAfterDeleteAuthority UsrRollbackActiveReblitFinalizationAuthority UsrRollbackActiveReblitFinalizationAuthorityError; do \
		grep -Fq "$$symbol" "$$reconciliation_root"; \
	done; \
	for symbol in UsrRollbackActiveReblitFinalizationError finalize_usr_rollback_active_reblit; do \
		grep -Fq "$$symbol" "$$recovery_root"; \
	done; \
	for module in finalization_authority_binding finalization_boundaries finalization_evidence_races finalization_lock_handoff finalization_matrix finalization_process_kill finalization_public_binding_races finalization_restart finalization_root_link_races finalization_storage_faults; do \
		grep -Fqx "mod $$module;" "$$gate_tests/mod.rs"; \
	done; \
	for module in reconcile_delete support; do grep -Fqx "mod $$module;" "$$executor_tests/mod.rs"; done; \
	grep -Fqx 'pub(in crate::client) struct UsrRollbackActiveReblitFinalizationSeal {' "$$orchestrator"; \
	seal_impl="$$( sed -n '/^impl UsrRollbackActiveReblitFinalizationSeal {/,/^}/p' "$$orchestrator" )"; \
	test "$$( grep -Fc '    fn new() -> Self {' <<<"$$seal_impl" )" = 1; \
	test "$$( grep -Fc '    pub(in crate::client) fn new_for_test() -> Self {' <<<"$$seal_impl" )" = 1; \
	awk '$$0 == "    #[cfg(test)]" { gated = 1; next } gated && $$0 == "    pub(in crate::client) fn new_for_test() -> Self {" { found++; gated = 0; next } gated { gated = 0 } END { exit found != 1 }' <<<"$$seal_impl"; \
	grep -Fq '        Self::new()' <<<"$$seal_impl"; \
	rg -U -q '^pub\(in crate::client\) use usr_rollback_active_reblit::\{\n    UsrRollbackActiveReblitBootRepairCompleteSeal, UsrRollbackActiveReblitBootRepairRequiredSeal,\n    UsrRollbackActiveReblitBootRepairUnverifiedSeal, UsrRollbackActiveReblitCompleteRouteSeal,\n    UsrRollbackActiveReblitFinalizationSeal,\n\};' "$$startup_gate"; \
	if awk 'previous == "#[cfg(test)]" && $$0 == "pub(in crate::client) use usr_rollback_active_reblit::{" { found = 1 } { previous = $$0 } END { exit !found }' "$$startup_gate"; then exit 1; else status="$$?"; test "$$status" = 1; fi; \
	grep -Fqx 'pub(in crate::client) enum UsrRollbackActiveReblitFinalizationAdmission<'\''reservation> {' "$$authority"; \
	grep -Fqx 'pub(in crate::client) struct UsrRollbackActiveReblitFinalizationAuthority<'\''reservation> {' "$$authority"; \
	grep -Fqx 'pub(in crate::client) struct UsrRollbackActiveReblitFinalizationAfterDeleteAuthority<'\''reservation> {' "$$authority"; \
	grep -Fq '    journal_binding: TransitionJournalBinding,' "$$authority"; \
	grep -Fqx '    journal_record_binding: TransitionJournalRecordBinding,' "$$authority"; \
	grep -Fq '    installation: Installation,' "$$authority"; \
	grep -Fq '    state_db: db::state::Database,' "$$authority"; \
	grep -Fq '    record: TransitionRecord,' "$$authority"; \
	grep -Fq '    database: UsrRollbackActiveReblitFinalizationDatabaseEvidence,' "$$authority"; \
	grep -Fq '    namespace: UsrRollbackActiveReblitFinalizationNamespaceProof,' "$$authority"; \
	grep -Fq '    _active_state_reservation: &'\''reservation ActiveStateReservation,' "$$authority"; \
	grep -Fq '_startup_gate_seal: &UsrRollbackActiveReblitFinalizationSeal,' "$$authority"; \
	grep -Fq 'let journal_binding = journal.binding();' "$$authority"; \
	grep -Fq 'if !journal.has_binding(&self.journal_binding)' "$$authority"; \
	grep -Fq 'let retained_state_db = state_db.clone();' "$$authority"; \
	grep -Fq 'debug_assert!(retained_state_db.same_instance(state_db));' "$$authority"; \
	grep -Fq 'record.operation != Operation::ActiveReblit || record.phase != Phase::RollbackComplete' "$$authority"; \
	grep -Fq 'record.candidate.id == record.previous.id' "$$authority"; \
	grep -Fq 'ForwardPhase::UsrExchangeIntent | ForwardPhase::UsrExchanged' "$$authority"; \
	grep -Fq 'ForwardPhase::RootLinksComplete, 14' "$$authority"; \
	grep -Fq 'rollback.previous_archive == RollbackAction::NotRequired' "$$authority"; \
	test "$$( grep -Fc 'RollbackAction::Applied | RollbackAction::AlreadySatisfied' "$$authority" )" = 2; \
	grep -Fq 'rollback.candidate.disposition == AbortDisposition::Quarantine' "$$authority"; \
	grep -Fq 'rollback.fresh_db == RollbackAction::NotRequired' "$$authority"; \
	grep -Fq 'rollback.boot == BootRollback::NotRequired' "$$authority"; \
	grep -Fq 'rollback.external_effects_may_remain' "$$authority"; \
	grep -Fq 'database_ownership_evidence_compatible(record, evidence)' "$$authority"; \
	grep -Fq 'metadata_provenance_evidence_compatible(record, evidence)' "$$authority"; \
	grep -Fq 'DatabaseEvidence::ExistingCandidate {' "$$authority"; \
	grep -Fq 'candidate: existing,' "$$authority"; \
	grep -Fq 'existing.ownership == db::state::TransitionOwnership::Cleared' "$$authority"; \
	grep -Fq 'existing.state == candidate' "$$authority"; \
	grep -Fq 'provenance: Some(_),' "$$authority"; \
	grep -Fq 'previous: None,' "$$authority"; \
	capture="$$( sed -n '/pub(in crate::client) fn capture(/,/pub(in crate::client) fn revalidate(/p' "$$authority" )"; \
	binding_line="$$( grep -nF 'journal.record_binding(installation.retained_mutable_cast_directory()?, record)?;' <<<"$$capture" | cut -d: -f1 )"; \
	database_before_line="$$( grep -nF 'let database_before = match inspect_current_database(record, state_db)? {' <<<"$$capture" | cut -d: -f1 )"; \
	namespace_begin_line="$$( grep -nF 'UsrRollbackActiveReblitFinalizationNamespaceInspection::begin' <<<"$$capture" | cut -d: -f1 )"; \
	namespace_finish_line="$$( grep -nF 'let namespace = match namespace_inspection.finish' <<<"$$capture" | cut -d: -f1 )"; \
	database_after_line="$$( grep -nF 'let database_after = match inspect_current_database(record, state_db)? {' <<<"$$capture" | cut -d: -f1 )"; \
	test "$$binding_line" -lt "$$database_before_line"; \
	test "$$database_before_line" -lt "$$namespace_begin_line"; \
	test "$$namespace_begin_line" -lt "$$namespace_finish_line"; \
	test "$$namespace_finish_line" -lt "$$database_after_line"; \
	if rg -n 'journal_record_binding:[[:space:]]+Option|journal_record_binding\.clone\(' "$$authority"; then exit 1; else status="$$?"; test "$$status" = 1; fi; \
	if rg -U -n '#\[derive\([^]]*Clone[^]]*\)\]\n(?:#\[[^\n]*\]\n)*(?:pub\([^)]*\)[[:space:]]+)?(?:struct|enum)[[:space:]]+(UsrRollbackActiveReblitFinalization(?:Authority|AfterDeleteAuthority|DatabaseEvidence|NamespaceProof))' "$$authority" "$$proof"; then exit 1; else status="$$?"; test "$$status" = 1; fi; \
	if rg -n 'impl Clone for UsrRollbackActiveReblitFinalization(?:Authority|AfterDeleteAuthority|DatabaseEvidence|NamespaceProof)' "$$authority" "$$proof"; then exit 1; else status="$$?"; test "$$status" = 1; fi; \
	if rg -n '\.delete\(|dispatch_usr_|persist_usr_|fn new\(\) -> Self' "$$authority" "$$proof"; then exit 1; else status="$$?"; test "$$status" = 1; fi; \
	if rg -n 'UsrRollbackFinalization(?:Authority|NamespaceProof)|UsrRollbackActiveReblitCompleteRoute(?:Authority|NamespaceProof)' "$$authority" "$$proof"; then exit 1; else status="$$?"; test "$$status" = 1; fi; \
	rg -n -F 'UsrRollbackActiveReblitFinalizationAuthority::capture(' crates/forge/src/client --glob '*.rs' --glob '!**/tests/**' --glob '!**/tests.rs' --glob '!**/*_tests.rs' --glob '!**/*_tests/**' --glob '!**/usr_rollback_active_reblit_finalization_authority.rs' > "$$refs"; \
	test "$$( wc -l < "$$refs" )" = 1; \
	test "$$( cut -d: -f1 "$$refs" )" = "$$orchestrator"; \
	grep -Fqx '        Phase::RollbackComplete => {' "$$orchestrator"; \
	test "$$( grep -Fc 'UsrRollbackActiveReblitFinalizationAuthority::capture(' "$$orchestrator" )" = 1; \
	test "$$( grep -Fc 'finalize_usr_rollback_active_reblit(journal, authority)?' "$$orchestrator" )" = 1; \
	grep -Fq 'Ok(Dispatch::Finalized { journal })' "$$orchestrator"; \
	candidate_line="$$( grep -nF 'Phase::CandidatePreserveIntent => {' "$$orchestrator" | cut -d: -f1 )"; \
	complete_line="$$( grep -nF 'Phase::CandidatePreserved => {' "$$orchestrator" | cut -d: -f1 )"; \
	finalize_line="$$( grep -nF 'Phase::RollbackComplete => {' "$$orchestrator" | cut -d: -f1 )"; \
	test "$$candidate_line" -lt "$$complete_line"; \
	test "$$complete_line" -lt "$$finalize_line"; \
	grep -Fq 'usr_rollback_active_reblit::Dispatch::Finalized { journal } => {' "$$startup_gate"; \
	grep -Fq 'return Self::admit_clean_after_terminal_finalization(installation, state_db, journal);' "$$startup_gate"; \
	grep -Fq 'return Self::admit_clean(installation, state_db, journal, in_flight);' "$$startup_gate"; \
	rg -U -q '^pub\(in crate::client\) fn finalize_usr_rollback_active_reblit\(\n    journal: TransitionJournalStore,\n    authority: UsrRollbackActiveReblitFinalizationAuthority<'\''_>,\n\) -> Result<TransitionJournalStore, UsrRollbackActiveReblitFinalizationError> \{' "$$executor"; \
	test "$$( grep -Fc '.revalidate(&journal)' "$$executor" )" = 1; \
	test "$$( grep -Fc '.attempt_record_bound_delete(&journal)' "$$executor" )" = 1; \
	test "$$( grep -Fc 'journal.delete_record_binding(' "$$authority" )" = 1; \
	if rg -n 'delete_revalidated_retained_cast|require_exact_public_source|inspect_exact_public_record|journal\.load\(' "$$authority" "$$proof" "$$executor"; then exit 1; else status="$$?"; test "$$status" = 1; fi; \
	grep -Fq '.revalidate_after_journal_delete(&journal)' "$$executor"; \
	for branch in 'Ok(()) => {' 'TransitionJournalRecordDeleteState::Absent' 'Err(source) => Err(UsrRollbackActiveReblitFinalizationError::Delete(source))' 'DeleteAndPostDeleteAuthority {'; do \
		grep -Fq "$$branch" "$$executor"; \
	done; \
	grep -Fq 'require_exact_active_reblit_rollback_complete_topology' "$$proof" "$$topology"; \
	grep -Fq '    before: NamespaceSnapshot,' "$$proof"; \
	grep -Fq '    after: NamespaceSnapshot,' "$$proof"; \
	inspection_struct="$$( sed -n '/struct UsrRollbackActiveReblitFinalizationNamespaceInspection {/,/^}/p' "$$proof" )"; \
	proof_struct="$$( sed -n '/struct UsrRollbackActiveReblitFinalizationNamespaceProof {/,/^}/p' "$$proof" )"; \
	test "$$( grep -Fc '    wrapper_index: usize,' <<<"$$inspection_struct" )" = 1; \
	test "$$( grep -Fc '    wrapper_index: usize,' <<<"$$proof_struct" )" = 1; \
	grep -Fq 'require_matching_fingerprints(&self.before, &self.after)?;' "$$proof"; \
	grep -Fq 'let fresh = capture_snapshot(installation, expected)?;' "$$proof"; \
	grep -Fq 'require_matching_fingerprints(&self.before, &fresh)?;' "$$proof"; \
	grep -Fq 'require_exact_wrapper_index(expected, &fresh, self.wrapper_index)?;' "$$proof"; \
	grep -Fq 'pub(in crate::client::startup_reconciliation) fn revalidate_after_journal_delete(' "$$proof"; \
	test "$$( grep -Fc 'require_exact_public_journal_absence(installation, journal)?;' "$$proof" )" = 2; \
	if rg -n 'canonical_journal_reopen|reopen_canonical_journal|finalize_usr_rollback_active_reblit_and_reopen|TransitionJournalStore::(?:open|try_open)' "$$executor"; then exit 1; else status="$$?"; test "$$status" = 1; fi; \
	if rg -n 'journal\.delete[[:space:]]*\(|\.delete[[:space:]]*\(|TransitionJournalStore::(?:open|try_open)' "$$executor"; then exit 1; else status="$$?"; test "$$status" = 1; fi; \
	sed -E 's,//.*$$,,' "$$executor" > "$$executor_code"; \
	if rg -n '^[[:space:]]*(loop|while|for)[[:space:]]|=[[:space:]]*(loop|while|for)[[:space:]]|retry|cleanup|retained_exchange|boot_synchronize|fresh_db_invalidation|candidate_preserve|attempt_move|state_db' "$$executor_code"; then exit 1; else status="$$?"; test "$$status" = 1; fi; \
	if rg -n 'diesel::|SqliteConnection|sql_query|\.execute[[:space:]]*\(|\.transaction[[:space:]]*\(|\.advance[[:space:]]*\(|run_(transaction|system)_triggers|root_links|archive_previous|clear_transition_if_matches|remove_transition_if_matches|remove_exact_|\.add[[:space:]]*\(|\.create[[:space:]]*\(|\.remove[[:space:]]*\(|\.batch_remove[[:space:]]*\(' "$$executor_code"; then exit 1; else status="$$?"; test "$$status" = 1; fi; \
	if rg -n 'transition_identity|linux_fs|std::fs|nix::|renameat|unlinkat|linkat|sync_(all|data)|write_all|set_permissions|chmod|mkdir|create_dir|remove_(dir|file)|hard_link|symlink' "$$executor_code"; then exit 1; else status="$$?"; test "$$status" = 1; fi; \
	for hook in arm_between_usr_rollback_active_reblit_finalization_database_captures arm_before_usr_rollback_active_reblit_finalization_fresh_namespace_capture arm_before_usr_rollback_active_reblit_finalization_final_revalidation arm_after_usr_rollback_active_reblit_finalization_delete; do \
		rg -q "$$hook" "$$authority" "$$proof" "$$executor" "$$reconciliation_root" "$$recovery_root"; \
	done; \
	for fault in arm_next_delete_canonical_unlink_fault arm_next_delete_directory_sync_fault assert_delete_canonical_unlink_fault_consumed assert_delete_directory_sync_fault_consumed; do grep -Fq "$$fault" "$$gate_tests/finalization_storage_faults.rs"; done; \
	for loop in 'for epoch in Epoch::ALL {' 'for source in CandidateSource::THROUGH_ROLLBACK_COMPLETE {' 'for usr_outcome in USR_OUTCOMES {' 'for candidate_outcome in CandidateOrigin::ALL {'; do grep -Fq "$$loop" "$$gate_tests/finalization_matrix.rs"; done; \
	for cardinality in 'Epoch::ALL.len(), 2' 'CandidateSource::THROUGH_ROLLBACK_COMPLETE.len(), 3' 'USR_OUTCOMES.len(), 2' 'CandidateOrigin::ALL.len(), 2'; do grep -Fq "$$cardinality" "$$gate_tests/finalization_matrix.rs"; done; \
	grep -Fq 'pub(super) const WRAPPER_INDICES: [usize; 2] = [0, WRAPPER_INDEX];' "$$gate_tests/support.rs"; \
	grep -Fq 'assert_eq!(WRAPPER_INDICES, [0, WRAPPER_INDEX]);' "$$gate_tests/finalization_matrix.rs"; \
	grep -Fq 'assert_eq!(cases, 24);' "$$gate_tests/finalization_matrix.rs"; \
	grep -Fq 'assert_eq!(cases, 15);' "$$gate_tests/finalization_root_link_races.rs"; \
	grep -Fq 'for wrapper_index in WRAPPER_INDICES {' "$$gate_tests/finalization_authority_binding.rs"; \
	grep -Fq 'const ALL: [Self; 3] = [' "$$process_kill"; \
	grep -Fq 'for epoch in Epoch::ALL {' "$$process_kill"; \
	grep -Fq 'for source in CandidateSource::ALL {' "$$process_kill"; \
	grep -Fq 'CandidateSource::RootLinksComplete => {' "$$process_kill"; \
	grep -Fq 'unreachable!("RootLinksComplete is outside the later process-kill source axis")' "$$process_kill"; \
	if grep -Fq 'CandidateSource::THROUGH_ROLLBACK_COMPLETE' "$$process_kill"; then exit 1; else status="$$?"; test "$$status" = 1; fi; \
	grep -Fq 'for boundary in FinalizationKillBoundary::ALL {' "$$process_kill"; \
	grep -Fq '        cases, 12,' "$$process_kill"; \
	test "$$( grep -Fc ') => Self {' "$$process_kill" )" = 4; \
	rg -U -q '\(ProcessEpoch::Current, ProcessSource::Intent\) => Self \{\n[[:space:]]+usr_outcome: RollbackActionOutcome::Applied,\n[[:space:]]+candidate_origin: CandidateOrigin::Applied,\n[[:space:]]+wrapper_index: 0,\n[[:space:]]+\},' "$$process_kill"; \
	rg -U -q '\(ProcessEpoch::Current, ProcessSource::Exchanged\) => Self \{\n[[:space:]]+usr_outcome: RollbackActionOutcome::Applied,\n[[:space:]]+candidate_origin: CandidateOrigin::AlreadySatisfied,\n[[:space:]]+wrapper_index: 13,\n[[:space:]]+\},' "$$process_kill"; \
	rg -U -q '\(ProcessEpoch::Historical, ProcessSource::Intent\) => Self \{\n[[:space:]]+usr_outcome: RollbackActionOutcome::AlreadySatisfied,\n[[:space:]]+candidate_origin: CandidateOrigin::Applied,\n[[:space:]]+wrapper_index: 13,\n[[:space:]]+\},' "$$process_kill"; \
	rg -U -q '\(ProcessEpoch::Historical, ProcessSource::Exchanged\) => Self \{\n[[:space:]]+usr_outcome: RollbackActionOutcome::AlreadySatisfied,\n[[:space:]]+candidate_origin: CandidateOrigin::AlreadySatisfied,\n[[:space:]]+wrapper_index: 0,\n[[:space:]]+\},' "$$process_kill"; \
	grep -Fq 'arm_before_usr_rollback_active_reblit_finalization_final_revalidation(kill_self)' "$$process_kill"; \
	test "$$( grep -Fc 'arm_journal_delete_durability_callback(' "$$process_kill" )" = 2; \
	for boundary in CanonicalUnlinked DeleteDirectorySynced; do grep -Fq "JournalDeleteDurabilityBoundary::$$boundary" "$$process_kill"; done; \
	test "$$( grep -Fc 'CleanSystemStartup::enter(' "$$process_kill" )" = 2; \
	grep -Fq 'let installation = Installation::open(&case.root, None).unwrap();' "$$process_kill"; \
	grep -Fq 'let database = open_state_database(&installation);' "$$process_kill"; \
	grep -Fq 'Command::new(env::current_exe().unwrap())' "$$process_kill"; \
	grep -Fq '.arg(TEST_NAME)' "$$process_kill"; \
	grep -Fq '.arg("--exact")' "$$process_kill"; \
	grep -Fq '.arg("--test-threads=1")' "$$process_kill"; \
	grep -Fq 'Some(nix::libc::SIGKILL)' "$$process_kill"; \
	grep -Fq 'nix::libc::kill(nix::libc::getpid(), nix::libc::SIGKILL)' "$$process_kill"; \
	grep -Fq 'const CHILD_DEADLINE: Duration = Duration::from_secs(15);' "$$process_kill"; \
	grep -Fq 'struct DeadlineChild {' "$$process_kill"; \
	test "$$( grep -Fc '.wait(CHILD_DEADLINE)' "$$process_kill" )" = 2; \
	grep -Fq 'if let Some(status) = self.child.as_mut().unwrap().try_wait().unwrap() {' "$$process_kill"; \
	grep -Fq 'if Instant::now() >= deadline {' "$$process_kill"; \
	grep -Fq 'impl Drop for DeadlineChild {' "$$process_kill"; \
	grep -Fq 'let control = tempfile::tempdir().unwrap();' "$$process_kill"; \
	grep -Fq 'assert_separate_control_path(&root, &control_path);' "$$process_kill"; \
	grep -Fq 'external ActiveReblit process-kill control does not match the terminal case' "$$process_kill"; \
	grep -Fq 'let terminal_bytes = fs::read(canonical_path(&root)).unwrap();' "$$process_kill"; \
	grep -Fq 'install_persistent_database(&mut fixture);' "$$process_kill"; \
	grep -Fq 'let retained_root = release_candidate_handles(fixture);' "$$process_kill"; \
	grep -Fqx 'struct ExistingCandidateDatabase {' "$$process_kill"; \
	grep -Fq 'assert_eq!(record.candidate.id, record.previous.id);' "$$process_kill"; \
	grep -Fq 'assert_eq!(in_flight, None);' "$$process_kill"; \
	grep -Fq 'assert_eq!(ownership, db::state::TransitionOwnership::Cleared);' "$$process_kill"; \
	grep -Fq '.expect("ActiveReblit candidate provenance must remain present");' "$$process_kill"; \
	test "$$( grep -Fc 'ExistingCandidateDatabase::capture(' "$$process_kill" )" = 6; \
	grep -Fq 'let wrapper = active_wrapper_path_at(&fixture, dimensions.wrapper_index);' "$$process_kill"; \
	test "$$( grep -Fc 'assert_wrapper(' "$$process_kill" )" = 7; \
	test "$$( grep -Fc 'snapshot_startup_recovery_namespace(' "$$process_kill" )" = 6; \
	grep -Fqx 'struct PublicJournalIdentity {' "$$process_kill"; \
	grep -Fq 'assert_journal_inventory(root, canonical_present);' "$$process_kill"; \
	grep -Fq 'public_before.assert_same_public_anchors(final_public);' "$$process_kill"; \
	grep -Fq 'StorageError::AcquireLock' "$$process_kill"; \
	grep -Fq 'assert_eq!(reopened.load().unwrap(), None)' "$$process_kill"; \
	grep -Fq 'arm_journal_update_durability_callback(JournalUpdateDurabilityBoundary::TemporaryFullySynced' "$$process_kill"; \
	test "$$( grep -Fc 'assert_no_candidate_effects()' "$$process_kill" )" = 4; \
	grep -Fq 'not a reboot or power-loss oracle' "$$process_kill"; \
	if rg -n 'arm_next_|assert_.*fault_consumed|finalize_usr_rollback_active_reblit[[:space:]]*\(|capture_finalization_ready[[:space:]]*\(|enter_clean_(candidate|fresh_handles)[[:space:]]*\(|FaultPoint|StorageFault' "$$process_kill"; then exit 1; else status="$$?"; test "$$status" = 1; fi; \
	test "$$( grep -Fc 'release_candidate_handles(fixture)' "$$gate_tests/finalization_restart.rs" )" = 2; \
	test "$$( grep -Fc 'assert_fresh_existing_candidate_database(' "$$gate_tests/finalization_restart.rs" )" = 4; \
	test "$$( grep -Fc 'CandidateSource::RootLinksComplete' "$$gate_tests/finalization_restart.rs" )" = 2; \
	test "$$( grep -Fc 'assert_eq!(terminal.generation, 14);' "$$gate_tests/finalization_restart.rs" )" = 2; \
	for disclaimer in 'fresh process-like' 'do not claim SIGKILL' 'reboot' 'power-loss durability'; do grep -Fq "$$disclaimer" "$$gate_tests/finalization_restart.rs"; done; \
	grep -Fq 'StorageError::AcquireLock' "$$gate_tests/finalization_lock_handoff.rs"; \
	grep -Fq 'OperationKind::Archived' "$$gate_tests/finalization_boundaries.rs"; \
	grep -Fq 'OperationKind::NewState' "$$gate_tests/finalization_boundaries.rs"; \
	grep -Fq 'wrong_candidate.candidate.id = None;' "$$gate_tests/finalization_authority_binding.rs"; \
	grep -Fq 'assert_eq!(terminal.generation, 14);' "$$gate_tests/finalization_authority_binding.rs"; \
	grep -Fq 'replace_with_same_bytes' "$$gate_tests/finalization_authority_binding.rs"; \
	for file in "$$authority" "$$proof" "$$topology" "$$executor" "$$orchestrator" "$$startup_gate" "$$recovery_root" "$$reconciliation_root" "$$namespace_root" "$$gate_tests"/finalization_*.rs "$$gate_tests/support.rs" "$$executor_tests"/*.rs misc/make/startup-rollback-active-reblit-finalization-tests.mk misc/make/help.mk Makefile; do \
		test "$$( wc -l < "$$file" )" -le 1000; \
	done; \
	$(CARGO) test -p forge --lib active_reblit_finalization -- --test-threads=1
