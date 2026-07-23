.PHONY: forge-startup-usr-rollback-new-state-dispatch-test

forge-startup-usr-rollback-new-state-dispatch-test:
	@set -euo pipefail; \
	timeout 10s mkdir -p "$(TOP_DIR)/target"; \
	listed="$$( timeout 10s mktemp "$(TOP_DIR)/target/new-state-suffix-list.XXXXXXXXXXXX" )"; \
	production="$$( timeout 10s mktemp "$(TOP_DIR)/target/new-state-suffix-production.XXXXXXXXXXXX" )"; \
	inventory="$$( timeout 10s mktemp "$(TOP_DIR)/target/new-state-suffix-inventory.XXXXXXXXXXXX" )"; \
	trap 'timeout 10s rm -f "$$listed" "$$production" "$$inventory"' EXIT; \
	timeout 300s $(CARGO) test -p forge --lib -- --list | timeout 300s tee "$$listed" >/dev/null; \
	timeout 10s grep -q . "$$listed"; \
	prefix='client::startup_gate::usr_rollback_new_state::tests::'; \
	timeout 10s test "$$( timeout 10s grep -c "^$$prefix.*: test$$" "$$listed" )" = 39; \
	for name in \
		candidate_move_process_kill::startup_new_state_candidate_move_process_kill_recovers_without_second_move \
		exclusions::startup_new_state_suffix_leaves_archived_candidate_preservation_zero_effect \
		exclusions::startup_new_state_suffix_defers_inexact_rollback_complete_with_zero_suffix_effects \
		failures::startup_new_state_suffix_candidate_effect_failure_retries_once_on_a_fresh_entry \
		failures::startup_new_state_suffix_database_effect_failures_restart_from_exact_present_or_joint_absence \
		failures::startup_new_state_suffix_source_durable_storage_failures_repeat_no_candidate_or_database_effect \
		failures::startup_new_state_suffix_successor_durable_storage_failure_never_redispatches_the_completed_phase \
		failures::startup_new_state_suffix_reloads_an_overwritten_durable_successor_before_dispatch \
		failures::startup_new_state_suffix_evidence_change_defers_before_route_or_later_effects \
		finalization::startup_new_state_suffix_terminal_handoff_retains_the_same_journal_lock_through_clean_startup \
		finalization::startup_new_state_suffix_reaudits_database_after_finalization_before_clean_admission \
		finalization::startup_new_state_suffix_finalization_converges_into_the_shared_prune_residue_audit \
		finalization::startup_new_state_suffix_rejects_terminal_record_recreated_during_clean_handoff \
		finalization::startup_new_state_suffix_rejects_mutable_namespace_substitution_after_terminal_finalization \
		finalization_restart::startup_new_state_root_links_finalization_restarts_from_observed_absence_with_fresh_handles \
		finalization_restart::startup_new_state_root_links_finalization_restarts_from_retained_terminal_source_with_fresh_handles \
		fresh_db_invalidation_process_kill::startup_root_links_new_state_fresh_db_invalidation_process_kills_recover_exactly \
		matrix::startup_new_state_suffix_routes_every_exact_candidate_preserved_matrix_without_later_effects \
		matrix::startup_new_state_suffix_invalidates_present_or_accepts_joint_absence_for_every_exact_matrix \
		matrix::startup_new_state_suffix_completes_every_exact_invalidated_outcome_without_repeating_effects \
		matrix::startup_new_state_suffix_finalizes_every_exact_terminal_matrix_without_later_effects \
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
		root_links_terminal_process_kill::startup_new_state_root_links_terminal_delete_process_kills_restart_cleanly \
		sequence::startup_new_state_suffix_consumes_exactly_one_checkpoint_per_entry_for_every_target_prefix \
		sequence::startup_new_state_suffix_runs_the_exact_multi_entry_sequence_without_same_entry_fallthrough \
		sequence::startup_new_state_suffix_reacquires_fresh_installation_database_journal_and_reservation_handles \
		storage_faults::startup_new_state_suffix_all_five_journal_faults_reenter_each_of_four_persistence_boundaries_exactly \
		storage_faults::startup_new_state_suffix_terminal_unlink_fault_restarts_from_exact_source_without_duplicate_effects \
		storage_faults::startup_new_state_suffix_terminal_directory_sync_fault_restarts_from_exact_absence_without_duplicate_effects \
		terminal_delete_process_kill::startup_new_state_suffix_terminal_delete_process_kills_restart_cleanly; do \
		timeout 10s grep -Fqx "$$prefix$$name: test" "$$listed"; \
	done; \
	gate=crates/forge/src/client/startup_gate.rs; \
	orchestrator=crates/forge/src/client/startup_gate/usr_rollback_new_state.rs; \
	archived_orchestrator=crates/forge/src/client/startup_gate/usr_rollback_activate_archived.rs; \
	active_orchestrator=crates/forge/src/client/startup_gate/usr_rollback_active_reblit.rs; \
	recovery_root=crates/forge/src/client/startup_recovery.rs; \
	candidate_leaf=crates/forge/src/client/startup_recovery/usr_rollback_candidate_preserve_dispatch.rs; \
	fresh_leaf=crates/forge/src/client/startup_recovery/usr_rollback_fresh_db_invalidation_dispatch.rs; \
	finalization_leaf=crates/forge/src/client/startup_recovery/usr_rollback_finalization.rs; \
	tests=crates/forge/src/client/startup_gate/usr_rollback_new_state/tests; \
	process_kill="$$tests/terminal_delete_process_kill.rs"; \
	candidate_process_kill="$$tests/candidate_move_process_kill.rs"; \
	candidate_process_harness="$$tests/candidate_move_process_harness.rs"; \
	candidate_boundaries="$$tests/candidate_process_kill_boundaries.rs"; \
	invalidation_process_kill="$$tests/fresh_db_invalidation_process_kill.rs"; \
	invalidation_process_harness="$$tests/fresh_db_invalidation_process_harness.rs"; \
	invalidation_process_evidence="$$tests/fresh_db_invalidation_process_evidence.rs"; \
	invalidation_process_boundaries="$$tests/fresh_db_invalidation_process_boundaries.rs"; \
	exact_fresh_removal=crates/forge/src/db/state/exact_fresh_transition_removal.rs; \
	timeout 10s test "$$( timeout 10s rg -l '^pub\(in crate::client\) struct UsrRollbackCandidatePreserveSeal \{' crates/forge/src/client --glob '*.rs' )" = "$$gate"; \
	timeout 10s grep -Fqx 'pub(in crate::client) struct UsrRollbackCandidatePreserveSeal {' "$$gate"; \
	timeout 10s awk '$$0 == "pub(in crate::client) struct UsrRollbackCandidatePreserveSeal {" { open = 1; next } open && $$0 == "    _private: ()," { field = 1; next } open && $$0 == "}" { closed = 1; open = 0 } END { exit !(field && closed) }' "$$gate"; \
	candidate_seal_impl="$$( timeout 10s sed -n '/^impl UsrRollbackCandidatePreserveSeal {/,/^}/p' "$$gate" )"; \
	timeout 10s test "$$( timeout 10s grep -Fc '    fn new() -> Self {' <<<"$$candidate_seal_impl" )" = 1; \
	timeout 10s test "$$( timeout 10s grep -Fc '    pub(in crate::client) fn new_for_test() -> Self {' <<<"$$candidate_seal_impl" )" = 1; \
	timeout 10s awk '$$0 == "    #[cfg(test)]" { gated = 1; next } gated && $$0 == "    pub(in crate::client) fn new_for_test() -> Self {" { found++; gated = 0; next } gated { gated = 0 } END { exit found != 1 }' <<<"$$candidate_seal_impl"; \
	for seal in UsrRollbackFreshDbInvalidationRouteSeal UsrRollbackFreshDbInvalidationSeal UsrRollbackCompleteRouteSeal UsrRollbackFinalizationSeal; do \
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
	timeout 10s grep -Fqx 'mod candidate_move_process_kill;' "$$tests/mod.rs"; \
	timeout 10s grep -Fqx 'mod candidate_move_process_harness;' "$$tests/mod.rs"; \
	timeout 10s grep -Fqx 'mod candidate_process_kill_boundaries;' "$$tests/mod.rs"; \
	timeout 10s grep -Fqx 'mod exclusions;' "$$tests/mod.rs"; \
	timeout 10s grep -Fqx 'mod failures;' "$$tests/mod.rs"; \
	timeout 10s grep -Fqx 'mod finalization;' "$$tests/mod.rs"; \
	timeout 10s grep -Fqx 'mod finalization_restart;' "$$tests/mod.rs"; \
	timeout 10s grep -Fqx 'mod matrix;' "$$tests/mod.rs"; \
	timeout 10s grep -Fqx 'mod preparation_failures;' "$$tests/mod.rs"; \
	timeout 10s grep -Fqx 'mod sequence;' "$$tests/mod.rs"; \
	timeout 10s grep -Fqx 'mod storage_faults;' "$$tests/mod.rs"; \
	timeout 10s grep -Fqx 'mod support;' "$$tests/mod.rs"; \
	timeout 10s grep -Fqx 'mod terminal_delete_process_kill;' "$$tests/mod.rs"; \
	timeout 10s grep -Fqx 'fn enter_result(system: &MutableSystemCapabilities) -> Result<CleanSystemStartup, startup_gate::Error> {' "$$tests/support.rs"; \
	timeout 10s test "$$( timeout 10s grep -Fc 'CleanSystemStartup::enter(system, &reservation)' "$$tests/support.rs" )" = 1; \
	if timeout 10s rg -n 'CleanSystemStartup::enter\(installation, database,' "$$tests/support.rs"; then exit 1; else status="$$?"; timeout 10s test "$$status" = 1; fi; \
	timeout 10s test "$$( timeout 10s grep -Fc 'ActiveStateReservation::acquire().unwrap();' "$$tests/support.rs" )" = 1; \
	if timeout 10s rg -n 'new_for_test|usr_rollback_new_state::dispatch|dispatch_usr_rollback_.*_and_reopen|persist_usr_rollback_.*_and_reopen' "$$tests"; then exit 1; else status="$$?"; timeout 10s test "$$status" = 1; fi; \
	timeout 10s grep -Fqx 'pub(super) fn dispatch<'\''reservation>(' "$$orchestrator"; \
	timeout 10s test "$$( timeout 10s grep -Fc 'if record.operation != Operation::NewState {' "$$orchestrator" )" = 1; \
	timeout 10s test "$$( timeout 10s grep -Fc '    match record.phase {' "$$orchestrator" )" = 1; \
	for phase in CandidatePreserveIntent CandidatePreserved FreshDbInvalidationIntent FreshDbInvalidated RollbackComplete; do \
		timeout 10s test "$$( timeout 10s grep -Fc "        Phase::$$phase => {" "$$orchestrator" )" = 1; \
	done; \
	timeout 10s test "$$( timeout 10s grep -Fc 'UsrRollbackCandidatePreserveSeal::new();' "$$orchestrator" )" = 1; \
	timeout 10s test "$$( timeout 10s grep -Fc 'UsrRollbackFreshDbInvalidationRouteSeal::new();' "$$orchestrator" )" = 1; \
	timeout 10s test "$$( timeout 10s grep -Fc 'UsrRollbackFreshDbInvalidationSeal::new();' "$$orchestrator" )" = 1; \
	timeout 10s test "$$( timeout 10s grep -Fc 'UsrRollbackCompleteRouteSeal::new();' "$$orchestrator" )" = 1; \
	timeout 10s test "$$( timeout 10s grep -Fc 'UsrRollbackFinalizationSeal::new();' "$$orchestrator" )" = 1; \
	timeout 10s test "$$( timeout 10s grep -Fc 'UsrRollbackCandidatePreserveAuthority::capture(' "$$orchestrator" )" = 1; \
	timeout 10s test "$$( timeout 10s grep -Fc 'UsrRollbackFreshDbInvalidationRouteAuthority::capture(' "$$orchestrator" )" = 1; \
	timeout 10s test "$$( timeout 10s grep -Fc 'UsrRollbackFreshDbInvalidationAuthority::capture(' "$$orchestrator" )" = 1; \
	timeout 10s test "$$( timeout 10s grep -Fc 'UsrRollbackCompleteRouteAuthority::capture(' "$$orchestrator" )" = 1; \
	timeout 10s test "$$( timeout 10s grep -Fc 'UsrRollbackFinalizationAuthority::capture(' "$$orchestrator" )" = 1; \
	timeout 10s test "$$( timeout 10s grep -Fc 'dispatch_usr_rollback_candidate_preserve_and_reopen(journal, record, ready)?' "$$orchestrator" )" = 1; \
	timeout 10s test "$$( timeout 10s grep -Fc 'persist_usr_rollback_fresh_db_invalidation_route_and_reopen(journal, authority)?' "$$orchestrator" )" = 1; \
	timeout 10s test "$$( timeout 10s grep -Fc 'dispatch_usr_rollback_fresh_db_invalidation_and_reopen(journal, ready)?' "$$orchestrator" )" = 1; \
	timeout 10s test "$$( timeout 10s grep -Fc 'persist_usr_rollback_complete_route_and_reopen(journal, authority)?' "$$orchestrator" )" = 1; \
	timeout 10s test "$$( timeout 10s grep -Fc 'finalize_usr_rollback(journal, authority)?' "$$orchestrator" )" = 1; \
	timeout 10s test "$$( timeout 10s grep -Fc 'UsrRollbackCandidatePreserveEffectSeal::new();' "$$candidate_leaf" )" = 1; \
	timeout 10s test "$$( timeout 10s grep -Fc 'UsrRollbackCandidatePreserveDurabilitySeal::new();' "$$candidate_leaf" )" = 1; \
	timeout 10s test "$$( timeout 10s grep -Fc 'UsrRollbackFreshDbInvalidationEffectSeal::new();' "$$fresh_leaf" )" = 1; \
	timeout 10s test "$$( timeout 10s grep -Fc '.into_effect_selection(&effect_seal, &journal)?' "$$candidate_leaf" )" = 1; \
	timeout 10s test "$$( timeout 10s grep -Fc '.reconcile(&effect_seal, &journal)?' "$$candidate_leaf" )" = 5; \
	timeout 10s test "$$( timeout 10s grep -Fc 'complete_post_move_durability(&durability_seal, &journal)?' "$$candidate_leaf" )" = 4; \
	timeout 10s test "$$( timeout 10s grep -Fc 'persist_usr_rollback_candidate_preserve_and_reopen(journal, durable)' "$$candidate_leaf" )" = 1; \
	timeout 10s test "$$( timeout 10s grep -Fc 'persist_usr_rollback_fresh_db_invalidation_and_reopen(journal, authority)' "$$fresh_leaf" )" = 1; \
	production_single_ref() { \
		needle="$$1"; \
		expected_file="$$2"; \
		timeout 10s rg -n -F "$$needle" crates/forge/src/client --glob '*.rs' --glob '!**/tests/**' --glob '!**/tests.rs' --glob '!**/*_tests.rs' --glob '!**/*_tests/**' > "$$inventory"; \
		timeout 10s test "$$( timeout 10s wc -l < "$$inventory" )" = 1; \
		timeout 10s test "$$( timeout 10s cut -d: -f1 "$$inventory" )" = "$$expected_file"; \
	}; \
	for symbol in UsrRollbackFreshDbInvalidationRouteAuthority UsrRollbackFreshDbInvalidationAuthority UsrRollbackCompleteRouteAuthority UsrRollbackFinalizationAuthority; do \
		production_single_ref "$$symbol::capture(" "$$orchestrator"; \
	done; \
	for seal in UsrRollbackFreshDbInvalidationRouteSeal UsrRollbackFreshDbInvalidationSeal UsrRollbackCompleteRouteSeal UsrRollbackFinalizationSeal; do \
		production_single_ref "$$seal::new();" "$$orchestrator"; \
	done; \
	timeout 10s rg -n -F 'UsrRollbackCandidatePreserveAuthority::capture(' crates/forge/src/client --glob '*.rs' --glob '!**/tests/**' --glob '!**/tests.rs' --glob '!**/*_tests.rs' --glob '!**/*_tests/**' > "$$inventory"; \
	timeout 10s test "$$( timeout 10s wc -l < "$$inventory" )" = 3; \
	timeout 10s test "$$( timeout 10s grep -Fc "$$orchestrator:" "$$inventory" )" = 1; \
	timeout 10s test "$$( timeout 10s grep -Fc "$$archived_orchestrator:" "$$inventory" )" = 1; \
	timeout 10s test "$$( timeout 10s grep -Fc "$$active_orchestrator:" "$$inventory" )" = 1; \
	timeout 10s rg -n -F 'UsrRollbackCandidatePreserveSeal::new();' crates/forge/src/client --glob '*.rs' --glob '!**/tests/**' --glob '!**/tests.rs' --glob '!**/*_tests.rs' --glob '!**/*_tests/**' > "$$inventory"; \
	timeout 10s test "$$( timeout 10s wc -l < "$$inventory" )" = 3; \
	timeout 10s test "$$( timeout 10s grep -Fc "$$orchestrator:" "$$inventory" )" = 1; \
	timeout 10s test "$$( timeout 10s grep -Fc "$$archived_orchestrator:" "$$inventory" )" = 1; \
	timeout 10s test "$$( timeout 10s grep -Fc "$$active_orchestrator:" "$$inventory" )" = 1; \
	production_single_ref 'UsrRollbackCandidatePreserveEffectSeal::new();' "$$candidate_leaf"; \
	production_single_ref 'UsrRollbackCandidatePreserveDurabilitySeal::new();' "$$candidate_leaf"; \
	production_single_ref 'UsrRollbackFreshDbInvalidationEffectSeal::new();' "$$fresh_leaf"; \
	production_single_ref 'persist_usr_rollback_candidate_preserve_and_reopen(journal, durable)' "$$candidate_leaf"; \
	production_single_ref 'persist_usr_rollback_fresh_db_invalidation_route_and_reopen(journal, authority)?' "$$orchestrator"; \
	production_single_ref 'persist_usr_rollback_fresh_db_invalidation_and_reopen(journal, authority)' "$$fresh_leaf"; \
	production_single_ref 'persist_usr_rollback_complete_route_and_reopen(journal, authority)?' "$$orchestrator"; \
	production_single_ref 'finalize_usr_rollback(journal, authority)?' "$$orchestrator"; \
	timeout 10s rg -n -F 'dispatch_usr_rollback_candidate_preserve_and_reopen(journal, record, ready)?' crates/forge/src/client --glob '*.rs' --glob '!**/tests/**' --glob '!**/tests.rs' --glob '!**/*_tests.rs' --glob '!**/*_tests/**' > "$$inventory"; \
	timeout 10s test "$$( timeout 10s wc -l < "$$inventory" )" = 3; \
	timeout 10s test "$$( timeout 10s grep -Fc "$$orchestrator:" "$$inventory" )" = 1; \
	timeout 10s test "$$( timeout 10s grep -Fc "$$archived_orchestrator:" "$$inventory" )" = 1; \
	timeout 10s test "$$( timeout 10s grep -Fc "$$active_orchestrator:" "$$inventory" )" = 1; \
	production_single_ref 'dispatch_usr_rollback_fresh_db_invalidation_and_reopen(journal, ready)?' "$$orchestrator"; \
	production_single_ref '.into_effect_selection(&effect_seal, &journal)?' "$$candidate_leaf"; \
	production_single_ref 'UsrRollbackFreshDbInvalidationReady::Apply(authority) => match authority.reconcile(&effect_seal, &journal)? {' "$$fresh_leaf"; \
	production_single_ref 'UsrRollbackFreshDbInvalidationReady::Finish(authority) => authority.reconcile(&effect_seal, &journal)?,' "$$fresh_leaf"; \
	timeout 10s rg -n -F 'complete_post_move_durability(&durability_seal, &journal)?' crates/forge/src/client --glob '*.rs' --glob '!**/tests/**' --glob '!**/tests.rs' --glob '!**/*_tests.rs' --glob '!**/*_tests/**' > "$$inventory"; \
	timeout 10s test "$$( timeout 10s wc -l < "$$inventory" )" = 4; \
	timeout 10s test "$$( timeout 10s grep -Fc "$$candidate_leaf:" "$$inventory" )" = 4; \
	timeout 10s rg -n -F 'return_exact_unchanged_source(journal, source_record, authority)' crates/forge/src/client --glob '*.rs' --glob '!**/tests/**' --glob '!**/tests.rs' --glob '!**/*_tests.rs' --glob '!**/*_tests/**' > "$$inventory"; \
	timeout 10s test "$$( timeout 10s wc -l < "$$inventory" )" = 2; \
	timeout 10s test "$$( timeout 10s grep -Fc "$$candidate_leaf:" "$$inventory" )" = 2; \
	timeout 10s grep -Fq "fn return_exact_unchanged_source<'reservation>(" "$$candidate_leaf"; \
	timeout 10s grep -Fq "    authority: UsrRollbackCandidatePreserveRestartAuthority<'reservation>," "$$candidate_leaf"; \
	timeout 10s test "$$( timeout 10s grep -Fc 'authority.into_exact_source_record(&journal)?' "$$candidate_leaf" )" = 1; \
	effect_line="$$( timeout 10s grep -nF 'let effect_seal = UsrRollbackCandidatePreserveEffectSeal::new();' "$$candidate_leaf" | timeout 10s cut -d: -f1 )"; \
	selection_line="$$( timeout 10s grep -nF 'authority.into_effect_selection(&effect_seal, &journal)?' "$$candidate_leaf" | timeout 10s cut -d: -f1 )"; \
	preparation_return_line="$$( timeout 10s grep -nF 'return return_exact_unchanged_source(journal, source_record, authority);' "$$candidate_leaf" | timeout 10s tail -n 1 | timeout 10s cut -d: -f1 )"; \
	durability_line="$$( timeout 10s grep -nF 'let durability_seal = UsrRollbackCandidatePreserveDurabilitySeal::new();' "$$candidate_leaf" | timeout 10s cut -d: -f1 )"; \
	timeout 10s test "$$effect_line" -lt "$$selection_line"; \
	timeout 10s test "$$selection_line" -lt "$$preparation_return_line"; \
	timeout 10s test "$$preparation_return_line" -lt "$$durability_line"; \
	reverse_line="$$( timeout 10s grep -nF 'super::startup_recovery::dispatch_usr_rollback_reverse_and_reopen(journal, ready)?' "$$gate" | timeout 10s cut -d: -f1 )"; \
	active_line="$$( timeout 10s grep -nF 'let (journal, record) = match usr_rollback_active_reblit::dispatch(' "$$gate" | timeout 10s cut -d: -f1 )"; \
	suffix_line="$$( timeout 10s grep -nF 'let (journal, record) = match usr_rollback_new_state::dispatch(' "$$gate" | timeout 10s cut -d: -f1 )"; \
	diagnostic_line="$$( timeout 10s grep -nF 'let pending = startup_reconciliation::PendingSystemTransition::inspect(' "$$gate" | timeout 10s tail -n 1 | timeout 10s cut -d: -f1 )"; \
	timeout 10s test "$$reverse_line" -lt "$$active_line"; \
	timeout 10s test "$$active_line" -lt "$$suffix_line"; \
	timeout 10s test "$$suffix_line" -lt "$$diagnostic_line"; \
	timeout 10s test "$$( timeout 10s grep -Fc 'usr_rollback_new_state::dispatch(' "$$gate" )" = 1; \
	timeout 10s test "$$( timeout 10s grep -Fc 'usr_rollback_new_state::Dispatch::Finalized { journal } => {' "$$gate" )" = 1; \
	new_state_finalized_arm="$$( timeout 10s sed -n '/usr_rollback_new_state::Dispatch::Finalized { journal } => {/,/^                }/p' "$$gate" )"; \
	timeout 10s grep -Fq 'return Self::admit_clean_after_terminal_finalization(installation, state_db, journal);' <<<"$$new_state_finalized_arm"; \
	terminal_handoff="$$( timeout 10s sed -n '/^    fn admit_clean_after_terminal_finalization(/,/^    }/p' "$$gate" )"; \
	timeout 10s grep -Fq 'after_usr_rollback_finalization_before_clean_audit();' <<<"$$terminal_handoff"; \
	timeout 10s grep -Fq 'return Self::admit_clean(installation, state_db, journal, in_flight);' <<<"$$terminal_handoff"; \
	timeout 10s test "$$( timeout 10s grep -Fc 'return Self::admit_clean(installation, state_db, journal, in_flight);' "$$gate" )" = 1; \
	timeout 10s test "$$( timeout 10s grep -Fc 'Self::admit_clean(installation, state_db, journal, in_flight)' "$$gate" )" = 2; \
	timeout 10s test "$$( timeout 10s grep -Fc 'return Self::admit_clean_after_terminal_finalization(installation, state_db, journal);' "$$gate" )" = 3; \
	timeout 10s test "$$( timeout 10s grep -Fc 'authority.journal().load_revalidated_retained_cast(cast)' "$$gate" )" = 1; \
	timeout 10s grep -Fq 'CanonicalTransitionAppearedDuringCleanAdmission {' "$$gate"; \
	timeout 10s grep -Fq 'arm_after_usr_rollback_finalization_before_clean_audit' "$$tests/finalization.rs"; \
	timeout 10s grep -Fq 'write_new_private_record(&canonical, &recreated)' "$$tests/finalization.rs"; \
	timeout 10s grep -Fq 'fs::rename(callback_cast, &callback_displaced)' "$$tests/finalization.rs"; \
	residue_audit_line="$$( timeout 10s grep -nF 'let residue = transition_identity::audit_archived_state_prune_residue' "$$gate" | timeout 10s cut -d: -f1 )"; \
	final_absence_line="$$( timeout 10s grep -nF 'authority.journal().load_revalidated_retained_cast(cast)' "$$gate" | timeout 10s cut -d: -f1 )"; \
	clean_admission_line="$$( timeout 10s grep -nF 'Ok(Self { _authority: authority })' "$$gate" | timeout 10s cut -d: -f1 )"; \
	timeout 10s test "$$residue_audit_line" -lt "$$final_absence_line"; \
	timeout 10s test "$$final_absence_line" -lt "$$clean_admission_line"; \
	timeout 10s test "$$( timeout 10s grep -Fc 'return Err(Error::RecoveryPending(pending));' "$$gate" )" -ge 4; \
	timeout 10s sed -E 's,//.*$$,,' "$$orchestrator" "$$active_orchestrator" "$$candidate_leaf" "$$fresh_leaf" > "$$production"; \
	if timeout 10s rg -n '^[[:space:]]*(loop|while|for)[[:space:]]|=[[:space:]]*(loop|while|for)[[:space:]]|retry' "$$production"; then exit 1; else status="$$?"; timeout 10s test "$$status" = 1; fi; \
	if timeout 10s rg -n 'std::fs|(^|[^_[:alnum:]])fs::|diesel::|SqliteConnection|sql_query|\.execute[[:space:]]*\(|\.transaction[[:space:]]*\(|\.advance[[:space:]]*\(|journal\.delete|\.delete[[:space:]]*\(|remove_exact_fresh_transition|renameat|rename[[:space:]]*\(|unlink|mkdir|create_dir|set_permissions|chmod|sync_(all|data)|run_transaction_triggers|run_system_triggers|root_links|archive_previous|cleanup' "$$production"; then exit 1; else status="$$?"; timeout 10s test "$$status" = 1; fi; \
	timeout 10s grep -Fq 'for epoch in Epoch::ALL {' "$$tests/sequence.rs" "$$tests/matrix.rs"; \
	timeout 10s grep -Fq 'for source in CandidateSource::ALL {' "$$tests/sequence.rs" "$$tests/matrix.rs"; \
	timeout 10s grep -Fq 'for source in CandidateSource::THROUGH_ROLLBACK_COMPLETE {' "$$tests/matrix.rs"; \
	timeout 10s grep -Fq 'assert_eq!(cases, 48);' "$$tests/matrix.rs"; \
	finalization_matrix="$$( timeout 10s sed -n '/^fn startup_new_state_suffix_finalizes_every_exact_terminal_matrix_without_later_effects/,/^}/p' "$$tests/matrix.rs" )"; \
	timeout 10s grep -Fq 'for source in CandidateSource::ALL {' <<<"$$finalization_matrix"; \
	if timeout 10s grep -Fq 'THROUGH_ROLLBACK_COMPLETE' <<<"$$finalization_matrix"; then exit 1; else status="$$?"; timeout 10s test "$$status" = 1; fi; \
	timeout 10s grep -Fq 'for prefix in TargetPrefix::ALL {' "$$tests/sequence.rs"; \
	timeout 10s grep -Fq 'for candidate_outcome in CandidateOutcome::ALL {' "$$tests/matrix.rs"; \
	timeout 10s grep -Fq 'for fresh_outcome in FreshOutcome::ALL {' "$$tests/matrix.rs"; \
	timeout 10s grep -Fq 'FreshRowLayout::Present' "$$tests/support.rs"; \
	timeout 10s grep -Fq 'FreshRowLayout::JointlyAbsent' "$$tests/support.rs"; \
	timeout 10s grep -Fq 'let kind = OperationKind::Archived;' "$$tests/exclusions.rs"; \
	timeout 10s grep -Fq 'arm_next_temporary_sync_fault' "$$tests/failures.rs"; \
	timeout 10s grep -Fq 'arm_next_update_first_directory_sync_fault' "$$tests/failures.rs"; \
	timeout 10s grep -Fq 'arm_exact_fresh_transition_removal_fault' "$$tests/failures.rs"; \
	timeout 10s grep -Fq 'arm_between_usr_rollback_fresh_db_invalidation_route_database_captures' "$$tests/failures.rs"; \
	for seam in arm_new_state_target_create_fault arm_before_new_state_target_create_reconciliation_capture arm_new_state_target_normalize_fault arm_before_new_state_target_normalize_reconciliation_capture arm_new_state_target_normalize_durability_fault arm_before_usr_rollback_new_state_candidate_preserve_effect_final_pre_capture arm_new_state_candidate_preserve_target_durability_fault arm_before_new_state_candidate_preserve_post_move_candidate_sync arm_new_state_candidate_preserve_post_move_durability_fault; do timeout 10s grep -Fq "$$seam" "$$tests/preparation_failures.rs"; done; \
	timeout 10s grep -Fqx 'const JOURNAL_FAULTS: [JournalFault; 5] = [' "$$tests/storage_faults.rs"; \
	for fault in arm_next_temporary_sync_fault arm_next_update_exchange_fault arm_next_update_first_directory_sync_fault arm_next_displaced_unlink_fault arm_next_update_final_directory_sync_fault; do timeout 10s grep -Fq "$$fault" "$$tests/storage_faults.rs"; done; \
	for fault in arm_next_delete_canonical_unlink_fault arm_next_delete_directory_sync_fault; do timeout 10s grep -Fq "$$fault" "$$tests/storage_faults.rs"; done; \
	timeout 10s test "$$( timeout 10s grep -Fc 'exercise_' "$$tests/storage_faults.rs" )" -ge 8; \
	timeout 10s grep -Fq 'const ALL: [Self; 3] = [' "$$process_kill"; \
	timeout 10s grep -Fq 'for epoch in Epoch::ALL {' "$$process_kill"; \
	timeout 10s grep -Fq 'for source in CandidateSource::ALL {' "$$process_kill"; \
	timeout 10s grep -Fq 'for boundary in TerminalDeleteKillBoundary::ALL {' "$$process_kill"; \
	timeout 10s grep -Fq '        cases, 12,' "$$process_kill"; \
	timeout 10s grep -Fq 'Command::new(env::current_exe().unwrap())' "$$process_kill"; \
	timeout 10s grep -Fq '.arg(TEST_NAME)' "$$process_kill"; \
	timeout 10s grep -Fq '.arg("--exact")' "$$process_kill"; \
	timeout 10s grep -Fq '.arg("--test-threads=1")' "$$process_kill"; \
	timeout 10s grep -Fq 'Some(nix::libc::SIGKILL)' "$$process_kill"; \
	timeout 10s grep -Fq 'crash_status.signal()' "$$process_kill"; \
	timeout 10s grep -Fq 'arm_before_usr_rollback_finalization_final_revalidation(kill_self)' "$$process_kill"; \
	timeout 10s grep -Fq 'JournalDeleteDurabilityBoundary::CanonicalUnlinked' "$$process_kill"; \
	timeout 10s grep -Fq 'JournalDeleteDurabilityBoundary::DeleteDirectorySynced' "$$process_kill"; \
	timeout 10s grep -Fq 'arm_journal_update_durability_callback' "$$process_kill"; \
	timeout 10s grep -Fq 'snapshot_startup_recovery_namespace' "$$process_kill"; \
	timeout 10s grep -Fq 'struct PublicJournalIdentity {' "$$process_kill"; \
	timeout 10s grep -Fq 'assert_journal_inventory(root, canonical_present);' "$$process_kill"; \
	timeout 10s grep -Fq 'public_before.assert_same_public_anchors(final_public);' "$$process_kill"; \
	timeout 10s grep -Fq 'let terminal_bytes = fs::read(canonical_path(&root)).unwrap();' "$$process_kill"; \
	timeout 10s grep -Fq 'struct DeadlineChild {' "$$process_kill"; \
	timeout 10s grep -Fq 'external process-kill control does not match' "$$process_kill"; \
	timeout 10s test "$$( timeout 10s grep -Fc 'CleanSystemStartup::enter(' "$$process_kill" )" = 2; \
	timeout 10s grep -Fq 'release_invalidation_fixture_handles(fixture)' "$$process_kill"; \
	timeout 10s grep -Fq 'install_persistent_joint_absence_database(&mut fixture)' "$$process_kill"; \
	timeout 10s grep -Fq 'StorageError::AcquireLock' "$$process_kill"; \
	timeout 10s grep -Fq 'assert_eq!(reopened.load().unwrap(), None)' "$$process_kill"; \
	timeout 10s grep -Fq 'not a reboot or' "$$process_kill"; \
	if timeout 10s rg -n 'arm_next_|finalize_usr_rollback|FaultPoint|StorageFault' "$$process_kill"; then exit 1; else status="$$?"; timeout 10s test "$$status" = 1; fi; \
	timeout 10s grep -Fq 'const ALL: [Self; 7] = [' "$$candidate_boundaries"; \
	for boundary in PostMovePreRecapture BeforeCandidateSync BeforeStagingParentSync BeforeTargetParentSync BeforeQuarantineParentSync BeforeFinalPostCapture BeforeDurablePostRevalidation; do timeout 10s grep -Fq "Self::$$boundary" "$$candidate_boundaries"; done; \
	for seam in arm_before_new_state_candidate_preserve_move_reconciliation_capture arm_before_new_state_candidate_preserve_post_move_candidate_sync arm_before_new_state_candidate_preserve_post_move_staging_parent_sync arm_before_new_state_candidate_preserve_post_move_target_parent_sync arm_before_new_state_candidate_preserve_post_move_quarantine_parent_sync arm_before_new_state_candidate_preserve_post_move_final_post_capture arm_before_new_state_candidate_preserve_durable_post_revalidation_capture; do timeout 10s grep -Fq "$$seam" "$$candidate_boundaries"; done; \
	timeout 10s grep -Fq 'for epoch in Epoch::ALL {' "$$candidate_process_kill"; \
	timeout 10s grep -Fq 'for source in CandidateSource::ALL {' "$$candidate_process_kill"; \
	timeout 10s grep -Fq 'for boundary in CandidateProcessKillBoundary::ALL {' "$$candidate_process_kill"; \
	timeout 10s grep -Fq '        cases, 28,' "$$candidate_process_kill"; \
	timeout 10s grep -Fq 'TargetPrefix::Canonical' "$$candidate_process_kill"; \
	timeout 10s grep -Fq 'Command::new(env::current_exe().unwrap())' "$$candidate_process_harness"; \
	timeout 10s grep -Fq '.arg(TEST_NAME)' "$$candidate_process_harness"; \
	timeout 10s grep -Fq '.arg("--exact")' "$$candidate_process_harness"; \
	timeout 10s grep -Fq '.arg("--test-threads=1")' "$$candidate_process_harness"; \
	timeout 10s grep -Fq 'Some(nix::libc::SIGKILL)' "$$candidate_process_kill"; \
	timeout 10s grep -Fq 'crash_status.signal()' "$$candidate_process_kill"; \
	timeout 10s grep -Fq 'new_state_candidate_preserve_move_attempt_count()' "$$candidate_process_kill"; \
	timeout 10s grep -Fq 'arm_before_usr_rollback_new_state_candidate_preserve_effect_final_pre_capture' "$$candidate_process_kill"; \
	timeout 10s grep -Fq 'snapshot_startup_recovery_namespace' "$$candidate_process_kill"; \
	timeout 10s grep -Fq 'struct PublicJournalIdentity {' "$$candidate_process_harness"; \
	timeout 10s grep -Fq 'struct FreshCandidateDatabase {' "$$candidate_process_harness"; \
	timeout 10s grep -Fq 'struct CandidateMoveEvidence {' "$$candidate_process_harness"; \
	timeout 10s grep -Fq 'struct DeadlineChild {' "$$candidate_process_harness"; \
	timeout 10s grep -Fq 'external NewState candidate-move control does not match' "$$candidate_process_harness"; \
	timeout 10s test "$$( timeout 10s grep -Fc 'CleanSystemStartup::enter(' "$$candidate_process_kill" )" = 2; \
	timeout 10s grep -Fq 'release_candidate_handles(fixture)' "$$candidate_process_kill"; \
	timeout 10s grep -Fq 'install_persistent_database(&mut fixture)' "$$candidate_process_kill"; \
	timeout 10s grep -Fq 'RollbackActionOutcome::AlreadySatisfied' "$$candidate_process_harness"; \
	timeout 10s grep -Fq 'not a reboot or' "$$candidate_process_harness"; \
	if timeout 10s rg -n 'arm_next_|finalize_usr_rollback|FaultPoint|StorageFault' "$$candidate_process_kill" "$$candidate_process_harness" "$$candidate_boundaries"; then exit 1; else status="$$?"; timeout 10s test "$$status" = 1; fi; \
	timeout 10s grep -Fq 'pub(super) const DATABASE: [Self; 5] = [' "$$invalidation_process_boundaries"; \
	timeout 10s grep -Fq 'pub(super) const JOURNAL: [Self; 5] = [' "$$invalidation_process_boundaries"; \
	timeout 10s grep -Fq 'pub(super) const ALL: [Self; 10] = [' "$$invalidation_process_boundaries"; \
	for boundary in PreimageValidated ProvenanceDeleted SelectionsDeleted StateRowDeletedBeforeCommit CommitReturnedBeforeReconciliation TemporaryFullySynced CanonicalExchanged UpdateFirstDirectorySynced DisplacedUnlinked UpdateFinalDirectorySynced; do timeout 10s grep -Fq "Self::$$boundary" "$$invalidation_process_boundaries"; done; \
	for boundary in PreimageValidated ProvenanceDeleted SelectionsDeleted StateRowDeletedBeforeCommit CommitReturnedBeforeReconciliation; do timeout 10s grep -Fq "ExactFreshTransitionRemovalBoundary::$$boundary" "$$invalidation_process_boundaries" "$$exact_fresh_removal"; done; \
	for boundary in TemporaryFullySynced CanonicalExchanged UpdateFirstDirectorySynced DisplacedUnlinked UpdateFinalDirectorySynced; do timeout 10s grep -Fq "JournalUpdateDurabilityBoundary::$$boundary" "$$invalidation_process_boundaries"; done; \
	timeout 10s grep -Fq 'for epoch in Epoch::ALL {' "$$invalidation_process_kill"; \
	timeout 10s grep -Fq 'for boundary in FreshDbInvalidationProcessBoundary::ALL {' "$$invalidation_process_kill"; \
	timeout 10s grep -Fq '        cases, 20,' "$$invalidation_process_kill"; \
	timeout 10s grep -Fq 'CandidateSource::RootLinksComplete' "$$invalidation_process_kill"; \
	if timeout 10s rg -n 'CandidateSource::(ALL|Intent|Exchanged)' "$$invalidation_process_kill" "$$invalidation_process_harness" "$$invalidation_process_evidence" "$$invalidation_process_boundaries"; then exit 1; else status="$$?"; timeout 10s test "$$status" = 1; fi; \
	timeout 10s grep -Fq 'assert_eq!(rollback.source, ForwardPhase::RootLinksComplete);' "$$invalidation_process_harness"; \
	timeout 10s grep -Fq 'assert_eq!(record.generation, 16);' "$$invalidation_process_harness"; \
	timeout 10s grep -Fq 'assert_eq!(successor.generation, 17);' "$$invalidation_process_harness"; \
	timeout 10s grep -Fq 'Command::new(env::current_exe().unwrap())' "$$invalidation_process_harness"; \
	timeout 10s grep -Fq '.arg(TEST_NAME)' "$$invalidation_process_harness"; \
	timeout 10s grep -Fq '.arg("--exact")' "$$invalidation_process_harness"; \
	timeout 10s grep -Fq '.arg("--test-threads=1")' "$$invalidation_process_harness"; \
	timeout 10s grep -Fq 'Some(nix::libc::SIGKILL)' "$$invalidation_process_kill"; \
	timeout 10s grep -Fq 'nix::libc::kill(nix::libc::getpid(), nix::libc::SIGKILL)' "$$invalidation_process_harness"; \
	timeout 10s grep -Fq 'struct DeadlineChild {' "$$invalidation_process_harness"; \
	timeout 10s grep -Fq 'impl Drop for DeadlineChild {' "$$invalidation_process_harness"; \
	timeout 10s grep -Fq 'Duration::from_secs(15)' "$$invalidation_process_harness"; \
	timeout 10s test "$$( timeout 10s grep -Fc 'CleanSystemStartup::enter(' "$$invalidation_process_kill" )" = 2; \
	timeout 10s grep -Fq 'release_invalidation_fixture_handles(fixture)' "$$invalidation_process_kill"; \
	timeout 10s grep -Fq 'install_persistent_selected_fresh_database(&mut fixture)' "$$invalidation_process_kill"; \
	timeout 10s grep -Fq '!candidate.selections.is_empty()' "$$invalidation_process_evidence"; \
	timeout 10s grep -Fq 'external v1 RootLinks invalidation control' "$$invalidation_process_harness"; \
	timeout 10s grep -Fq 'recovery-child-is-next-database-opener' "$$invalidation_process_harness"; \
	timeout 10s grep -Fq 'Raw inspection deliberately precedes any journal store or SQLite open.' "$$invalidation_process_kill"; \
	timeout 10s grep -Fq 'TemporaryRecordContents::Successor' "$$invalidation_process_boundaries"; \
	timeout 10s grep -Fq 'TemporaryRecordContents::Source' "$$invalidation_process_boundaries"; \
	timeout 10s grep -Fq 'RootAbiSnapshot::capture' "$$invalidation_process_kill"; \
	timeout 10s grep -Fq 'StableNamespaceSnapshot::capture' "$$invalidation_process_kill"; \
	timeout 10s grep -Fq 'expected_removals' "$$invalidation_process_kill"; \
	timeout 10s grep -Fq 'retained_exchange_syscall_count(), 0' "$$invalidation_process_kill"; \
	timeout 10s grep -Fq 'boot_synchronize_attempt_count(), 0' "$$invalidation_process_kill"; \
	timeout 10s grep -Fq 'arm_before_usr_rollback_finalization_final_revalidation' "$$invalidation_process_kill"; \
	for disclaimer in 'genuine same-boot process death' 'not a reboot simulation' 'power-loss durability oracle'; do timeout 10s grep -Fq "$$disclaimer" "$$invalidation_process_harness"; done; \
	if timeout 10s rg -n 'diesel::|remove_exact_fresh_transition|\.advance[[:space:]]*\(|persist_usr_|finalize_usr_rollback|arm_next_|FaultPoint|StorageFault' "$$invalidation_process_kill" "$$invalidation_process_harness" "$$invalidation_process_evidence" "$$invalidation_process_boundaries"; then exit 1; else status="$$?"; timeout 10s test "$$status" = 1; fi; \
	for file in "$$gate" "$$orchestrator" "$$active_orchestrator" "$$recovery_root" "$$candidate_leaf" "$$fresh_leaf" "$$finalization_leaf" "$$tests/mod.rs" "$$tests/support.rs" "$$tests/sequence.rs" "$$tests/matrix.rs" "$$tests/failures.rs" "$$tests/finalization.rs" "$$tests/preparation_failures.rs" "$$tests/storage_faults.rs" "$$tests/exclusions.rs" "$$process_kill" "$$candidate_process_kill" "$$candidate_process_harness" "$$candidate_boundaries" "$$invalidation_process_kill" "$$invalidation_process_harness" "$$invalidation_process_evidence" "$$invalidation_process_boundaries" "$$exact_fresh_removal" misc/make/startup-rollback-new-state-dispatch-tests.mk Makefile; do \
		timeout 10s test "$$( timeout 10s wc -l < "$$file" )" -le 1000; \
	done; \
	timeout 1200s $(CARGO) test -p forge --lib "$$prefix" -- --test-threads=1
