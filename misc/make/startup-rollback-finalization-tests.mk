.PHONY: forge-startup-usr-rollback-finalization-test

forge-startup-usr-rollback-finalization-test:
	@set -euo pipefail; \
	mkdir -p "$(TOP_DIR)/target"; \
	listed="$$( mktemp "$(TOP_DIR)/target/rollback-finalization-list.XXXXXXXXXXXX" )"; \
	refs="$$( mktemp "$(TOP_DIR)/target/rollback-finalization-refs.XXXXXXXXXXXX" )"; \
	executor_code="$$( mktemp "$(TOP_DIR)/target/rollback-finalization-code.XXXXXXXXXXXX" )"; \
	trap 'rm -f "$$listed" "$$refs" "$$executor_code"' EXIT; \
	$(CARGO) test -p forge --lib -- --list | tee "$$listed" >/dev/null; \
	grep -q . "$$listed"; \
	authority_prefix='client::startup_reconciliation::usr_rollback_finalization_authority::tests::'; \
	test "$$( grep -c "^$$authority_prefix.*: test$$" "$$listed" )" = 7; \
	for name in \
		admission::startup_usr_rollback_finalization_admits_exact_current_and_historical_terminal_evidence \
		admission::startup_usr_rollback_finalization_admits_root_links_only_at_generation_eighteen \
		admission::startup_usr_rollback_finalization_rejects_inexact_phase_operation_plan_and_database \
		evidence::startup_usr_rollback_finalization_capture_sandwich_rejects_database_and_namespace_changes \
		evidence::startup_usr_rollback_finalization_revalidation_rejects_reopened_and_changed_authority \
		evidence::startup_usr_rollback_finalization_refuses_terminal_namespace_lookalikes \
		record_binding::startup_usr_rollback_finalization_binding_rejects_same_bytes_on_a_different_inode; do \
		grep -Fqx "$$authority_prefix$$name: test" "$$listed"; \
	done; \
	executor_prefix='client::startup_recovery::usr_rollback_finalization::tests::'; \
	test "$$( grep -c "^$$executor_prefix.*: test$$" "$$listed" )" = 14; \
	for name in \
		bound_delete_errors::new_state_finalization_preserves_absent_bound_delete_error \
		bound_delete_errors::new_state_finalization_preserves_absent_error_when_post_delete_evidence_also_changes \
		bound_delete_errors::new_state_finalization_preserves_exact_source_bound_delete_error \
		evidence_races::startup_usr_rollback_finalization_final_evidence_races_never_delete \
		evidence_races::startup_usr_rollback_finalization_rejects_reopened_and_cross_root_journal_bindings \
		matrix::startup_usr_rollback_finalization_success_matrix_retains_exact_canonical_absence \
		post_delete_evidence::startup_usr_rollback_finalization_post_delete_evidence_races_never_report_success \
		public_binding_races::startup_usr_rollback_finalization_bound_delete_never_unlinks_a_last_seam_replacement \
		public_binding_races::startup_usr_rollback_finalization_rejects_hidden_canonical_record_displacement \
		public_binding_races::startup_usr_rollback_finalization_rejects_public_journal_directory_and_lock_substitution \
		public_binding_races::startup_usr_rollback_finalization_rejects_source_recreation_after_delete_and_absence_proof \
		root_link_races::startup_usr_rollback_finalization_root_links_rejects_all_five_link_races_at_each_evidence_seam \
		storage_reconciliation::startup_usr_rollback_finalization_returns_the_same_continuously_locked_store \
		storage_reconciliation::startup_usr_rollback_finalization_storage_matrix_preserves_all_bound_delete_errors; do \
		grep -Fqx "$$executor_prefix$$name: test" "$$listed"; \
	done; \
	startup_prefix='client::startup_gate::usr_rollback_new_state::tests::finalization::'; \
	test "$$( grep -c "^$$startup_prefix.*: test$$" "$$listed" )" = 5; \
	for name in \
		startup_new_state_suffix_terminal_handoff_retains_the_same_journal_lock_through_clean_startup \
		startup_new_state_suffix_reaudits_database_after_finalization_before_clean_admission \
		startup_new_state_suffix_finalization_converges_into_the_shared_prune_residue_audit \
		startup_new_state_suffix_rejects_terminal_record_recreated_during_clean_handoff \
		startup_new_state_suffix_rejects_mutable_namespace_substitution_after_terminal_finalization; do \
		grep -Fqx "$$startup_prefix$$name: test" "$$listed"; \
	done; \
	restart_prefix='client::startup_gate::usr_rollback_new_state::tests::finalization_restart::'; \
	test "$$( grep -c "^$$restart_prefix.*: test$$" "$$listed" )" = 2; \
	for name in \
		startup_new_state_root_links_finalization_restarts_from_observed_absence_with_fresh_handles \
		startup_new_state_root_links_finalization_restarts_from_retained_terminal_source_with_fresh_handles; do \
		grep -Fqx "$$restart_prefix$$name: test" "$$listed"; \
	done; \
	endpoint='client::startup_recovery::usr_rollback_resume_route::tests::root_links_route_endpoint::startup_root_links_complete_fresh_entries_reach_operation_specific_stable_endpoints_without_second_reverse_exchange'; \
	grep -Fqx "$$endpoint: test" "$$listed"; \
	fresh_endpoint='client::startup_recovery::usr_rollback_fresh_db_invalidation_route::tests::endpoint::startup_root_links_complete_new_state_reaches_generation_18_then_finalizes_cleanly'; \
	grep -Fqx "$$fresh_endpoint: test" "$$listed"; \
	process_kill='client::startup_gate::usr_rollback_new_state::tests::terminal_delete_process_kill::startup_new_state_suffix_terminal_delete_process_kills_restart_cleanly'; \
	grep -Fqx "$$process_kill: test" "$$listed"; \
	authority=crates/forge/src/client/startup_reconciliation/usr_rollback_finalization_authority.rs; \
	proof=crates/forge/src/client/startup_reconciliation/activation_namespace/rollback_finalization_proof.rs; \
	topology=crates/forge/src/client/startup_reconciliation/activation_namespace/candidate_preserve_proof.rs; \
	executor=crates/forge/src/client/startup_recovery/usr_rollback_finalization.rs; \
	orchestrator=crates/forge/src/client/startup_gate/usr_rollback_new_state.rs; \
	startup_gate=crates/forge/src/client/startup_gate.rs; \
	recovery_root=crates/forge/src/client/startup_recovery.rs; \
	reconciliation_root=crates/forge/src/client/startup_reconciliation.rs; \
	namespace_root=crates/forge/src/client/startup_reconciliation/activation_namespace.rs; \
	authority_tests=crates/forge/src/client/startup_reconciliation/usr_rollback_finalization_authority/tests; \
	executor_tests=crates/forge/src/client/startup_recovery/usr_rollback_finalization/tests; \
	gate_tests=crates/forge/src/client/startup_gate/usr_rollback_new_state/tests; \
	startup_process_kill="$$gate_tests/terminal_delete_process_kill.rs"; \
	root_links_endpoint=crates/forge/src/client/startup_recovery/usr_rollback_resume_route/tests/root_links_route_endpoint.rs; \
	fresh_endpoint_file=crates/forge/src/client/startup_recovery/usr_rollback_fresh_db_invalidation_route/tests/endpoint.rs; \
	grep -Fqx 'mod usr_rollback_finalization_authority;' "$$reconciliation_root"; \
	grep -Fqx 'mod rollback_finalization_proof;' "$$namespace_root"; \
	grep -Fqx 'mod usr_rollback_finalization;' "$$recovery_root"; \
	for symbol in UsrRollbackFinalizationAdmission UsrRollbackFinalizationAfterDeleteAuthority UsrRollbackFinalizationAuthority UsrRollbackFinalizationAuthorityError; do grep -Fq "$$symbol" "$$reconciliation_root"; done; \
	for module in bound_delete_errors evidence_races matrix post_delete_evidence public_binding_races root_link_races route_support storage_reconciliation support; do grep -Fqx "mod $$module;" "$$executor_tests/mod.rs"; done; \
	grep -Fqx 'mod record_binding;' "$$authority_tests/../tests.rs"; \
	grep -Fqx 'mod finalization_restart;' "$$gate_tests/mod.rs"; \
	grep -Fq 'UsrRollbackFinalizationSeal::new();' "$$orchestrator"; \
	grep -Fq 'let journal = finalize_usr_rollback(journal, authority)?;' "$$orchestrator"; \
	grep -Fq 'Ok(Dispatch::Finalized { journal })' "$$orchestrator"; \
	grep -Fq 'usr_rollback_new_state::Dispatch::Finalized { journal } => {' "$$startup_gate"; \
	grep -Fq 'return Self::admit_clean_after_terminal_finalization(installation, state_db, journal);' "$$startup_gate"; \
	for field in 'record.operation == Operation::NewState' 'record.phase == Phase::RollbackComplete' 'ForwardPhase::UsrExchangeIntent | ForwardPhase::UsrExchanged' 'ForwardPhase::RootLinksComplete, 18' 'rollback.previous_archive == RollbackAction::NotRequired' 'rollback.candidate.disposition == AbortDisposition::Quarantine' 'rollback.boot == BootRollback::NotRequired' 'rollback.external_effects_may_remain'; do grep -Fq "$$field" "$$authority"; done; \
	test "$$( grep -Fc 'RollbackAction::Applied | RollbackAction::AlreadySatisfied' "$$authority" )" = 3; \
	grep -Fq '    absence: db::state::ExactFreshTransitionAbsence,' "$$authority"; \
	grep -Fq '    journal_record_binding: TransitionJournalRecordBinding,' "$$authority"; \
	if rg -n 'journal_record_binding:[[:space:]]+Option|journal_record_binding\.clone\(' "$$authority"; then exit 1; else test "$$?" = 1; fi; \
	capture="$$( sed -n '/pub(in crate::client) fn capture(/,/pub(in crate::client) fn revalidate(/p' "$$authority" )"; \
	binding_line="$$( grep -nF 'journal.record_binding(installation.retained_mutable_cast_directory()?, record)?;' <<<"$$capture" | cut -d: -f1 )"; \
	database_before_line="$$( grep -nF 'let database_before = match inspect_current_database(record, state_db)? {' <<<"$$capture" | cut -d: -f1 )"; \
	namespace_line="$$( grep -nF 'UsrRollbackFinalizationNamespaceInspection::begin' <<<"$$capture" | cut -d: -f1 )"; \
	database_after_line="$$( grep -nF 'let database_after = match inspect_current_database(record, state_db)? {' <<<"$$capture" | cut -d: -f1 )"; \
	test "$$binding_line" -lt "$$database_before_line"; test "$$database_before_line" -lt "$$namespace_line"; test "$$namespace_line" -lt "$$database_after_line"; \
	if rg -U -n '#\[derive\([^]]*Clone[^]]*\)\]\n(?:#\[[^\n]*\]\n)*(?:pub\([^)]*\)[[:space:]]+)?(?:struct|enum)[[:space:]]+UsrRollbackFinalization(?:Authority|AfterDeleteAuthority|DatabaseEvidence|NamespaceProof)' "$$authority" "$$proof"; then exit 1; else test "$$?" = 1; fi; \
	if rg -n 'impl Clone for UsrRollbackFinalization(?:Authority|AfterDeleteAuthority|DatabaseEvidence|NamespaceProof)' "$$authority" "$$proof"; then exit 1; else test "$$?" = 1; fi; \
	rg -n -F 'UsrRollbackFinalizationAuthority::capture(' crates/forge/src/client --glob '*.rs' --glob '!**/tests/**' --glob '!**/tests.rs' --glob '!**/*_tests.rs' --glob '!**/*_tests/**' --glob '!**/usr_rollback_finalization_authority.rs' > "$$refs"; \
	test "$$( wc -l < "$$refs" )" = 1; test "$$( cut -d: -f1 "$$refs" )" = "$$orchestrator"; \
	test "$$( grep -Fc '.revalidate(&journal)' "$$executor" )" = 1; \
	test "$$( grep -Fc '.attempt_record_bound_delete(&journal)' "$$executor" )" = 1; \
	test "$$( grep -Fc 'journal.delete_record_binding(' "$$authority" )" = 1; \
	if rg -n 'delete_revalidated_retained_cast|require_exact_public_source|inspect_exact_public_record|journal\.load\(' "$$authority" "$$proof" "$$executor"; then exit 1; else test "$$?" = 1; fi; \
	grep -Fq '.revalidate_after_journal_delete(&journal)' "$$executor"; \
	for branch in 'Ok(()) => {' 'TransitionJournalRecordDeleteState::Absent' 'Err(source) => Err(UsrRollbackFinalizationError::Delete(source))' 'DeleteAndPostDeleteAuthority {'; do grep -Fq "$$branch" "$$executor"; done; \
	grep -Fq 'require_exact_new_state_rollback_complete_topology' "$$proof" "$$topology"; \
	test "$$( grep -Fc 'require_exact_public_journal_absence(installation, journal)?;' "$$proof" )" = 2; \
	if rg -n 'canonical_journal_reopen|reopen_canonical_journal|TransitionJournalStore::(?:open|try_open)' "$$executor"; then exit 1; else test "$$?" = 1; fi; \
	sed -E 's,//.*$$,,' "$$executor" > "$$executor_code"; \
	if rg -n '^[[:space:]]*(loop|while|for)[[:space:]]|=[[:space:]]*(loop|while|for)[[:space:]]|retry|cleanup|run_(transaction|system)_triggers|\.advance[[:space:]]*\(|retained_exchange|boot_synchronize|attempt_move|state_db' "$$executor_code"; then exit 1; else test "$$?" = 1; fi; \
	grep -Fq 'for source in Source::THROUGH_ROLLBACK_COMPLETE {' "$$executor_tests/matrix.rs"; \
	grep -Fq 'assert_eq!(cases, 48);' "$$executor_tests/matrix.rs"; \
	grep -Fq 'assert_eq!(cases, 15);' "$$executor_tests/root_link_races.rs"; \
	grep -Fq 'assert_eq!(cases, 6);' "$$executor_tests/storage_reconciliation.rs"; \
	test "$$( grep -Fc 'release_invalidation_fixture_handles(fixture)' "$$gate_tests/finalization_restart.rs" )" = 2; \
	for disclaimer in 'fresh process-like' 'do not claim SIGKILL' 'reboot' 'power-loss durability'; do grep -Fq "$$disclaimer" "$$gate_tests/finalization_restart.rs"; done; \
	for endpoint_file in "$$root_links_endpoint" "$$fresh_endpoint_file"; do grep -Fq '.expect("exact generation-18 RootLinks NewState terminal must finalize cleanly");' "$$endpoint_file"; grep -Fq '.expect("finalized RootLinks NewState endpoint must remain clean");' "$$endpoint_file"; done; \
	grep -Fq 'const ALL: [Self; 3] = [' "$$startup_process_kill"; \
	for boundary in FinalPreRevalidation CanonicalUnlinked DeleteDirectorySynced; do grep -Fq "Self::$$boundary" "$$startup_process_kill"; done; \
	grep -Fq 'for epoch in Epoch::ALL {' "$$startup_process_kill"; \
	grep -Fq 'for source in CandidateSource::ALL {' "$$startup_process_kill"; \
	grep -Fq 'for boundary in TerminalDeleteKillBoundary::ALL {' "$$startup_process_kill"; \
	grep -Fq 'RootLinksComplete is outside the later process-kill source axis' "$$startup_process_kill"; \
	grep -Fq '        cases, 12,' "$$startup_process_kill"; \
	grep -Fq 'Command::new(env::current_exe().unwrap())' "$$startup_process_kill"; \
	grep -Fq 'Some(nix::libc::SIGKILL)' "$$startup_process_kill"; \
	grep -Fq 'let result = unsafe { nix::libc::kill(nix::libc::getpid(), nix::libc::SIGKILL) };' "$$startup_process_kill"; \
	grep -Fq 'arm_before_usr_rollback_finalization_final_revalidation(kill_self)' "$$startup_process_kill"; \
	test "$$( grep -Fc 'arm_journal_delete_durability_callback(' "$$startup_process_kill" )" = 2; \
	for boundary in CanonicalUnlinked DeleteDirectorySynced; do grep -Fq "JournalDeleteDurabilityBoundary::$$boundary" "$$startup_process_kill"; done; \
	grep -Fq 'struct PublicJournalIdentity {' "$$startup_process_kill"; \
	grep -Fq 'assert_journal_inventory(root, canonical_present);' "$$startup_process_kill"; \
	grep -Fq 'public_before.assert_same_public_anchors(final_public);' "$$startup_process_kill"; \
	grep -Fq 'StorageError::AcquireLock' "$$startup_process_kill"; \
	grep -Fq 'assert!(!canonical_path(&case.root).exists());' "$$startup_process_kill"; \
	grep -Fq 'assert_eq!(reopened.load().unwrap(), None)' "$$startup_process_kill"; \
	grep -Fq 'not a reboot or' "$$startup_process_kill"; \
	grep -Fq 'power-loss oracle' "$$startup_process_kill"; \
	test "$$( grep -Fc 'CleanSystemStartup::enter(' "$$startup_process_kill" )" = 2; \
	if rg -n 'arm_next_|finalize_usr_rollback|FaultPoint|StorageFault' "$$startup_process_kill"; then exit 1; else test "$$?" = 1; fi; \
	for file in "$$authority" "$$proof" "$$topology" "$$executor" "$$orchestrator" "$$startup_gate" "$$recovery_root" "$$reconciliation_root" "$$namespace_root" "$$authority_tests"/*.rs "$$executor_tests"/*.rs "$$gate_tests/finalization.rs" "$$gate_tests/finalization_restart.rs" "$$gate_tests/terminal_delete_process_kill.rs" "$$root_links_endpoint" "$$fresh_endpoint_file" misc/make/startup-rollback-finalization-tests.mk; do test "$$( wc -l < "$$file" )" -le 1000; done; \
	$(CARGO) test -p forge --lib "$$authority_prefix" -- --test-threads=1; \
	$(CARGO) test -p forge --lib "$$executor_prefix" -- --test-threads=1; \
	$(CARGO) test -p forge --lib "$$startup_prefix" -- --test-threads=1; \
	$(CARGO) test -p forge --lib "$$restart_prefix" -- --test-threads=1; \
	$(CARGO) test -p forge --lib "$$endpoint" -- --exact --test-threads=1; \
	$(CARGO) test -p forge --lib "$$fresh_endpoint" -- --exact --test-threads=1; \
	$(CARGO) test -p forge --lib "$$process_kill" -- --exact --test-threads=1
