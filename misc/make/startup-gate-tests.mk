forge-client-startup-gate-test:
	@set -eu; \
	listed="$$( timeout 300s $(CARGO) test -p forge --lib -- --list )"; \
	timeout 10s test -n "$$listed"; \
	for test in \
		client::startup_gate_tests::active_reblit_replacement::compatible_database_and_active_selection_admit_restrictive_replacement_repair \
		client::startup_gate_tests::active_reblit_replacement::foreign_in_flight_database_ownership_causes_zero_replacement_chmod \
		client::startup_gate_tests::active_reblit_replacement::stale_active_selection_causes_zero_replacement_chmod \
		client::startup_gate_tests::active_reblit_replacement::mismatched_record_cannot_reuse_replacement_mutation_authority \
		client::startup_gate_tests::active_reblit_replacement::mismatched_installation_cannot_reuse_replacement_mutation_authority \
		client::startup_gate_tests::valid_unresolved_journal_precedes_malformed_live_state_system_intent_and_repositories \
		client::startup_gate_tests::corrupt_canonical_journal_blocks_startup_without_rewriting_evidence \
		client::startup_gate_tests::orphan_transition_row_precedes_malformed_live_state_and_repository_construction \
		client::startup_gate_tests::archived_state_prune_residue_types_block_startup_before_live_state_intent_and_repositories \
		client::startup_gate_tests::archived_state_prune_residue_inserted_between_bounded_scans_blocks_startup \
		client::startup_gate_tests::archived_state_prune_residue_audit_rejects_an_oversized_quarantine_without_removing_entries \
		client::startup_gate_tests::archived_state_prune_residue_audit_accepts_unrelated_entries_without_changing_them \
		client::startup_gate_tests::archived_state_prune_residue_audit_rejects_root_or_quarantine_substitution \
		client::startup_gate_tests::clean_startup_loads_the_default_intent_only_after_strict_discovery \
		client::startup_gate_tests::explicit_intent_remains_authoritative_without_loading_the_malformed_default \
		client::startup_gate_tests::cli_notice_preserves_full_verbose_and_failed_startup_semantics \
		client::startup_gate_tests::unsafe_symlink_and_hardlinked_default_sources_fail_unchanged \
		client::startup_gate_tests::default_source_substitution_after_retention_fails_closed \
		client::startup_gate_tests::default_intent_root_and_directory_name_substitution_fail_closed \
		client::startup_gate_tests::frozen_client_ignores_system_journal_and_persistent_transition_rows \
		client::startup_gate_tests::system_builder_cannot_use_frozen_discovery_to_bypass_the_startup_gate; do \
		timeout 10s grep -Fqx "$$test: test" <<<"$$listed"; \
		timeout 300s $(CARGO) test -p forge --lib "$$test" -- --exact --test-threads=1; \
	done; \
	normalizer=crates/forge/src/transition_identity/active_reblit_replacement_recovery.rs; \
	authority_contract=crates/forge/src/client/startup_reconciliation/replacement_mutation_authority.rs; \
	startup_gate=crates/forge/src/client/startup_gate.rs; \
	provider_count="$$( timeout 10s rg -n 'ActiveReblitReplacementMutationAuthorityProvider::new\(' crates/forge/src/client --glob '*.rs' --glob '!**/tests/**' --glob '!**/*_tests.rs' --glob '!**/*_tests/**' | timeout 10s wc -l )"; \
	timeout 10s test "$$provider_count" = 1; \
	timeout 10s grep -Fqx '            let mutation_seal = ActiveReblitReplacementMutationSeal::new();' "$$startup_gate"; \
	timeout 10s awk '$$0 == "            let mut mutation_authority = startup_reconciliation::ActiveReblitReplacementMutationAuthorityProvider::new(" { state = 1; next } state == 1 && $$0 == "                &mutation_seal," { state = 2; next } state == 2 && $$0 == "                installation," { state = 3; next } state == 3 && $$0 == "                &journal," { state = 4; next } state == 4 && $$0 == "                state_db," { state = 5; next } state == 5 && $$0 == "                active_state_reservation," { state = 6; next } state == 6 && $$0 == "                &record," { state = 7; next } state == 7 && $$0 == "                in_flight.clone()," { state = 8; next } state == 8 && $$0 == "            );" { found = 1 } END { exit !found }' "$$startup_gate"; \
	call_count="$$( timeout 10s rg -n 'transition_identity::recover_active_reblit_replacement_residue\(' crates/forge/src/client --glob '*.rs' --glob '!**/tests/**' --glob '!**/*_tests.rs' --glob '!**/*_tests/**' | timeout 10s wc -l )"; \
	timeout 10s test "$$call_count" = 1; \
	timeout 10s grep -Fqx '            transition_identity::recover_active_reblit_replacement_residue(&mut mutation_authority)?;' "$$startup_gate"; \
	timeout 10s awk 'previous == "    #[cfg(test)]" && $$0 == "    pub(in crate::client) fn new_for_test(" { found = 1 } { previous = $$0 } END { exit !found }' "$$authority_contract"; \
	timeout 10s awk 'previous == "#[cfg(test)]" && $$0 == "pub(crate) fn recover_active_reblit_replacement_residue_with_explicit_context_for_test(" { found = 1 } { previous = $$0 } END { exit !found }' "$$normalizer"; \
	timeout 10s awk 'previous == "#[cfg(test)]" && $$0 == "pub(crate) fn recover_active_reblit_replacement_residue_for_namespace_test(" { found = 1 } { previous = $$0 } END { exit !found }' "$$normalizer"
