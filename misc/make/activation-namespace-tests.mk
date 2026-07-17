.PHONY: forge-startup-activation-namespace-test

forge-startup-activation-namespace-test:
	@set -eu; \
	listed="$$( timeout 300s $(CARGO) test -p forge --lib -- --list )"; \
	timeout 10s grep -q . <<<"$$listed"; \
	count="$$( timeout 10s grep -c '^client::startup_reconciliation::activation_namespace::tests::.*: test$$' <<<"$$listed" )"; \
	timeout 10s test "$$count" = 24; \
	for test in \
		client::startup_reconciliation::activation_namespace::tests::usr_xattrs::startup_activation_inventory_rejects_live_usr_extended_attributes \
		client::startup_reconciliation::activation_namespace::tests::usr_xattrs::startup_activation_inventory_rejects_staged_usr_extended_attributes \
		client::startup_reconciliation::activation_namespace::tests::usr_xattrs::startup_activation_retained_revalidation_rejects_new_usr_extended_attributes \
		client::startup_reconciliation::activation_namespace::tests::usr_xattrs::startup_activation_retained_revalidation_rejects_new_staged_usr_extended_attributes \
		client::startup_reconciliation::activation_namespace::tests::startup_activation_inventory_accepts_exact_preparing_layout_without_mutation \
		client::startup_reconciliation::activation_namespace::tests::startup_activation_inventory_rejects_raw_names_bounds_acls_and_isolation_foreign_entries \
		client::startup_reconciliation::activation_namespace::tests::startup_activation_inventory_final_revalidation_detects_public_namespace_substitution \
		client::startup_reconciliation::activation_namespace::tests::startup_activation_inventory_binds_slot_links_to_transition_role_and_state \
		client::startup_reconciliation::activation_namespace::tests::startup_activation_inventory_active_reblit_previous_state_id_is_typed \
		client::startup_reconciliation::activation_namespace::tests::startup_activation_inventory_active_reblit_preserve_accepts_only_paired_destinations \
		client::startup_reconciliation::activation_namespace::tests::startup_activation_policy_forward_layout_matrix_is_exact \
		client::startup_reconciliation::activation_namespace::tests::startup_activation_policy_rollback_actions_override_source_ordinal \
		client::startup_reconciliation::activation_namespace::tests::startup_activation_policy_cleanup_and_abi_matrix_is_exact \
		client::startup_reconciliation::activation_namespace::tests::isolation_abi::startup_activation_isolation_abi_crash_prefixes_match_trigger_phase_contract \
		client::startup_reconciliation::activation_namespace::tests::partial_replacement::candidate_prepared_restrictive_replacement_residues_are_normalized_then_admitted \
		client::startup_reconciliation::activation_namespace::tests::partial_replacement::rollback_from_candidate_prepared_can_finish_the_same_replacement_residue \
		client::startup_reconciliation::activation_namespace::tests::partial_replacement::canonical_populated_rollback_wrapper_is_delegated_to_phase_policy_unchanged \
		client::startup_reconciliation::activation_namespace::tests::partial_replacement::replacement_residue_is_never_normalized_before_or_after_candidate_prepared \
		client::startup_reconciliation::activation_namespace::tests::partial_replacement::foreign_replacement_residue_is_untouched_and_rejected \
		client::startup_reconciliation::activation_namespace::tests::partial_replacement::ambiguous_current_transition_replacements_are_both_untouched \
		client::startup_reconciliation::activation_namespace::tests::partial_replacement::second_current_transition_replacement_inserted_before_chmod_leaves_both_inodes_untouched \
		client::startup_reconciliation::activation_namespace::tests::partial_replacement::journal_advance_before_chmod_preserves_both_replacement_inodes_and_names \
		client::startup_reconciliation::activation_namespace::tests::partial_replacement::public_name_substitution_before_normalization_chmods_neither_inode \
		client::startup_reconciliation::activation_namespace::tests::partial_replacement::populated_replacement_is_normalized_durably_but_never_admitted; do \
		timeout 10s grep -Fqx "$$test: test" <<<"$$listed"; \
	done; \
	capture_contract="crates/forge/src/client/startup_reconciliation/activation_namespace/capture/mod.rs"; \
	capture_model="crates/forge/src/client/startup_reconciliation/activation_namespace/capture/model.rs"; \
	capture_wrappers="crates/forge/src/client/startup_reconciliation/activation_namespace/capture/wrappers.rs"; \
	timeout 10s grep -Fqx 'mod isolation_abi;' crates/forge/src/client/startup_reconciliation/activation_namespace/tests.rs; \
	timeout 10s grep -Fqx 'mod partial_replacement;' crates/forge/src/client/startup_reconciliation/activation_namespace/tests.rs; \
	timeout 10s grep -Fqx 'mod usr_xattrs;' crates/forge/src/client/startup_reconciliation/activation_namespace/tests.rs; \
	timeout 10s grep -Fq 'const ISOLATION_SCAFFOLD_DIRECTORIES: [&[u8]; 6]' "$$capture_contract"; \
	timeout 10s grep -Fqx 'pub(super) struct RetainedIsolationScaffold {' "$$capture_model"; \
	timeout 10s grep -Fqx '    pub(super) isolation_scaffolds: Vec<RetainedIsolationScaffold>,' "$$capture_model"; \
	timeout 10s grep -Fq 'ISOLATION_SCAFFOLD_DIRECTORIES.contains(&child.as_slice())' "$$capture_wrappers"; \
	timeout 10s grep -Fq 'for scaffold in &wrapper.isolation_scaffolds {' "$$capture_model"; \
	timeout 10s awk '$$0 ~ /^fn safe_usr_witness\(store: &TreeMarkerStore/ { active = 1 } active && $$0 == "    budget.operation(path)?;" { operations++ } active && $$0 ~ /let witness = InodeWitness::read/ { before = NR } active && $$0 ~ /require_no_access_acl_until/ { access = NR } active && $$0 ~ /require_no_default_acl_until/ { default_acl = NR } active && $$0 ~ /require_no_xattrs_until/ { xattrs = NR } active && $$0 ~ /if InodeWitness::read\(file, path\)\? != witness/ { after = NR } active && $$0 ~ /Ok\(witness\)/ { valid = operations == 5 && before < access && access < default_acl && default_acl < xattrs && xattrs < after; exit !valid } END { exit !valid }' "$$capture_contract"; \
	timeout 10s grep -Fqx '    let directory_witness = safe_usr_witness(&store, &path, budget)?;' "$$capture_wrappers"; \
	timeout 10s test "$$( timeout 10s wc -l < crates/forge/src/client/startup_reconciliation/activation_namespace/tests/usr_xattrs.rs )" -le 1000; \
	timeout 900s $(CARGO) test -p forge --lib \
		"client::startup_reconciliation::activation_namespace::tests::" \
		-- --test-threads=1
