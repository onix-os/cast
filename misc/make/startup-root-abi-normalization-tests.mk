.PHONY: forge-startup-usr-exchanged-root-abi-normalization-test

forge-startup-usr-exchanged-root-abi-normalization-test:
	@set -eu; \
	listed="$$( timeout 300s $(CARGO) test -p forge --lib -- --list )"; \
	timeout 10s grep -q . <<<"$$listed"; \
	count="$$( timeout 10s grep -c '^client::startup_recovery::usr_exchanged_root_abi_normalization::tests::.*: test$$' <<<"$$listed" )"; \
	timeout 10s test "$$count" = 19; \
	for test in \
		client::startup_recovery::usr_exchanged_root_abi_normalization::tests::subset_matrix::startup_usr_exchanged_root_abi_all_canonical_subsets_converge_without_phase_skip \
		client::startup_recovery::usr_exchanged_root_abi_normalization::tests::effect_faults::startup_usr_exchanged_root_abi_publisher_sync_failure_requires_complete_retry_sync \
		client::startup_recovery::usr_exchanged_root_abi_normalization::tests::effect_faults::startup_usr_exchanged_root_abi_complete_sync_failure_retries_without_publication \
		client::startup_recovery::usr_exchanged_root_abi_normalization::tests::effect_faults::startup_usr_exchanged_root_abi_exact_eexist_is_authenticated_and_wrong_eexist_fails_partial \
		client::startup_recovery::usr_exchanged_root_abi_normalization::tests::effect_faults::startup_usr_exchanged_root_abi_foreign_collision_at_every_publication_index_stays_at_source \
		client::startup_recovery::usr_exchanged_root_abi_normalization::tests::effect_faults::startup_usr_exchanged_root_abi_next_name_race_after_preflight_fails_partial \
		client::startup_recovery::usr_exchanged_root_abi_normalization::tests::effect_faults::startup_usr_exchanged_root_abi_existing_and_new_exact_target_aba_fail_closed \
		client::startup_recovery::usr_exchanged_root_abi_normalization::tests::evidence_races::startup_usr_exchanged_root_abi_final_database_guard_blocks_pre_effect_race \
		client::startup_recovery::usr_exchanged_root_abi_normalization::tests::evidence_races::startup_usr_exchanged_root_abi_same_bytes_journal_replacement_breaks_record_binding \
		client::startup_recovery::usr_exchanged_root_abi_normalization::tests::evidence_races::startup_usr_exchanged_root_abi_public_journal_directory_replacement_blocks_effect \
		client::startup_recovery::usr_exchanged_root_abi_normalization::tests::evidence_races::startup_usr_exchanged_root_abi_post_publication_database_race_fails_before_success \
		client::startup_recovery::usr_exchanged_root_abi_normalization::tests::evidence_races::startup_usr_exchanged_root_abi_complete_sync_guards_database_and_link_aba \
		client::startup_recovery::usr_exchanged_root_abi_normalization::tests::evidence_races::startup_usr_exchanged_root_abi_non_root_post_effect_race_is_ambiguous \
		client::startup_recovery::usr_exchanged_root_abi_normalization::tests::evidence_races::startup_usr_exchanged_root_abi_root_file_and_symlink_races_are_ambiguous \
		client::startup_recovery::usr_exchanged_root_abi_normalization::tests::evidence_races::startup_usr_exchanged_root_abi_public_root_replacement_blocks_without_mutation \
		client::startup_recovery::usr_exchanged_root_abi_normalization::tests::evidence_races::startup_usr_exchanged_root_abi_post_publication_root_replacement_is_ambiguous \
		client::startup_recovery::usr_exchanged_root_abi_normalization::tests::production_dispatch::startup_usr_exchanged_root_abi_temporary_and_foreign_final_names_never_mutate_or_decide \
		client::startup_recovery::usr_exchanged_root_abi_normalization::tests::production_dispatch::startup_usr_exchanged_root_abi_incomplete_success_ends_one_entry_at_source \
		client::startup_recovery::usr_exchanged_root_abi_normalization::tests::production_dispatch::startup_root_abi_normalizer_is_sealed_from_non_usr_exchanged_sources; do \
		timeout 10s grep -Fqx "$$test: test" <<<"$$listed"; \
	done; \
	proof=crates/forge/src/client/startup_reconciliation/activation_namespace/usr_exchanged_root_abi_proof.rs; \
	authority=crates/forge/src/client/startup_reconciliation/usr_exchanged_root_abi_authority.rs; \
	executor=crates/forge/src/client/startup_recovery/usr_exchanged_root_abi_normalization.rs; \
	gate=crates/forge/src/client/startup_gate.rs; \
	timeout 10s test "$$( timeout 10s grep -Fc 'crate::client::create_root_links_retained(root_path, root)' "$$proof" )" = 1; \
	timeout 10s test "$$( timeout 10s rg -n 'Phase::UsrExchanged' "$$proof" "$$authority" | timeout 10s wc -l )" -ge 2; \
	if timeout 10s rg -n 'Phase::RootLinksComplete|\.advance\(|\.delete\(|\.create\(' "$$proof" "$$authority" "$$executor"; then exit 1; fi; \
	if timeout 10s rg -n 'exchange_forward|exchange_reverse|run_transaction_triggers|run_system_triggers|archive_previous|remove_file|remove_dir|renameat|unlinkat' "$$proof" "$$authority" "$$executor"; then exit 1; fi; \
	normalize_line="$$( timeout 10s grep -nF 'UsrExchangedRootAbiNormalizationAuthority::capture(' "$$gate" | timeout 10s cut -d: -f1 )"; \
	decision_line="$$( timeout 10s grep -nF 'UsrRollbackDecisionAuthority::capture(' "$$gate" | timeout 10s cut -d: -f1 )"; \
	timeout 10s test "$$normalize_line" -lt "$$decision_line"; \
	for file in "$$proof" "$$authority" "$$executor" \
		crates/forge/src/client/startup_reconciliation/activation_namespace/capture/root_entries.rs \
		crates/forge/src/client/startup_recovery/usr_exchanged_root_abi_normalization/tests/subset_matrix.rs \
		crates/forge/src/client/startup_recovery/usr_exchanged_root_abi_normalization/tests/effect_faults.rs \
		crates/forge/src/client/startup_recovery/usr_exchanged_root_abi_normalization/tests/evidence_races.rs \
		crates/forge/src/client/startup_recovery/usr_exchanged_root_abi_normalization/tests/production_dispatch.rs \
		misc/make/startup-root-abi-normalization-tests.mk; do \
		timeout 10s test "$$( timeout 10s wc -l < "$$file" )" -le 1000; \
	done; \
	timeout 1200s $(CARGO) test -p forge --lib \
		'client::startup_recovery::usr_exchanged_root_abi_normalization::tests::' \
		-- --test-threads=1
