.PHONY: forge-startup-usr-rollback-active-reblit-candidate-dispatch-test

forge-startup-usr-rollback-active-reblit-candidate-dispatch-test:
	@set -euo pipefail; \
	timeout 10s mkdir -p "$(TOP_DIR)/target"; \
	listed="$$( timeout 10s mktemp "$(TOP_DIR)/target/active-reblit-startup-list.XXXXXXXXXXXX" )"; \
	trap 'timeout 10s rm -f "$$listed"' EXIT; \
	timeout 300s $(CARGO) test -p forge --lib -- --list | timeout 30s tee "$$listed" >/dev/null; \
	timeout 10s grep -q . "$$listed"; \
	prefix='client::startup_gate::usr_rollback_active_reblit::tests::'; \
	timeout 10s test "$$( timeout 10s grep -c "^$$prefix.*: test$$" "$$listed" )" = 17; \
	for name in \
		complete_authority_binding::startup_active_reblit_complete_route_authority_rejects_reopened_and_cross_root_journal_bindings \
		complete_evidence_races::startup_active_reblit_complete_route_rejects_database_provenance_journal_and_namespace_races \
		complete_exclusions::startup_active_reblit_complete_route_preserves_operation_and_phase_ordering \
		complete_matrix::startup_active_reblit_complete_route_covers_all_sixteen_exact_candidate_preserved_cases \
		complete_restart::startup_active_reblit_complete_route_source_durable_failure_converges_with_fresh_handles \
		complete_restart::startup_active_reblit_complete_route_successor_durable_failure_remains_terminal_with_fresh_handles \
		complete_storage_faults::startup_active_reblit_complete_route_all_five_journal_faults_converge_on_second_entry \
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
	timeout 10s grep -Fqx 'mod usr_rollback_active_reblit;' "$$gate"; \
	if timeout 10s awk 'previous == "#[cfg(test)]" && $$0 == "mod usr_rollback_active_reblit;" { found = 1 } { previous = $$0 } END { exit !found }' "$$gate"; then exit 1; fi; \
	timeout 10s grep -Fqx '#[cfg(test)]' "$$orchestrator"; \
	timeout 10s grep -Fqx 'mod tests;' "$$orchestrator"; \
	for module in complete_authority_binding complete_evidence_races complete_exclusions complete_matrix complete_restart complete_storage_faults durability_failures effect_failures evidence_races exclusions matrix new_state_regression restart storage_faults support; do \
		timeout 10s grep -Fqx "mod $$module;" "$$tests/mod.rs"; \
	done; \
	timeout 10s test "$$( timeout 10s rg -n '^#\[test\]$$' "$$tests" | timeout 10s wc -l )" = 17; \
	timeout 10s test "$$( timeout 10s grep -Fc 'CleanSystemStartup::enter(installation, database, &reservation)' "$$tests/support.rs" )" = 1; \
	timeout 10s test "$$( timeout 10s grep -Fc 'ActiveStateReservation::acquire().unwrap();' "$$tests/support.rs" )" = 1; \
	if timeout 10s rg -n --glob '!complete_authority_binding.rs' 'new_for_test|UsrRollbackActiveReblitCompleteRouteAuthority::capture|usr_rollback_active_reblit::dispatch|super::super::dispatch|dispatch_usr_rollback_candidate_preserve_and_reopen|persist_usr_rollback_active_reblit_(candidate_preserve|complete_route)_and_reopen' "$$tests"; then exit 1; else status="$$?"; timeout 10s test "$$status" = 1; fi; \
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
	timeout 10s test "$$( timeout 10s grep -Fc 'Ok(Dispatch::Handled { journal, record })' "$$orchestrator" )" = 2; \
	timeout 10s test "$$( timeout 10s grep -Fc 'return Ok(Dispatch::Unhandled { journal, record });' "$$orchestrator" )" = 3; \
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
	if timeout 10s rg -n 'Phase::(FreshDbInvalidationIntent|FreshDbInvalidated|RollbackComplete)' "$$orchestrator"; then exit 1; else status="$$?"; timeout 10s test "$$status" = 1; fi; \
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
	for file in "$$gate" "$$orchestrator" "$$new_state" "$$leaf" "$$tests"/*.rs misc/make/startup-rollback-active-reblit-candidate-dispatch-tests.mk; do \
		timeout 10s test "$$( timeout 10s wc -l < "$$file" )" -le 1000; \
	done; \
	timeout 1800s $(CARGO) test -p forge --lib "$$prefix" -- --test-threads=1
