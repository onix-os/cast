.PHONY: forge-clean-boot-synchronization-test \
	forge-legacy-boot-repair-test \
	forge-active-reblit-boot-projection-database-test

forge-active-reblit-boot-projection-database-test:
	@set -euo pipefail; \
	listed="$$( timeout 300s $(CARGO) test -p forge --lib -- --list )"; \
	timeout 10s grep -q . <<<"$$listed"; \
	count="$$( timeout 10s grep -Ec '^client::active_reblit_boot_projection::tests::[^:]+: test$$' <<<"$$listed" )"; \
	timeout 10s test "$$count" = 18; \
	for test in \
		client::active_reblit_boot_projection::tests::preparation_canonicalizes_and_deduplicates_the_selected_package_union \
		client::active_reblit_boot_projection::tests::reverse_id_head_and_timestamp_ties_have_deterministic_history_order \
		client::active_reblit_boot_projection::tests::one_capture_performs_exactly_two_bounded_layout_queries \
		client::active_reblit_boot_projection::tests::layout_sandwich_rejects_a_mutation_between_bounded_queries \
		client::active_reblit_boot_projection::tests::state_layout_layout_state_sandwich_rejects_a_mid_query_state_mutation \
		client::active_reblit_boot_projection::tests::package_count_policy_admits_n_and_rejects_n_plus_one_before_layout_query \
		client::active_reblit_boot_projection::tests::package_id_byte_policy_accounts_only_the_canonical_unique_union \
		client::active_reblit_boot_projection::tests::layout_row_policy_rejects_n_plus_one_rows \
		client::active_reblit_boot_projection::tests::layout_string_byte_policy_admits_n_and_rejects_n_plus_one \
		client::active_reblit_boot_projection::tests::expired_deadline_stops_before_any_layout_query \
		client::active_reblit_boot_projection::tests::cancelled_bounded_query_has_a_typed_failure \
		client::active_reblit_boot_projection::tests::revalidation_accepts_unchanged_state_and_layout_evidence \
		client::active_reblit_boot_projection::tests::revalidation_rejects_an_added_history_state \
		client::active_reblit_boot_projection::tests::revalidation_rejects_a_removed_history_state \
		client::active_reblit_boot_projection::tests::revalidation_rejects_an_exact_state_field_mutation \
		client::active_reblit_boot_projection::tests::revalidation_rejects_an_added_selected_package_layout \
		client::active_reblit_boot_projection::tests::revalidation_rejects_a_removed_selected_package_layout \
		client::active_reblit_boot_projection::tests::revalidation_rejects_reordered_layout_records; do \
		timeout 10s grep -Fqx "$$test: test" <<<"$$listed"; \
	done; \
	timeout 900s $(CARGO) test -p forge --lib "client::active_reblit_boot_projection::tests::" -- --test-threads=1

forge-clean-boot-synchronization-test:
	@set -euo pipefail; \
	listed="$$( timeout 300s $(CARGO) test -p forge --lib -- --list )"; \
	timeout 10s grep -q . <<<"$$listed"; \
	count="$$( timeout 10s grep -Ec '^client::clean_boot_synchronization::tests::[^:]+: test$$' <<<"$$listed" )"; \
	timeout 10s test "$$count" = 5; \
	for test in \
		client::clean_boot_synchronization::tests::clean_standalone_boot_synchronization_retains_authority_through_one_worker_attempt \
		client::clean_boot_synchronization::tests::final_public_journal_binding_rejects_replacement_after_the_leading_check \
		client::clean_boot_synchronization::tests::unresolved_journal_blocks_standalone_boot_before_the_worker \
		client::clean_boot_synchronization::tests::orphan_transition_row_blocks_standalone_boot_before_the_worker \
		client::clean_boot_synchronization::tests::post_authority_failure_supersedes_the_boot_backend_error; do \
		timeout 10s grep -Fqx "$$test: test" <<<"$$listed"; \
	done; \
	timeout 900s $(CARGO) test -p forge --lib "client::clean_boot_synchronization::tests::" -- --test-threads=1

forge-legacy-boot-repair-test:
	@set -euo pipefail; \
	listed="$$( timeout 300s $(CARGO) test -p forge --lib -- --list )"; \
	timeout 10s grep -q . <<<"$$listed"; \
	count="$$( timeout 10s grep -Ec '^client::legacy_boot_repair::tests::[^:]+: test$$' <<<"$$listed" )"; \
	timeout 10s test "$$count" = 4; \
	for test in \
		client::legacy_boot_repair::tests::legacy_worker_rejects_a_client_with_a_different_state_database_capability \
		client::legacy_boot_repair::tests::legacy_worker_rejects_public_journal_replacement_during_boot \
		client::legacy_boot_repair::tests::legacy_worker_retains_the_exact_journal_lock_through_boot \
		client::legacy_boot_repair::tests::legacy_authorization_rechecks_orphan_transition_ownership; do \
		timeout 10s grep -Fqx "$$test: test" <<<"$$listed"; \
	done; \
	timeout 900s $(CARGO) test -p forge --lib "client::legacy_boot_repair::tests::" -- --test-threads=1
