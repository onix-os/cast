.PHONY: forge-startup-usr-rollback-new-state-dispatch-test

forge-startup-usr-rollback-new-state-dispatch-test:
	@set -euo pipefail; \
	timeout 10s mkdir -p "$(TOP_DIR)/target"; \
	listed="$$( timeout 10s mktemp "$(TOP_DIR)/target/new-state-suffix-list.XXXXXXXXXXXX" )"; \
	production="$$( timeout 10s mktemp "$(TOP_DIR)/target/new-state-suffix-production.XXXXXXXXXXXX" )"; \
	inventory="$$( timeout 10s mktemp "$(TOP_DIR)/target/new-state-suffix-inventory.XXXXXXXXXXXX" )"; \
	trap 'timeout 10s rm -f "$$listed" "$$production" "$$inventory"' EXIT; \
	timeout 300s $(CARGO) test -p forge --lib -- --list | timeout 30s tee "$$listed" >/dev/null; \
	timeout 10s grep -q . "$$listed"; \
	prefix='client::startup_gate::usr_rollback_new_state::tests::'; \
	timeout 10s test "$$( timeout 10s grep -c "^$$prefix.*: test$$" "$$listed" )" = 25; \
	for name in \
		exclusions::startup_new_state_suffix_leaves_every_non_new_state_operation_zero_effect \
		exclusions::startup_new_state_suffix_retains_rollback_complete_with_zero_suffix_effects \
		failures::startup_new_state_suffix_candidate_effect_failure_retries_once_on_a_fresh_entry \
		failures::startup_new_state_suffix_database_effect_failures_restart_from_exact_present_or_joint_absence \
		failures::startup_new_state_suffix_source_durable_storage_failures_repeat_no_candidate_or_database_effect \
		failures::startup_new_state_suffix_successor_durable_storage_failure_never_redispatches_the_completed_phase \
		failures::startup_new_state_suffix_reloads_an_overwritten_durable_successor_before_dispatch \
		failures::startup_new_state_suffix_evidence_change_defers_before_route_or_later_effects \
		matrix::startup_new_state_suffix_routes_every_exact_candidate_preserved_matrix_without_later_effects \
		matrix::startup_new_state_suffix_invalidates_present_or_accepts_joint_absence_for_every_exact_matrix \
		matrix::startup_new_state_suffix_completes_every_exact_invalidated_outcome_without_repeating_effects \
		preparation_failures::startup_new_state_suffix_target_creation_not_applied_retries_only_creation_on_a_fresh_entry \
		preparation_failures::startup_new_state_suffix_target_creation_post_evidence_change_defers_to_normalization_without_move \
		preparation_failures::startup_new_state_suffix_target_creation_post_evidence_failure_consumes_creation_before_fresh_recovery \
		preparation_failures::startup_new_state_suffix_target_normalization_not_applied_retries_only_normalization \
		preparation_failures::startup_new_state_suffix_normalization_durability_failure_restarts_from_canonical_target_without_second_chmod \
		preparation_failures::startup_new_state_suffix_normalization_post_evidence_failure_requires_a_fresh_repaired_entry \
		preparation_failures::startup_new_state_suffix_pre_move_durability_failure_makes_zero_move_and_retries_with_fresh_evidence \
		preparation_failures::startup_new_state_suffix_pre_move_evidence_failure_makes_zero_move_until_repaired \
		preparation_failures::startup_new_state_suffix_post_move_durability_failure_finishes_without_second_move \
		preparation_failures::startup_new_state_suffix_post_move_evidence_failure_reopens_preserved_layout_without_second_move \
		sequence::startup_new_state_suffix_consumes_exactly_one_checkpoint_per_entry_for_every_target_prefix \
		sequence::startup_new_state_suffix_runs_the_exact_multi_entry_sequence_without_same_entry_fallthrough \
		sequence::startup_new_state_suffix_reacquires_fresh_installation_database_journal_and_reservation_handles \
		storage_faults::startup_new_state_suffix_all_five_journal_faults_reenter_each_of_four_persistence_boundaries_exactly; do \
		timeout 10s grep -Fqx "$$prefix$$name: test" "$$listed"; \
	done; \
	gate=crates/forge/src/client/startup_gate.rs; \
	orchestrator=crates/forge/src/client/startup_gate/usr_rollback_new_state.rs; \
	recovery_root=crates/forge/src/client/startup_recovery.rs; \
	candidate_leaf=crates/forge/src/client/startup_recovery/usr_rollback_candidate_preserve_dispatch.rs; \
	fresh_leaf=crates/forge/src/client/startup_recovery/usr_rollback_fresh_db_invalidation_dispatch.rs; \
	tests=crates/forge/src/client/startup_gate/usr_rollback_new_state/tests; \
	for seal in UsrRollbackCandidatePreserveSeal UsrRollbackFreshDbInvalidationRouteSeal UsrRollbackFreshDbInvalidationSeal UsrRollbackCompleteRouteSeal; do \
		timeout 10s test "$$( timeout 10s rg -l "^pub\\(in crate::client\\) struct $$seal \\{" crates/forge/src/client --glob '*.rs' )" = "$$orchestrator"; \
		timeout 10s grep -Fq "$$seal," "$$gate"; \
		timeout 10s grep -Fqx "pub(in crate::client) struct $$seal {" "$$orchestrator"; \
		timeout 10s awk -v seal="$$seal" '$$0 == "pub(in crate::client) struct " seal " {" { open = 1; next } open && $$0 == "    _private: ()," { field = 1; next } open && $$0 == "}" { closed = 1; open = 0 } END { exit !(field && closed) }' "$$orchestrator"; \
		seal_impl="$$( timeout 10s sed -n "/^impl $$seal {/,/^}/p" "$$orchestrator" )"; \
		timeout 10s test "$$( timeout 10s grep -Fc '    fn new() -> Self {' <<<"$$seal_impl" )" = 1; \
		timeout 10s test "$$( timeout 10s grep -Fc '    pub(in crate::client) fn new_for_test() -> Self {' <<<"$$seal_impl" )" = 1; \
		timeout 10s awk '$$0 == "    #[cfg(test)]" { gated = 1; next } gated && $$0 == "    pub(in crate::client) fn new_for_test() -> Self {" { found++; gated = 0; next } gated { gated = 0 } END { exit found != 1 }' <<<"$$seal_impl"; \
	done; \
	for seal in UsrRollbackCandidatePreserveEffectSeal UsrRollbackCandidatePreserveDurabilitySeal UsrRollbackFreshDbInvalidationEffectSeal; do \
		seal_file="$$candidate_leaf"; \
		if timeout 10s test "$$seal" = UsrRollbackFreshDbInvalidationEffectSeal; then seal_file="$$fresh_leaf"; fi; \
		timeout 10s test "$$( timeout 10s rg -l "^pub\\(in crate::client\\) struct $$seal \\{" crates/forge/src/client --glob '*.rs' )" = "$$seal_file"; \
		timeout 10s grep -Fq "$$seal," "$$recovery_root"; \
		timeout 10s grep -Fqx "pub(in crate::client) struct $$seal {" "$$seal_file"; \
		timeout 10s awk -v seal="$$seal" '$$0 == "pub(in crate::client) struct " seal " {" { open = 1; next } open && $$0 == "    _private: ()," { field = 1; next } open && $$0 == "}" { closed = 1; open = 0 } END { exit !(field && closed) }' "$$seal_file"; \
		seal_impl="$$( timeout 10s sed -n "/^impl $$seal {/,/^}/p" "$$seal_file" )"; \
		timeout 10s test "$$( timeout 10s grep -Fc '    fn new() -> Self {' <<<"$$seal_impl" )" = 1; \
		timeout 10s test "$$( timeout 10s grep -Fc '    pub(in crate::client) fn new_for_test() -> Self {' <<<"$$seal_impl" )" = 1; \
		timeout 10s awk '$$0 == "    #[cfg(test)]" { gated = 1; next } gated && $$0 == "    pub(in crate::client) fn new_for_test() -> Self {" { found++; gated = 0; next } gated { gated = 0 } END { exit found != 1 }' <<<"$$seal_impl"; \
	done; \
	timeout 10s grep -Fqx 'mod usr_rollback_new_state;' "$$gate"; \
	timeout 10s grep -Fqx '#[cfg(test)]' "$$orchestrator"; \
	timeout 10s grep -Fqx 'mod tests;' "$$orchestrator"; \
	timeout 10s grep -Fqx 'mod exclusions;' "$$tests/mod.rs"; \
	timeout 10s grep -Fqx 'mod failures;' "$$tests/mod.rs"; \
	timeout 10s grep -Fqx 'mod matrix;' "$$tests/mod.rs"; \
	timeout 10s grep -Fqx 'mod preparation_failures;' "$$tests/mod.rs"; \
	timeout 10s grep -Fqx 'mod sequence;' "$$tests/mod.rs"; \
	timeout 10s grep -Fqx 'mod storage_faults;' "$$tests/mod.rs"; \
	timeout 10s grep -Fqx 'mod support;' "$$tests/mod.rs"; \
	timeout 10s test "$$( timeout 10s grep -Fc 'CleanSystemStartup::enter(installation, database, &reservation)' "$$tests/support.rs" )" = 1; \
	timeout 10s test "$$( timeout 10s grep -Fc 'ActiveStateReservation::acquire().unwrap();' "$$tests/support.rs" )" = 1; \
	if timeout 10s rg -n 'new_for_test|usr_rollback_new_state::dispatch|dispatch_usr_rollback_.*_and_reopen|persist_usr_rollback_.*_and_reopen' "$$tests"; then exit 1; else status="$$?"; timeout 10s test "$$status" = 1; fi; \
	timeout 10s grep -Fqx 'pub(super) fn dispatch<'\''reservation>(' "$$orchestrator"; \
	timeout 10s test "$$( timeout 10s grep -Fc 'if record.operation != Operation::NewState {' "$$orchestrator" )" = 1; \
	timeout 10s test "$$( timeout 10s grep -Fc '    match record.phase {' "$$orchestrator" )" = 1; \
	for phase in CandidatePreserveIntent CandidatePreserved FreshDbInvalidationIntent FreshDbInvalidated; do \
		timeout 10s test "$$( timeout 10s grep -Fc "        Phase::$$phase => {" "$$orchestrator" )" = 1; \
	done; \
	timeout 10s test "$$( timeout 10s grep -Fc 'UsrRollbackCandidatePreserveSeal::new();' "$$orchestrator" )" = 1; \
	timeout 10s test "$$( timeout 10s grep -Fc 'UsrRollbackFreshDbInvalidationRouteSeal::new();' "$$orchestrator" )" = 1; \
	timeout 10s test "$$( timeout 10s grep -Fc 'UsrRollbackFreshDbInvalidationSeal::new();' "$$orchestrator" )" = 1; \
	timeout 10s test "$$( timeout 10s grep -Fc 'UsrRollbackCompleteRouteSeal::new();' "$$orchestrator" )" = 1; \
	timeout 10s test "$$( timeout 10s grep -Fc 'UsrRollbackCandidatePreserveAuthority::capture(' "$$orchestrator" )" = 1; \
	timeout 10s test "$$( timeout 10s grep -Fc 'UsrRollbackFreshDbInvalidationRouteAuthority::capture(' "$$orchestrator" )" = 1; \
	timeout 10s test "$$( timeout 10s grep -Fc 'UsrRollbackFreshDbInvalidationAuthority::capture(' "$$orchestrator" )" = 1; \
	timeout 10s test "$$( timeout 10s grep -Fc 'UsrRollbackCompleteRouteAuthority::capture(' "$$orchestrator" )" = 1; \
	timeout 10s test "$$( timeout 10s grep -Fc 'dispatch_usr_rollback_candidate_preserve_and_reopen(journal, record, ready)?' "$$orchestrator" )" = 1; \
	timeout 10s test "$$( timeout 10s grep -Fc 'persist_usr_rollback_fresh_db_invalidation_route_and_reopen(journal, authority)?' "$$orchestrator" )" = 1; \
	timeout 10s test "$$( timeout 10s grep -Fc 'dispatch_usr_rollback_fresh_db_invalidation_and_reopen(journal, ready)?' "$$orchestrator" )" = 1; \
	timeout 10s test "$$( timeout 10s grep -Fc 'persist_usr_rollback_complete_route_and_reopen(journal, authority)?' "$$orchestrator" )" = 1; \
	timeout 10s test "$$( timeout 10s grep -Fc 'UsrRollbackCandidatePreserveEffectSeal::new();' "$$candidate_leaf" )" = 1; \
	timeout 10s test "$$( timeout 10s grep -Fc 'UsrRollbackCandidatePreserveDurabilitySeal::new();' "$$candidate_leaf" )" = 1; \
	timeout 10s test "$$( timeout 10s grep -Fc 'UsrRollbackFreshDbInvalidationEffectSeal::new();' "$$fresh_leaf" )" = 1; \
	timeout 10s test "$$( timeout 10s grep -Fc '.into_effect_selection(&effect_seal, &journal)?' "$$candidate_leaf" )" = 1; \
	timeout 10s test "$$( timeout 10s grep -Fc '.reconcile(&effect_seal, &journal)?' "$$candidate_leaf" )" = 3; \
	timeout 10s test "$$( timeout 10s grep -Fc 'complete_post_move_durability(&durability_seal, &journal)?' "$$candidate_leaf" )" = 2; \
	timeout 10s test "$$( timeout 10s grep -Fc 'persist_usr_rollback_candidate_preserve_and_reopen(journal, durable)' "$$candidate_leaf" )" = 1; \
	timeout 10s test "$$( timeout 10s grep -Fc 'persist_usr_rollback_fresh_db_invalidation_and_reopen(journal, authority)' "$$fresh_leaf" )" = 1; \
	production_single_ref() { \
		needle="$$1"; \
		expected_file="$$2"; \
		timeout 10s rg -n -F "$$needle" crates/forge/src/client --glob '*.rs' --glob '!**/tests/**' --glob '!**/tests.rs' --glob '!**/*_tests.rs' --glob '!**/*_tests/**' > "$$inventory"; \
		timeout 10s test "$$( timeout 10s wc -l < "$$inventory" )" = 1; \
		timeout 10s test "$$( timeout 10s cut -d: -f1 "$$inventory" )" = "$$expected_file"; \
	}; \
	for symbol in UsrRollbackCandidatePreserveAuthority UsrRollbackFreshDbInvalidationRouteAuthority UsrRollbackFreshDbInvalidationAuthority UsrRollbackCompleteRouteAuthority; do \
		production_single_ref "$$symbol::capture(" "$$orchestrator"; \
	done; \
	for seal in UsrRollbackCandidatePreserveSeal UsrRollbackFreshDbInvalidationRouteSeal UsrRollbackFreshDbInvalidationSeal UsrRollbackCompleteRouteSeal; do \
		production_single_ref "$$seal::new();" "$$orchestrator"; \
	done; \
	production_single_ref 'UsrRollbackCandidatePreserveEffectSeal::new();' "$$candidate_leaf"; \
	production_single_ref 'UsrRollbackCandidatePreserveDurabilitySeal::new();' "$$candidate_leaf"; \
	production_single_ref 'UsrRollbackFreshDbInvalidationEffectSeal::new();' "$$fresh_leaf"; \
	production_single_ref 'persist_usr_rollback_candidate_preserve_and_reopen(journal, durable)' "$$candidate_leaf"; \
	production_single_ref 'persist_usr_rollback_fresh_db_invalidation_route_and_reopen(journal, authority)?' "$$orchestrator"; \
	production_single_ref 'persist_usr_rollback_fresh_db_invalidation_and_reopen(journal, authority)' "$$fresh_leaf"; \
	production_single_ref 'persist_usr_rollback_complete_route_and_reopen(journal, authority)?' "$$orchestrator"; \
	production_single_ref 'dispatch_usr_rollback_candidate_preserve_and_reopen(journal, record, ready)?' "$$orchestrator"; \
	production_single_ref 'dispatch_usr_rollback_fresh_db_invalidation_and_reopen(journal, ready)?' "$$orchestrator"; \
	production_single_ref '.into_effect_selection(&effect_seal, &journal)?' "$$candidate_leaf"; \
	production_single_ref 'UsrRollbackFreshDbInvalidationReady::Apply(authority) => match authority.reconcile(&effect_seal, &journal)? {' "$$fresh_leaf"; \
	production_single_ref 'UsrRollbackFreshDbInvalidationReady::Finish(authority) => authority.reconcile(&effect_seal, &journal)?,' "$$fresh_leaf"; \
	timeout 10s rg -n -F 'complete_post_move_durability(&durability_seal, &journal)?' crates/forge/src/client --glob '*.rs' --glob '!**/tests/**' --glob '!**/tests.rs' --glob '!**/*_tests.rs' --glob '!**/*_tests/**' > "$$inventory"; \
	timeout 10s test "$$( timeout 10s wc -l < "$$inventory" )" = 2; \
	timeout 10s test "$$( timeout 10s grep -Fc "$$candidate_leaf:" "$$inventory" )" = 2; \
	timeout 10s rg -n -F 'return_exact_unchanged_source(journal, source_record)' crates/forge/src/client --glob '*.rs' --glob '!**/tests/**' --glob '!**/tests.rs' --glob '!**/*_tests.rs' --glob '!**/*_tests/**' > "$$inventory"; \
	timeout 10s test "$$( timeout 10s wc -l < "$$inventory" )" = 2; \
	timeout 10s test "$$( timeout 10s grep -Fc "$$candidate_leaf:" "$$inventory" )" = 2; \
	effect_line="$$( timeout 10s grep -nF 'let effect_seal = UsrRollbackCandidatePreserveEffectSeal::new();' "$$candidate_leaf" | timeout 10s cut -d: -f1 )"; \
	selection_line="$$( timeout 10s grep -nF 'authority.into_effect_selection(&effect_seal, &journal)?' "$$candidate_leaf" | timeout 10s cut -d: -f1 )"; \
	preparation_return_line="$$( timeout 10s grep -nF 'return return_exact_unchanged_source(journal, source_record);' "$$candidate_leaf" | timeout 10s tail -n 1 | timeout 10s cut -d: -f1 )"; \
	durability_line="$$( timeout 10s grep -nF 'let durability_seal = UsrRollbackCandidatePreserveDurabilitySeal::new();' "$$candidate_leaf" | timeout 10s cut -d: -f1 )"; \
	timeout 10s test "$$effect_line" -lt "$$selection_line"; \
	timeout 10s test "$$selection_line" -lt "$$preparation_return_line"; \
	timeout 10s test "$$preparation_return_line" -lt "$$durability_line"; \
	reverse_line="$$( timeout 10s grep -nF 'super::startup_recovery::dispatch_usr_rollback_reverse_and_reopen(journal, ready)?' "$$gate" | timeout 10s cut -d: -f1 )"; \
	suffix_line="$$( timeout 10s grep -nF 'let (journal, record) = match usr_rollback_new_state::dispatch(' "$$gate" | timeout 10s cut -d: -f1 )"; \
	diagnostic_line="$$( timeout 10s grep -nF 'let pending = startup_reconciliation::PendingSystemTransition::inspect(' "$$gate" | timeout 10s tail -n 1 | timeout 10s cut -d: -f1 )"; \
	timeout 10s test "$$reverse_line" -lt "$$suffix_line"; \
	timeout 10s test "$$suffix_line" -lt "$$diagnostic_line"; \
	timeout 10s test "$$( timeout 10s grep -Fc 'usr_rollback_new_state::dispatch(' "$$gate" )" = 1; \
	timeout 10s test "$$( timeout 10s grep -Fc 'return Err(Error::RecoveryPending(pending));' "$$gate" )" -ge 4; \
	timeout 10s sed -E 's,//.*$$,,' "$$orchestrator" "$$candidate_leaf" "$$fresh_leaf" > "$$production"; \
	if timeout 10s rg -n '^[[:space:]]*(loop|while|for)[[:space:]]|=[[:space:]]*(loop|while|for)[[:space:]]|retry' "$$production"; then exit 1; else status="$$?"; timeout 10s test "$$status" = 1; fi; \
	if timeout 10s rg -n 'std::fs|(^|[^_[:alnum:]])fs::|diesel::|SqliteConnection|sql_query|\.execute[[:space:]]*\(|\.transaction[[:space:]]*\(|\.advance[[:space:]]*\(|journal\.delete|\.delete[[:space:]]*\(|remove_exact_fresh_transition|renameat|rename[[:space:]]*\(|unlink|mkdir|create_dir|set_permissions|chmod|sync_(all|data)|run_transaction_triggers|run_system_triggers|root_links|archive_previous|cleanup' "$$production"; then exit 1; else status="$$?"; timeout 10s test "$$status" = 1; fi; \
	timeout 10s grep -Fq 'for epoch in Epoch::ALL {' "$$tests/sequence.rs" "$$tests/matrix.rs"; \
	timeout 10s grep -Fq 'for source in CandidateSource::ALL {' "$$tests/sequence.rs" "$$tests/matrix.rs"; \
	timeout 10s grep -Fq 'for prefix in TargetPrefix::ALL {' "$$tests/sequence.rs"; \
	timeout 10s grep -Fq 'for candidate_outcome in CandidateOutcome::ALL {' "$$tests/matrix.rs"; \
	timeout 10s grep -Fq 'for fresh_outcome in FreshOutcome::ALL {' "$$tests/matrix.rs"; \
	timeout 10s grep -Fq 'FreshRowLayout::Present' "$$tests/support.rs"; \
	timeout 10s grep -Fq 'FreshRowLayout::JointlyAbsent' "$$tests/support.rs"; \
	timeout 10s grep -Fq 'OperationKind::Archived, OperationKind::ActiveReblit' "$$tests/exclusions.rs"; \
	timeout 10s grep -Fq 'arm_next_temporary_sync_fault' "$$tests/failures.rs"; \
	timeout 10s grep -Fq 'arm_next_update_first_directory_sync_fault' "$$tests/failures.rs"; \
	timeout 10s grep -Fq 'arm_exact_fresh_transition_removal_fault' "$$tests/failures.rs"; \
	timeout 10s grep -Fq 'arm_between_usr_rollback_fresh_db_invalidation_route_database_captures' "$$tests/failures.rs"; \
	for seam in arm_new_state_target_create_fault arm_before_new_state_target_create_reconciliation_capture arm_new_state_target_normalize_fault arm_before_new_state_target_normalize_reconciliation_capture arm_new_state_target_normalize_durability_fault arm_before_usr_rollback_new_state_candidate_preserve_effect_final_pre_capture arm_new_state_candidate_preserve_target_durability_fault arm_before_new_state_candidate_preserve_post_move_candidate_sync arm_new_state_candidate_preserve_post_move_durability_fault; do timeout 10s grep -Fq "$$seam" "$$tests/preparation_failures.rs"; done; \
	timeout 10s grep -Fqx 'const JOURNAL_FAULTS: [JournalFault; 5] = [' "$$tests/storage_faults.rs"; \
	for fault in arm_next_temporary_sync_fault arm_next_update_exchange_fault arm_next_update_first_directory_sync_fault arm_next_displaced_unlink_fault arm_next_update_final_directory_sync_fault; do timeout 10s grep -Fq "$$fault" "$$tests/storage_faults.rs"; done; \
	timeout 10s test "$$( timeout 10s grep -Fc 'exercise_' "$$tests/storage_faults.rs" )" -ge 8; \
	for file in "$$gate" "$$orchestrator" "$$recovery_root" "$$candidate_leaf" "$$fresh_leaf" "$$tests/mod.rs" "$$tests/support.rs" "$$tests/sequence.rs" "$$tests/matrix.rs" "$$tests/failures.rs" "$$tests/preparation_failures.rs" "$$tests/storage_faults.rs" "$$tests/exclusions.rs" misc/make/startup-rollback-new-state-dispatch-tests.mk Makefile; do \
		timeout 10s test "$$( timeout 10s wc -l < "$$file" )" -le 1000; \
	done; \
	timeout 1200s $(CARGO) test -p forge --lib "$$prefix" -- --test-threads=1
