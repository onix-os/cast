.PHONY: forge-startup-usr-rollback-active-reblit-candidate-dispatch-test

forge-startup-usr-rollback-active-reblit-candidate-dispatch-test:
	@set -euo pipefail; \
	timeout 10s mkdir -p "$(TOP_DIR)/target"; \
	listed="$$( timeout 10s mktemp "$(TOP_DIR)/target/active-reblit-startup-list.XXXXXXXXXXXX" )"; \
	trap 'timeout 10s rm -f "$$listed"' EXIT; \
	timeout 300s $(CARGO) test -p forge --lib -- --list | timeout 300s tee "$$listed" >/dev/null; \
	timeout 10s grep -q . "$$listed"; \
	prefix='client::startup_gate::usr_rollback_active_reblit::tests::'; \
	timeout 10s test "$$( timeout 10s grep "^$$prefix.*: test$$" "$$listed" | timeout 10s grep -Evc "^$$prefix"'(finalization_|boot_repair_required_|boot_repair_complete_|boot_repair_unverified_)' )" = 22; \
	for name in \
		candidate_wrapper_exchange_process_kill::startup_active_reblit_candidate_wrapper_exchange_process_kill_recovers_without_second_exchange \
		complete_authority_binding::startup_active_reblit_complete_route_authority_rejects_reopened_and_cross_root_journal_bindings \
		complete_evidence_races::startup_active_reblit_complete_route_rejects_database_provenance_journal_and_namespace_races \
		complete_evidence_races::startup_active_reblit_complete_route_root_links_rejects_all_root_abi_mutations_at_two_seams \
		complete_exclusions::startup_active_reblit_complete_route_preserves_operation_and_phase_ordering \
		complete_matrix::startup_active_reblit_complete_route_covers_all_twenty_four_exact_candidate_preserved_cases \
		complete_record_binding::startup_active_reblit_complete_route_bound_advance_same_byte_replacements_never_succeed \
		complete_record_binding::startup_active_reblit_complete_route_same_byte_successor_replacement_fails_reopened_binding \
		complete_record_binding::startup_active_reblit_complete_route_same_byte_successor_replacement_fails_same_store_binding \
		complete_restart::startup_active_reblit_complete_route_source_durable_fresh_handle_reopen_retries_only_the_route \
		complete_restart::startup_active_reblit_complete_route_successor_durable_fresh_handle_reopen_skips_the_route \
		complete_storage_faults::startup_active_reblit_complete_route_all_five_journal_faults_reopen_exact_durable_record \
		durability_failures::startup_active_reblit_candidate_dispatch_all_six_durability_barriers_fail_at_exact_prefix_for_both_origins \
		effect_failures::startup_active_reblit_candidate_dispatch_classifies_all_three_raw_exchange_reports_from_evidence \
		evidence_races::startup_active_reblit_candidate_dispatch_rejects_database_provenance_journal_and_namespace_races \
		exclusions::startup_active_reblit_candidate_dispatch_excludes_activate_archived_with_zero_effects \
		matrix::startup_active_reblit_candidate_dispatch_applied_matrix_uses_one_nonzero_wrapper_exchange \
		matrix::startup_active_reblit_candidate_dispatch_finish_matrix_preserves_without_exchange \
		new_state_regression::startup_active_reblit_candidate_dispatch_precedes_new_state_without_stealing_its_checkpoint \
		restart::startup_active_reblit_candidate_dispatch_source_durable_failure_fresh_entry_finishes_without_second_exchange \
		restart::startup_active_reblit_candidate_dispatch_successor_durable_failure_fresh_entry_never_redispatches_exchange \
		storage_faults::startup_active_reblit_candidate_dispatch_all_five_journal_faults_reopen_exact_source_or_successor; do \
		timeout 10s grep -Fqx "$$prefix$$name: test" "$$listed"; \
	done; \
	gate=crates/forge/src/client/startup_gate.rs; \
	orchestrator=crates/forge/src/client/startup_gate/usr_rollback_active_reblit.rs; \
	new_state=crates/forge/src/client/startup_gate/usr_rollback_new_state.rs; \
	leaf=crates/forge/src/client/startup_recovery/usr_rollback_candidate_preserve_dispatch.rs; \
	tests=crates/forge/src/client/startup_gate/usr_rollback_active_reblit/tests; \
	process_kill="$$tests/candidate_wrapper_exchange_process_kill.rs"; \
	process_harness="$$tests/candidate_wrapper_exchange_process_harness.rs"; \
	process_boundaries="$$tests/candidate_wrapper_exchange_kill_boundaries.rs"; \
	focused_exports=crates/forge/src/client/startup_reconciliation/focused_test_exports.rs; \
	timeout 10s grep -Fqx 'mod usr_rollback_active_reblit;' "$$gate"; \
	if timeout 10s awk 'previous == "#[cfg(test)]" && $$0 == "mod usr_rollback_active_reblit;" { found = 1 } { previous = $$0 } END { exit !found }' "$$gate"; then exit 1; fi; \
	timeout 10s grep -Fqx '#[cfg(test)]' "$$orchestrator"; \
	timeout 10s grep -Fqx 'mod tests;' "$$orchestrator"; \
	for module in candidate_wrapper_exchange_kill_boundaries candidate_wrapper_exchange_process_harness candidate_wrapper_exchange_process_kill complete_authority_binding complete_evidence_races complete_exclusions complete_matrix complete_record_binding complete_restart complete_storage_faults durability_failures effect_failures evidence_races exclusions matrix new_state_regression restart storage_faults support; do \
		timeout 10s grep -Fqx "mod $$module;" "$$tests/mod.rs"; \
	done; \
	timeout 10s test "$$( timeout 10s rg -n '^#\[test\]$$' "$$tests" --glob '!finalization_*.rs' --glob '!boot_repair_required_*.rs' --glob '!boot_repair_complete_*.rs' --glob '!boot_repair_unverified_*.rs' | timeout 10s wc -l )" = 22; \
	timeout 10s test "$$( timeout 10s grep -Fc 'CleanSystemStartup::enter(' "$$tests/support.rs" )" = 3; \
	timeout 10s test "$$( timeout 10s grep -Fc 'ActiveStateReservation::acquire().unwrap();' "$$tests/support.rs" )" = 3; \
	if timeout 10s rg -n --glob '!complete_*.rs' --glob '!support.rs' --glob '!finalization_*.rs' --glob '!boot_repair_required_*.rs' 'UsrRollbackCandidatePreserve(?:Seal::new_for_test|Authority::capture)|UsrRollbackActiveReblitCompleteRoute(?:Seal::new_for_test|Authority::capture)|usr_rollback_active_reblit::dispatch|super::super::dispatch|dispatch_usr_rollback_candidate_preserve_and_reopen|persist_usr_rollback_active_reblit_(candidate_preserve|complete_route)_and_reopen' "$$tests"; then exit 1; else status="$$?"; timeout 10s test "$$status" = 1; fi; \
	timeout 10s grep -Fqx 'pub(super) fn dispatch<'\''reservation>(' "$$orchestrator"; \
	timeout 10s test "$$( timeout 10s grep -Fc 'if record.operation != Operation::ActiveReblit {' "$$orchestrator" )" = 1; \
	timeout 10s grep -Fqx '        Phase::CandidatePreserveIntent => {' "$$orchestrator"; \
	timeout 10s grep -Fqx '        Phase::CandidatePreserved => {' "$$orchestrator"; \
	timeout 10s test "$$( timeout 10s grep -Fc 'UsrRollbackCandidatePreserveSeal::new();' "$$orchestrator" )" = 1; \
	timeout 10s test "$$( timeout 10s grep -Fc 'UsrRollbackCandidatePreserveAuthority::capture(' "$$orchestrator" )" = 1; \
	timeout 10s test "$$( timeout 10s grep -Fc 'dispatch_usr_rollback_candidate_preserve_and_reopen(journal, record, ready)?' "$$orchestrator" )" = 1; \
	timeout 10s test "$$( timeout 10s grep -Fc 'UsrRollbackActiveReblitCompleteRouteSeal::new();' "$$orchestrator" )" = 1; \
	timeout 10s test "$$( timeout 10s grep -Fc 'UsrRollbackActiveReblitCompleteRouteAuthority::capture(' "$$orchestrator" )" = 1; \
	timeout 10s test "$$( timeout 10s grep -Fc 'persist_usr_rollback_active_reblit_complete_route_and_reopen(journal, authority)?' "$$orchestrator" )" = 1; \
	boot_repair_started_arm="$$( timeout 10s sed -n '/Phase::BootRepairStarted => {/,/Phase::BootRepairComplete => {/p' "$$orchestrator" | timeout 10s sed '$$d' )"; \
	boot_repair_complete_arm="$$( timeout 10s sed -n '/Phase::BootRepairComplete => {/,/Phase::RollbackComplete => {/p' "$$orchestrator" | timeout 10s sed '$$d' )"; \
	boot_repair_started_handled="$$( timeout 10s grep -Fc 'Ok(Dispatch::Handled { journal, record })' <<<"$$boot_repair_started_arm" )"; \
	boot_repair_started_unhandled="$$( timeout 10s grep -Fc 'return Ok(Dispatch::Unhandled { journal, record });' <<<"$$boot_repair_started_arm" )"; \
	boot_repair_complete_handled="$$( timeout 10s grep -Fc 'Ok(Dispatch::Handled { journal, record })' <<<"$$boot_repair_complete_arm" )"; \
	boot_repair_complete_unhandled="$$( timeout 10s grep -Fc 'return Ok(Dispatch::Unhandled { journal, record });' <<<"$$boot_repair_complete_arm" )"; \
	timeout 10s test "$$boot_repair_started_handled" = 1; \
	timeout 10s test "$$boot_repair_started_unhandled" = 1; \
	timeout 10s test "$$boot_repair_complete_handled" = 1; \
	timeout 10s test "$$boot_repair_complete_unhandled" = 1; \
	handled_total="$$( timeout 10s grep -Fc 'Ok(Dispatch::Handled { journal, record })' "$$orchestrator" )"; \
	unhandled_total="$$( timeout 10s grep -Fc 'return Ok(Dispatch::Unhandled { journal, record });' "$$orchestrator" )"; \
	timeout 10s test "$$((handled_total - boot_repair_started_handled - boot_repair_complete_handled))" = 3; \
	timeout 10s test "$$((unhandled_total - boot_repair_started_unhandled - boot_repair_complete_unhandled))" = 4; \
	timeout 10s grep -Fqx '    UsrRollbackActiveReblitDispatch(#[from] usr_rollback_active_reblit::Error),' "$$gate"; \
	timeout 10s grep -Fqx '    CandidatePreserveAuthority(' "$$orchestrator"; \
	timeout 10s grep -Fqx '        #[from] crate::client::startup_reconciliation::UsrRollbackCandidatePreserveAuthorityError,' "$$orchestrator"; \
	timeout 10s grep -Fqx '    CandidatePreserveDispatch(#[from] crate::client::startup_recovery::UsrRollbackCandidatePreserveDispatchError),' "$$orchestrator"; \
	timeout 10s grep -Fqx '    CompleteRouteAuthority(' "$$orchestrator"; \
	timeout 10s grep -Fqx '        #[from] crate::client::startup_reconciliation::UsrRollbackActiveReblitCompleteRouteAuthorityError,' "$$orchestrator"; \
	timeout 10s grep -Fqx '    CompleteRoutePersistence(' "$$orchestrator"; \
	timeout 10s grep -Fqx '        #[from] crate::client::startup_recovery::UsrRollbackActiveReblitCompleteRoutePersistenceError,' "$$orchestrator"; \
	timeout 10s grep -Fq 'pub(super) fn assert_active_authority_dispatch_error(error: &startup_gate::Error) {' "$$tests/support.rs"; \
	timeout 10s grep -Fq 'UsrRollbackCandidatePreserveDispatchError::Authority(_)' "$$tests/support.rs"; \
	timeout 10s grep -Fq 'UsrRollbackActiveReblitCandidatePreservePersistenceError::Authority(_)' "$$tests/support.rs"; \
	candidate_arm="$$( timeout 10s sed -n '/Phase::CandidatePreserveIntent => {/,/Phase::CandidatePreserved => {/p' "$$orchestrator" | timeout 10s sed '$$d' )"; \
	if timeout 10s rg -n 'Phase::(FreshDbInvalidationIntent|FreshDbInvalidated|RollbackComplete)|finalize_usr_rollback' <<<"$$candidate_arm"; then exit 1; else status="$$?"; timeout 10s test "$$status" = 1; fi; \
	if timeout 10s rg -n '^[[:space:]]*(loop|while|for)[[:space:]]|=[[:space:]]*(loop|while|for)[[:space:]]|retry' "$$orchestrator"; then exit 1; else status="$$?"; timeout 10s test "$$status" = 1; fi; \
	if timeout 10s rg -n 'std::fs|(^|[^_[:alnum:]])fs::|diesel::|SqliteConnection|sql_query|\.execute[[:space:]]*\(|\.transaction[[:space:]]*\(|\.advance[[:space:]]*\(|journal\.delete|\.delete[[:space:]]*\(|remove_exact_fresh_transition|renameat|rename[[:space:]]*\(|unlink|mkdir|create_dir|set_permissions|chmod|sync_(all|data)|run_transaction_triggers|run_system_triggers|root_links|archive_previous|cleanup' "$$orchestrator"; then exit 1; else status="$$?"; timeout 10s test "$$status" = 1; fi; \
	reverse_line="$$( timeout 10s grep -nF 'super::startup_recovery::dispatch_usr_rollback_reverse_and_reopen(journal, ready)?' "$$gate" | timeout 10s cut -d: -f1 )"; \
	active_line="$$( timeout 10s grep -nF 'let (journal, record) = match usr_rollback_active_reblit::dispatch(' "$$gate" | timeout 10s cut -d: -f1 )"; \
	new_state_line="$$( timeout 10s grep -nF 'let (journal, record) = match usr_rollback_new_state::dispatch(' "$$gate" | timeout 10s cut -d: -f1 )"; \
	timeout 10s test "$$( timeout 10s grep -Fc 'usr_rollback_active_reblit::dispatch(' "$$gate" )" = 1; \
	timeout 10s test "$$reverse_line" -lt "$$active_line"; \
	timeout 10s test "$$active_line" -lt "$$new_state_line"; \
	handled="$$( timeout 10s sed -n '/usr_rollback_active_reblit::Dispatch::Handled { journal, record } => {/,/^                }$$/p' "$$gate" )"; \
	timeout 10s test "$$( timeout 10s grep -Fc 'let in_flight = state_db.audit_in_flight_transition()?;' <<<"$$handled" )" = 1; \
	timeout 10s test "$$( timeout 10s grep -Fc 'let pending = startup_reconciliation::PendingSystemTransition::inspect(' <<<"$$handled" )" = 1; \
	timeout 10s test "$$( timeout 10s grep -Fc 'return Err(Error::RecoveryPending(pending));' <<<"$$handled" )" = 1; \
	timeout 10s awk '$$0 == "                    let pending = startup_reconciliation::PendingSystemTransition::inspect(" { state = 1; next } state == 1 && $$0 == "                        installation," { state = 2; next } state == 2 && $$0 == "                        state_db," { state = 3; next } state == 3 && $$0 == "                        journal," { state = 4; next } state == 4 && $$0 == "                        record," { state = 5; next } state == 5 && $$0 == "                        in_flight," { state = 6; next } state == 6 && $$0 == "                    )" { found = 1 } END { exit !found }' <<<"$$handled"; \
	audit_line="$$( timeout 10s grep -nF 'let in_flight = state_db.audit_in_flight_transition()?;' <<<"$$handled" | timeout 10s cut -d: -f1 )"; \
	pending_line="$$( timeout 10s grep -nF 'let pending = startup_reconciliation::PendingSystemTransition::inspect(' <<<"$$handled" | timeout 10s cut -d: -f1 )"; \
	return_line="$$( timeout 10s grep -nF 'return Err(Error::RecoveryPending(pending));' <<<"$$handled" | timeout 10s cut -d: -f1 )"; \
	timeout 10s test "$$audit_line" -lt "$$pending_line"; \
	timeout 10s test "$$pending_line" -lt "$$return_line"; \
	timeout 10s grep -Fq 'pub(super) const WRAPPER_INDEX: usize = 13;' "$$tests/support.rs"; \
	timeout 10s grep -Fq 'for epoch in Epoch::ALL {' "$$tests/matrix.rs"; \
	timeout 10s test "$$( timeout 10s grep -Fc 'for source in CandidateSource::ALL {' "$$tests/matrix.rs" )" = 2; \
	timeout 10s test "$$( timeout 10s grep -Fc 'fixture.fixture.database_snapshot()' "$$tests/matrix.rs" )" -ge 4; \
	timeout 10s grep -Fq 'release_candidate_handles(fixture)' "$$tests/restart.rs"; \
	timeout 10s test "$$( timeout 10s grep -Fc 'enter_fresh_handles(retained.path())' "$$tests/restart.rs" )" = 2; \
	for fault in temporary_sync update_exchange update_first_directory_sync displaced_unlink update_final_directory_sync; do \
		timeout 10s grep -Fq "$$fault" "$$tests/storage_faults.rs"; \
	done; \
	for fault in CandidateSync CandidateWrapperSync ReservationWrapperSync RootsParentSync QuarantineParentSync FinalPostCapture; do \
		timeout 10s grep -Fq "ActiveReblitCandidatePreservePostExchangeDurabilityFaultPoint::$$fault" "$$tests/durability_failures.rs"; \
	done; \
	timeout 10s grep -Fq 'CandidateOrigin::Applied' "$$tests/durability_failures.rs"; \
	timeout 10s grep -Fq 'CandidateOrigin::AlreadySatisfied' "$$tests/durability_failures.rs"; \
	timeout 10s grep -Fq 'DURABILITY_FAULTS.len()' "$$tests/durability_failures.rs"; \
	for report in ErrorWithoutApply SuccessWithoutApply ErrorAfterApply; do \
		timeout 10s grep -Fq "ActiveReblitCandidatePreserveExchangeFault::$$report" "$$tests/effect_failures.rs"; \
	done; \
	timeout 10s grep -Fq 'active_reblit_candidate_preserve_exchange_attempt_count(), 2' "$$tests/effect_failures.rs"; \
	for blocker in DatabaseConflict MetadataProvenanceConflict; do timeout 10s grep -Fq "RecoveryBlocker::$$blocker" "$$tests/evidence_races.rs"; done; \
	timeout 10s grep -Fq 'arm_before_active_reblit_candidate_preserve_persistence_durable_trailing_evidence' "$$tests/evidence_races.rs"; \
	timeout 10s grep -Fq 'arm_before_active_reblit_candidate_preserve_durable_post_revalidation_capture' "$$tests/evidence_races.rs"; \
	timeout 10s grep -Fq 'OperationKind::Archived' "$$tests/exclusions.rs"; \
	timeout 10s grep -Fq 'OperationKind::NewState' "$$tests/new_state_regression.rs"; \
	timeout 10s grep -Fq 'active_reblit_candidate_preserve_exchange_attempt_count(), 0' "$$tests/new_state_regression.rs"; \
	timeout 10s grep -Fq 'pub(super) const ALL: [Self; 8] = [' "$$process_boundaries"; \
	for boundary in PostExchangePreRecapture BeforeCandidateSync BeforeCandidateWrapperSync BeforeReservationWrapperSync BeforeRootsParentSync BeforeQuarantineParentSync BeforeFinalPostCapture BeforeDurablePostRevalidation; do timeout 10s grep -Fq "Self::$$boundary" "$$process_boundaries"; done; \
	for seam in arm_before_active_reblit_candidate_preserve_reconciliation_capture arm_before_active_reblit_candidate_preserve_post_exchange_candidate_sync arm_before_active_reblit_candidate_preserve_post_exchange_candidate_wrapper_sync arm_before_active_reblit_candidate_preserve_post_exchange_reservation_wrapper_sync arm_before_active_reblit_candidate_preserve_post_exchange_roots_parent_sync arm_before_active_reblit_candidate_preserve_post_exchange_quarantine_parent_sync arm_before_active_reblit_candidate_preserve_post_exchange_final_post_capture arm_before_active_reblit_candidate_preserve_durable_post_revalidation_capture; do timeout 10s grep -Fq "$$seam" "$$process_boundaries"; timeout 10s grep -Fq "$$seam" "$$focused_exports"; done; \
	timeout 10s grep -Fq 'for epoch in Epoch::ALL {' "$$process_kill"; \
	timeout 10s grep -Fq 'for source in CandidateSource::ALL {' "$$process_kill"; \
	timeout 10s grep -Fq 'for boundary in CandidateWrapperExchangeKillBoundary::ALL {' "$$process_kill"; \
	timeout 10s grep -Fq '        cases, 32,' "$$process_kill"; \
	timeout 10s grep -Fq 'CandidateOrigin::Applied' "$$process_kill"; \
	timeout 10s grep -Fq 'Command::new(env::current_exe().unwrap())' "$$process_harness"; \
	timeout 10s grep -Fq '.arg(TEST_NAME)' "$$process_harness"; \
	timeout 10s grep -Fq '.arg("--exact")' "$$process_harness"; \
	timeout 10s grep -Fq '.arg("--test-threads=1")' "$$process_harness"; \
	timeout 10s grep -Fq 'const CHILD_DEADLINE: Duration = Duration::from_secs(15);' "$$process_harness"; \
	timeout 10s grep -Fq 'Some(nix::libc::SIGKILL)' "$$process_kill"; \
	timeout 10s grep -Fq 'crash_status.signal()' "$$process_kill"; \
	timeout 10s grep -Fq 'active_reblit_candidate_preserve_exchange_attempt_count(),' "$$process_harness"; \
	timeout 10s grep -Fq 'active_reblit_candidate_preserve_exchange_attempt_count(), 0' "$$process_kill"; \
	timeout 10s grep -Fq 'take_active_reblit_candidate_preserve_post_exchange_durability_events()' "$$process_kill"; \
	timeout 10s grep -Fq 'snapshot_startup_recovery_namespace' "$$process_kill"; \
	timeout 10s grep -Fq 'struct PublicJournalIdentity {' "$$process_harness"; \
	timeout 10s grep -Fq 'struct ExistingCandidateDatabase {' "$$process_harness"; \
	timeout 10s grep -Fq 'struct WrapperExchangeEvidence {' "$$process_harness"; \
	timeout 10s grep -Fq 'struct DeadlineChild {' "$$process_harness"; \
	timeout 10s grep -Fq 'Option<(PathBuf, (u64, u64))>' "$$process_harness"; \
	timeout 10s grep -Fq 'external ActiveReblit wrapper-exchange control does not match' "$$process_harness"; \
	timeout 10s test "$$( timeout 10s grep -Fc 'CleanSystemStartup::enter(' "$$process_kill" )" = 2; \
	timeout 10s grep -Fq 'release_candidate_handles(fixture)' "$$process_kill"; \
	timeout 10s grep -Fq 'install_persistent_database(&mut fixture)' "$$process_kill"; \
	timeout 10s grep -Fq 'RollbackActionOutcome::AlreadySatisfied' "$$process_harness"; \
	timeout 10s grep -Fq 'same-boot process death only' "$$process_harness"; \
	if timeout 10s rg -n 'arm_next_|finalize_usr_rollback|FaultPoint|StorageFault|dispatch_usr_rollback|persist_usr_rollback|journal\.(advance|delete)' "$$process_kill" "$$process_harness" "$$process_boundaries"; then exit 1; else status="$$?"; timeout 10s test "$$status" = 1; fi; \
	for file in "$$gate" "$$orchestrator" "$$new_state" "$$leaf" "$$focused_exports" "$$tests"/*.rs misc/make/startup-rollback-active-reblit-candidate-dispatch-tests.mk; do \
		timeout 10s test "$$( timeout 10s wc -l < "$$file" )" -le 1000; \
	done; \
	timeout 1800s $(CARGO) test -p forge --lib "$$prefix" -- --test-threads=1
