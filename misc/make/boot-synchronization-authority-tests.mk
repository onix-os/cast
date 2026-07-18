.PHONY: forge-clean-boot-synchronization-test \
	forge-legacy-boot-repair-test \
	forge-active-reblit-boot-projection-database-test \
	forge-active-reblit-boot-asset-plan-test \
	forge-boot-asset-snapshot-test

forge-active-reblit-boot-asset-plan-test:
	@set -euo pipefail; \
	listed="$$( timeout 300s $(CARGO) test -p forge --lib -- --list )"; \
	timeout 10s grep -q . <<<"$$listed"; \
	count="$$( timeout 10s grep -Ec '^client::active_reblit_boot_projection::asset_plan::tests::[^:]+: test$$' <<<"$$listed" )"; \
	timeout 10s test "$$count" = 21; \
	for test in \
		client::active_reblit_boot_projection::asset_plan::tests::complete_plan_is_state_scoped_deterministic_and_role_complete \
		client::active_reblit_boot_projection::asset_plan::tests::canonical_empty_digest_is_rejected_for_every_critical_boot_role \
		client::active_reblit_boot_projection::asset_plan::tests::optional_empty_assets_share_one_sorted_snapshot_digest \
		client::active_reblit_boot_projection::asset_plan::tests::role_references_above_snapshot_limit_share_a_bounded_digest_inventory \
		client::active_reblit_boot_projection::asset_plan::tests::no_head_systemd_boot_asset_is_not_applicable \
		client::active_reblit_boot_projection::asset_plan::tests::systemd_boot_without_any_kernel_is_not_applicable \
		client::active_reblit_boot_projection::asset_plan::tests::kernel_less_history_does_not_contribute_unused_schema_or_cmdline_assets \
		client::active_reblit_boot_projection::asset_plan::tests::historical_systemd_boot_assets_are_not_bootloader_authority \
		client::active_reblit_boot_projection::asset_plan::tests::two_head_systemd_boot_assets_are_rejected \
		client::active_reblit_boot_projection::asset_plan::tests::unselected_layouts_cannot_enter_or_poison_a_state_plan \
		client::active_reblit_boot_projection::asset_plan::tests::identical_multi_package_owners_collapse_but_conflicts_fail \
		client::active_reblit_boot_projection::asset_plan::tests::the_same_logical_path_may_resolve_differently_in_distinct_states \
		client::active_reblit_boot_projection::asset_plan::tests::final_boot_asset_symlinks_resolve_to_regular_cas_bytes \
		client::active_reblit_boot_projection::asset_plan::tests::symlink_cycles_and_missing_targets_fail_closed \
		client::active_reblit_boot_projection::asset_plan::tests::invalid_symlink_targets_and_file_modes_fail_closed \
		client::active_reblit_boot_projection::asset_plan::tests::ownership_and_unsupported_mode_bits_fail_before_planning \
		client::active_reblit_boot_projection::asset_plan::tests::symlink_hop_byte_and_depth_limits_are_exact \
		client::active_reblit_boot_projection::asset_plan::tests::descendants_beneath_symlink_or_regular_ancestors_fail_closed \
		client::active_reblit_boot_projection::asset_plan::tests::selected_invalid_stone_target_is_rejected_before_classification \
		client::active_reblit_boot_projection::asset_plan::tests::asset_path_kernel_snapshot_and_work_bounds_fail_with_typed_errors \
		client::active_reblit_boot_projection::asset_plan::tests::expired_planning_deadline_fails_before_asset_admission; do \
		timeout 10s grep -Fqx "$$test: test" <<<"$$listed"; \
	done; \
	timeout 900s $(CARGO) test -p forge --lib "client::active_reblit_boot_projection::asset_plan::tests::" -- --test-threads=1

forge-boot-asset-snapshot-test:
	@set -euo pipefail; \
	listed="$$( timeout 300s $(CARGO) test -p forge --lib -- --list )"; \
	timeout 10s grep -q . <<<"$$listed"; \
	count="$$( timeout 10s grep -Ec '^client::boot_asset_snapshots::tests::[^:]+: test$$' <<<"$$listed" )"; \
	timeout 10s test "$$count" = 16; \
	for test in \
		client::boot_asset_snapshots::tests::sealed_snapshot_has_exact_bytes_digest_length_metadata_and_seals \
		client::boot_asset_snapshots::tests::sealed_snapshot_rejects_write_shrink_grow_and_additional_seals \
		client::boot_asset_snapshots::tests::wrong_digest_fails_without_publishing_a_snapshot \
		client::boot_asset_snapshots::tests::count_and_aggregate_byte_limits_admit_n_and_reject_n_plus_one \
		client::boot_asset_snapshots::tests::per_asset_and_descriptor_budgets_fail_before_memfd_allocation \
		client::boot_asset_snapshots::tests::expired_deadline_fails_before_opening_the_asset_pool \
		client::boot_asset_snapshots::tests::canonical_empty_asset_is_sealed_without_an_asset_pool \
		client::boot_asset_snapshots::tests::short_digest_uses_the_descriptor_rooted_flat_asset_path \
		client::boot_asset_snapshots::tests::fifo_and_symlink_sources_fail_closed_without_blocking \
		client::boot_asset_snapshots::tests::source_replacement_after_open_fails_closed \
		client::boot_asset_snapshots::tests::source_mutation_after_copy_fails_closed \
		client::boot_asset_snapshots::tests::failed_batch_drops_prior_snapshots_and_leaves_policy_reusable \
		client::boot_asset_snapshots::tests::duplicate_digest_is_canonicalized_without_a_duplicate_snapshot \
		client::boot_asset_snapshots::tests::digest_lookup_is_sorted_deduplicated_and_independent_of_descriptor_offsets \
		client::boot_asset_snapshots::tests::source_growth_after_length_preflight_fails_before_snapshot_publication \
		client::boot_asset_snapshots::tests::materialization_timeout_maps_to_the_boot_snapshot_deadline; do \
		timeout 10s grep -Fqx "$$test: test" <<<"$$listed"; \
	done; \
	timeout 900s $(CARGO) test -p forge --lib "client::boot_asset_snapshots::tests::" -- --test-threads=1

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
