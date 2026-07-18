.PHONY: forge-startup-usr-rollback-activate-archived-candidate-dispatch-test

forge-startup-usr-rollback-activate-archived-candidate-dispatch-test:
	@set -euo pipefail; \
	timeout 10s mkdir -p "$(TOP_DIR)/target"; \
	listed="$$( timeout 10s mktemp "$(TOP_DIR)/target/activate-archived-candidate-dispatch-list.XXXXXXXXXXXX" )"; \
	trap 'timeout 10s rm -f "$$listed"' EXIT; \
	timeout 300s $(CARGO) test -p forge --lib -- --list | timeout 30s tee "$$listed" >/dev/null; \
	persistence_prefix='client::startup_recovery::usr_rollback_activate_archived_candidate_preserve_persistence::tests::'; \
	startup_prefix='client::startup_gate::usr_rollback_activate_archived::tests::'; \
	timeout 10s test "$$( timeout 10s grep -c "^$$persistence_prefix"'.*: test$$' "$$listed" )" = 11; \
	for name in \
		matrix::startup_archived_candidate_preserve_persistence_applied_matrix_persists_exact_successor \
		matrix::startup_archived_candidate_preserve_persistence_finish_matrix_persists_exact_successor \
		matrix::startup_archived_candidate_preserve_persistence_changes_only_journal_after_durability \
		evidence_races::startup_archived_candidate_preserve_persistence_rejects_reopened_and_cross_root_journals \
		evidence_races::startup_archived_candidate_preserve_persistence_final_evidence_races_fail_before_advance \
		storage_reopen::startup_archived_candidate_preserve_persistence_faults_reopen_exact_source_or_successor \
		storage_reopen::startup_archived_candidate_preserve_persistence_consumes_old_store_and_reopens_success \
		restart::startup_archived_candidate_preserve_source_fault_restart_finishes_without_second_move \
		restart::startup_archived_candidate_preserve_successor_fault_restart_skips_preservation \
		production_dispatch::startup_activate_archived_candidate_preserve_production_leaf_dispatches_all_exact_cases_once \
		production_dispatch::startup_activate_archived_candidate_preserve_production_leaf_rejects_cross_operation_pairing; do \
		timeout 10s grep -Fqx "$$persistence_prefix$$name: test" "$$listed"; \
	done; \
	for name in \
		candidate_admission::startup_activate_archived_candidate_child_handles_only_exact_operation_and_phase \
		candidate_admission::startup_activate_archived_candidate_child_excludes_other_operations_and_phases_without_effects \
		candidate_evidence_races::startup_activate_archived_candidate_dispatch_rejects_every_final_evidence_race \
		candidate_matrix::startup_activate_archived_candidate_dispatch_applied_matrix_moves_once_and_returns_pending \
		candidate_matrix::startup_activate_archived_candidate_dispatch_finish_matrix_never_moves_and_returns_pending \
		candidate_matrix::startup_activate_archived_candidate_dispatch_never_falls_through_to_completion_in_same_entry \
		candidate_move_process_kill::startup_activate_archived_candidate_move_process_kill_recovers_without_second_move \
		candidate_restart::startup_activate_archived_candidate_source_fault_fresh_entry_finishes_without_second_move \
		candidate_restart::startup_activate_archived_candidate_successor_fault_fresh_entry_completes_without_second_move; do \
		timeout 10s grep -Fqx "$$startup_prefix$$name: test" "$$listed"; \
	done; \
	gate=crates/forge/src/client/startup_gate.rs; \
	child=crates/forge/src/client/startup_gate/usr_rollback_activate_archived.rs; \
	reconciliation=crates/forge/src/client/startup_reconciliation.rs; \
	authority=crates/forge/src/client/startup_reconciliation/usr_rollback_candidate_preserve_authority.rs; \
	archived_authority=crates/forge/src/client/startup_reconciliation/usr_rollback_candidate_preserve_authority/archived_effect.rs; \
	archived_persistence_authority=crates/forge/src/client/startup_reconciliation/usr_rollback_candidate_preserve_authority/archived_effect/persistence.rs; \
	production_leaf=crates/forge/src/client/startup_recovery/usr_rollback_candidate_preserve_dispatch.rs; \
	recovery=crates/forge/src/client/startup_recovery.rs; \
	persistence=crates/forge/src/client/startup_recovery/usr_rollback_activate_archived_candidate_preserve_persistence.rs; \
	persistence_tests=crates/forge/src/client/startup_recovery/usr_rollback_activate_archived_candidate_preserve_persistence/tests; \
	startup_tests=crates/forge/src/client/startup_gate/usr_rollback_activate_archived/tests; \
	process_kill="$$startup_tests/candidate_move_process_kill.rs"; \
	process_kill_boundaries="$$startup_tests/candidate_process_kill_boundaries.rs"; \
	timeout 10s grep -Fqx 'mod usr_rollback_activate_archived;' "$$gate"; \
	if timeout 10s awk 'previous == "#[cfg(test)]" && $$0 == "mod usr_rollback_activate_archived;" { found = 1 } { previous = $$0 } END { exit !found }' "$$gate"; then exit 1; fi; \
	timeout 10s grep -Fq 'record.operation != Operation::ActivateArchived' "$$child"; \
	timeout 10s grep -Fqx '        Phase::CandidatePreserveIntent => {' "$$child"; \
	timeout 10s grep -Fq 'UsrRollbackCandidatePreserveAdmission::Apply(authority)' "$$child"; \
	timeout 10s grep -Fq 'UsrRollbackCandidatePreserveAdmission::Finish(authority)' "$$child"; \
	timeout 10s grep -Fq 'dispatch_usr_rollback_candidate_preserve_and_reopen(journal, record, ready)' "$$child"; \
	candidate_arm="$$( timeout 10s sed -n '/Phase::CandidatePreserveIntent => {/,/Phase::CandidatePreserved => {/p' "$$child" | timeout 10s sed '$$d' )"; \
	if timeout 10s rg -n 'UsrRollbackActivateArchivedCompleteRouteAuthority::capture|persist_usr_rollback_activate_archived_complete_route_and_reopen|Phase::RollbackComplete|finalize_usr_rollback' <<<"$$candidate_arm"; then exit 1; else status="$$?"; timeout 10s test "$$status" = 1; fi; \
	archived_line="$$( timeout 10s grep -nF 'match usr_rollback_activate_archived::dispatch(' "$$gate" | timeout 10s cut -d: -f1 )"; \
	active_line="$$( timeout 10s grep -nF 'match usr_rollback_active_reblit::dispatch(' "$$gate" | timeout 10s cut -d: -f1 )"; \
	new_state_line="$$( timeout 10s grep -nF 'match usr_rollback_new_state::dispatch(' "$$gate" | timeout 10s cut -d: -f1 )"; \
	timeout 10s test "$$archived_line" -lt "$$active_line"; \
	timeout 10s test "$$active_line" -lt "$$new_state_line"; \
	handled="$$( timeout 10s sed -n '/usr_rollback_activate_archived::Dispatch::Handled/,/^                }/p' "$$gate" )"; \
	timeout 10s grep -Fq 'PendingSystemTransition::inspect' <<<"$$handled"; \
	timeout 10s grep -Fq 'return Err(Error::RecoveryPending(pending));' <<<"$$handled"; \
	timeout 10s grep -Fqx 'mod usr_rollback_activate_archived_candidate_preserve_persistence;' "$$recovery"; \
	if timeout 10s awk 'previous == "#[cfg(test)]" && $$0 == "mod usr_rollback_activate_archived_candidate_preserve_persistence;" { found = 1 } { previous = $$0 } END { exit !found }' "$$recovery"; then exit 1; fi; \
	timeout 10s grep -Fq 'UsrRollbackCandidatePreserveApplyEffectSelection::MoveArchived' "$$production_leaf" "$$authority"; \
	timeout 10s grep -Fq 'UsrRollbackCandidatePreserveFinishDurabilitySelection::Archived' "$$production_leaf" "$$authority"; \
	timeout 10s grep -Fq 'persist_usr_rollback_archived_candidate_preserve_and_reopen(journal, durable)' "$$production_leaf"; \
	timeout 10s test "$$( timeout 10s grep -Fc 'authority.revalidate(&journal)' "$$persistence" )" = 2; \
	timeout 10s test "$$( timeout 10s grep -Fc 'journal.advance(&source_record, &successor)' "$$persistence" )" = 1; \
	timeout 10s test "$$( timeout 10s grep -Fc 'reopen_canonical_journal(&installation)' "$$persistence" )" = 1; \
	timeout 10s grep -Fq 'if actual == source_record =>' "$$persistence"; \
	timeout 10s grep -Fq 'if actual == successor =>' "$$persistence"; \
	timeout 10s grep -Fq 'ArchivedDurabilityOrigin::Applied' "$$archived_authority"; \
	timeout 10s grep -Fq 'ArchivedDurabilityOrigin::AlreadySatisfied' "$$archived_authority"; \
	if timeout 10s rg -n 'pub[^\n]*(ArchivedDurabilityOrigin|origin)' "$$archived_authority" "$$archived_persistence_authority"; then exit 1; else status="$$?"; timeout 10s test "$$status" = 1; fi; \
	production_code="$$( timeout 10s sed -E 's,//.*$$,,' "$$child" "$$persistence" "$$archived_authority" "$$archived_persistence_authority" )"; \
	if timeout 10s rg -n '^[[:space:]]*(loop|while)[[:space:]]|=[[:space:]]*(loop|while)[[:space:]]|retry' <<<"$$production_code"; then exit 1; else status="$$?"; timeout 10s test "$$status" = 1; fi; \
	for race in Database Provenance Journal Installation Namespace Plan; do timeout 10s grep -Fq "EvidenceRace::$$race" "$$persistence_tests/evidence_races.rs" "$$startup_tests/candidate_evidence_races.rs"; done; \
	for fault in temporary_sync update_exchange update_first_directory_sync displaced_unlink update_final_directory_sync; do timeout 10s grep -Fq "$$fault" "$$persistence_tests/storage_reopen.rs"; done; \
	timeout 10s grep -Fqx 'mod candidate_move_process_kill;' "$$startup_tests/mod.rs"; \
	timeout 10s grep -Fqx 'mod candidate_process_kill_boundaries;' "$$startup_tests/mod.rs"; \
	timeout 10s grep -Fq 'for epoch in Epoch::ALL {' "$$process_kill"; \
	timeout 10s grep -Fq 'for source in CandidateSource::ALL {' "$$process_kill"; \
	timeout 10s grep -Fq 'for boundary in CandidateProcessKillBoundary::ALL {' "$$process_kill"; \
	timeout 10s grep -Fq 'cases, 28,' "$$process_kill"; \
	for boundary in PostMovePreRecapture BeforeCandidateSync BeforeStagingParentSync BeforeTargetParentSync BeforeRootsParentSync BeforeFinalPostCapture BeforeDurablePostRevalidation; do timeout 10s grep -Fq "Self::$$boundary" "$$process_kill_boundaries"; done; \
	for hook in arm_before_archived_candidate_preserve_move_reconciliation_capture arm_before_archived_candidate_preserve_post_candidate_sync arm_before_archived_candidate_preserve_post_staging_parent_sync arm_before_archived_candidate_preserve_post_target_parent_sync arm_before_archived_candidate_preserve_post_roots_parent_sync arm_before_archived_candidate_preserve_post_final_capture arm_before_archived_candidate_preserve_durable_post_revalidation_capture; do timeout 10s grep -Fq "$$hook" "$$process_kill_boundaries"; done; \
	timeout 10s grep -Fq 'kill_after_real_candidate_move' "$$process_kill"; \
	timeout 10s grep -Fq 'nix::libc::kill(nix::libc::getpid(), nix::libc::SIGKILL)' "$$process_kill"; \
	timeout 10s grep -Fq 'crash_status.signal()' "$$process_kill"; \
	timeout 10s grep -Fq 'Some(nix::libc::SIGKILL)' "$$process_kill"; \
	timeout 10s grep -Fq 'Command::new(env::current_exe().unwrap())' "$$process_kill"; \
	timeout 10s grep -Fq 'const CHILD_DEADLINE: Duration = Duration::from_secs(15);' "$$process_kill"; \
	timeout 10s grep -Fq 'let _ = child.kill();' "$$process_kill"; \
	timeout 10s grep -Fq 'let status = child.wait().unwrap();' "$$process_kill"; \
	timeout 10s test "$$( timeout 10s grep -Fc 'CleanSystemStartup::enter(' "$$process_kill" )" = 2; \
	timeout 10s grep -Fq 'Installation::open(&case.root, None)' "$$process_kill"; \
	timeout 10s grep -Fq 'open_state_database(&installation)' "$$process_kill"; \
	timeout 10s grep -Fq 'snapshot_startup_recovery_namespace(&root)' "$$process_kill"; \
	timeout 10s grep -Fq 'snapshot_startup_recovery_namespace(&case.root)' "$$process_kill"; \
	timeout 10s grep -Fq 'RollbackActionOutcome::AlreadySatisfied' "$$process_kill"; \
	timeout 10s grep -Fq 'archived_candidate_preserve_move_attempt_count(), 0' "$$process_kill"; \
	timeout 10s grep -Fq 'same-boot process death only' "$$process_kill"; \
	timeout 10s grep -Fq 'power-loss oracle' "$$process_kill"; \
	timeout 10s grep -Fq 'historical record epoch is not a reboot' "$$process_kill"; \
	if timeout 10s rg -n 'dispatch_usr_rollback_candidate_preserve_and_reopen|persist_usr_rollback|journal\.(advance|delete)|reconcile_move|arm_archived_candidate_preserve_move_fault|arm_next_|StorageFault' "$$process_kill" "$$process_kill_boundaries"; then exit 1; else status="$$?"; timeout 10s test "$$status" = 1; fi; \
	for file in "$$gate" "$$child" "$$reconciliation" "$$authority" "$$archived_authority" "$$archived_persistence_authority" "$$production_leaf" "$$recovery" "$$persistence" "$$persistence_tests"/*.rs "$$startup_tests"/*.rs misc/make/startup-rollback-activate-archived-candidate-dispatch-tests.mk Makefile misc/make/help.mk; do \
		timeout 10s test "$$( timeout 10s wc -l < "$$file" )" -le 1000; \
	done; \
	timeout 1800s $(CARGO) test -p forge --lib "$$persistence_prefix" -- --test-threads=1; \
	timeout 1800s $(CARGO) test -p forge --lib startup_activate_archived_candidate -- --test-threads=1
