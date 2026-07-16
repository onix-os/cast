.PHONY: forge-startup-activation-namespace-test

forge-startup-activation-namespace-test:
	@set -eu; \
	listed="$$( timeout 300s $(CARGO) test -p forge --lib -- --list )"; \
	timeout 10s test -n "$$listed"; \
	count="$$( timeout 10s grep -c '^client::startup_reconciliation::activation_namespace::tests::startup_activation_.*: test$$' <<<"$$listed" )"; \
	timeout 10s test "$$count" = 9; \
	for test in \
		client::startup_reconciliation::activation_namespace::tests::startup_activation_inventory_accepts_exact_preparing_layout_without_mutation \
		client::startup_reconciliation::activation_namespace::tests::startup_activation_inventory_rejects_raw_names_bounds_acls_and_isolation_foreign_entries \
		client::startup_reconciliation::activation_namespace::tests::startup_activation_inventory_final_revalidation_detects_public_namespace_substitution \
		client::startup_reconciliation::activation_namespace::tests::startup_activation_inventory_binds_slot_links_to_transition_role_and_state \
		client::startup_reconciliation::activation_namespace::tests::startup_activation_inventory_active_reblit_previous_state_id_is_typed \
		client::startup_reconciliation::activation_namespace::tests::startup_activation_inventory_active_reblit_preserve_accepts_only_paired_destinations \
		client::startup_reconciliation::activation_namespace::tests::startup_activation_policy_forward_layout_matrix_is_exact \
		client::startup_reconciliation::activation_namespace::tests::startup_activation_policy_rollback_actions_override_source_ordinal \
		client::startup_reconciliation::activation_namespace::tests::startup_activation_policy_cleanup_and_abi_matrix_is_exact; do \
		timeout 10s grep -Fqx "$$test: test" <<<"$$listed"; \
	done; \
	timeout 900s $(CARGO) test -p forge --lib \
		"client::startup_reconciliation::activation_namespace::tests::startup_activation_" \
		-- --test-threads=1
