.PHONY: forge-read-only-installation-test forge-read-only-substrate-test \
	stone-recipe-derivation-provenance-test container-cgroup-test \
	container-process-runtime-test gitwrap-repository-fs-test \
	forge-security-fixture-test

forge-read-only-installation-test:
	@set -eu; \
	listed="$$( $(CARGO) test -p forge --lib -- --list )"; \
	test -n "$$listed"; \
	for test in \
		installation::snapshot::tests::two_readers_share_global_and_custom_cache_locks_until_the_last_reader_drops \
		installation::snapshot::tests::writable_root_opened_explicitly_read_only_never_becomes_mutable_or_frozen \
		installation::snapshot::tests::mutable_and_frozen_modes_do_not_expose_read_only_snapshot_authority \
		installation::snapshot::tests::explicit_snapshot_is_rejected_before_client_coordinator_or_database_mutation \
		installation::snapshot::tests::naturally_read_only_open_is_rejected_before_client_coordinator_or_database_mutation \
		installation::snapshot::tests::contended_shared_snapshot_lock_has_a_typed_zero_budget_timeout_without_mutation \
		installation::snapshot::tests::missing_cast_fails_without_creating_or_changing_any_entry \
		installation::snapshot::tests::missing_default_cache_fails_without_recreating_or_changing_any_entry \
		installation::snapshot::tests::missing_global_lock_fails_without_recreating_it \
		installation::snapshot::tests::missing_custom_cache_lock_fails_without_recreating_it \
		installation::snapshot::tests::missing_custom_cache_directory_fails_without_creating_or_changing_any_entry \
		installation::snapshot::tests::retained_snapshot_rejects_installation_root_substitution \
		installation::snapshot::tests::retained_snapshot_rejects_cast_directory_substitution \
		installation::snapshot::tests::retained_snapshot_rejects_lockfile_substitution \
		installation::snapshot::tests::retained_snapshot_rejects_default_cache_directory_substitution \
		installation::snapshot::tests::retained_snapshot_rejects_custom_cache_directory_substitution \
		installation::snapshot::tests::retained_snapshot_rejects_custom_cache_lockfile_substitution \
		installation::snapshot::tests::open_revalidate_clone_and_drop_leave_recursive_metadata_and_contents_unchanged; do \
		grep -Fqx "$$test: test" <<<"$$listed"; \
		$(CARGO) test -p forge --lib "$$test" -- --exact --test-threads=1; \
	done

forge-read-only-substrate-test:
	@set -eu; \
	listed="$$( $(CARGO) test -p forge --lib -- --list )"; \
	test -n "$$listed"; \
	for test in \
		db::read_only::tests::deserialized_adapters_query_exact_state_meta_and_selected_layout_without_mutation \
		db::read_only::tests::authorizer_denies_writes_and_functions_and_connection_remains_clean \
		db::read_only::tests::opcode_and_deadline_interruptions_are_deterministic_and_handlers_are_cleared \
		db::read_only::tests::temp_store_is_memory_only_and_ordered_scan_leaves_source_unchanged \
		db::read_only::tests::sidecar_inode_kinds_fail_closed_and_are_preserved \
		db::read_only::tests::oversized_database_image_fails_before_allocation_without_mutation \
		db::read_only::tests::corrupt_database_image_fails_typed_without_mutation \
		db::read_only::tests::metadata_reconstructed_id_and_i32_release_corruption_fail_typed \
		db::read_only::tests::missing_unknown_and_extra_diesel_migrations_fail_typed \
		db::read_only::tests::absent_migration_table_is_version_set_validation_failure_not_migration \
		transition_journal::read_only::tests::absent_and_preexisting_clean_journals_are_retained_without_provisioning \
		transition_journal::read_only::tests::valid_canonical_transition_fails_closed_and_is_preserved \
		transition_journal::read_only::tests::corrupt_canonical_and_interrupted_temporary_fail_closed_unchanged; do \
		grep -Fqx "$$test: test" <<<"$$listed"; \
		$(CARGO) test -p forge --lib "$$test" -- --exact --test-threads=1; \
	done

stone-recipe-derivation-provenance-test:
	@set -eu; \
	listed="$$( timeout 300s $(CARGO) test -p stone_recipe --lib -- --list )"; \
	test -n "$$listed"; \
	for test in \
		derivation::tests::identical_plans_have_identical_bytes_and_ids \
		derivation::tests::complete_evaluation_fingerprint_is_part_of_canonical_identity \
		derivation::tests::nested_provenance_shape_and_order_are_part_of_canonical_identity \
		derivation::tests::v2_provenance_aggregate_helpers_preserve_nested_semantics \
		derivation::tests::validation_rejects_invalid_nested_evaluation_fingerprints_at_the_exact_field \
		derivation::tests::validation_rejects_ambient_or_non_normalized_provenance_names \
		derivation::tests::validation_binds_recipe_and_profiles_to_their_locked_inputs \
		derivation::tests::validation_binds_policy_name_root_and_composition_to_the_build_lock \
		derivation::tests::validation_rejects_non_normalized_policy_origins \
		derivation::tests::validation_replays_policy_transition_state; do \
		grep -Fqx "$$test: test" <<<"$$listed"; \
		timeout 300s $(CARGO) test -p stone_recipe --lib "$$test" -- --exact --test-threads=1; \
	done

container-cgroup-test:
	@set -eu; \
	listed="$$( timeout 120s $(CARGO) test -p container --lib -- --list )"; \
	count="$$( timeout 10s grep -c '^cgroup::tests::.*: test$$' <<<"$$listed" )"; \
	test "$$count" = 41; \
	timeout 300s $(CARGO) test -p container --lib "cgroup::tests::" -- --test-threads=1

# Socket-diagnostic tests remain in the complete `make test` lane. The local
# sandbox denies their `send(MSG_NOSIGNAL | MSG_DONTWAIT)` syscall with EPERM;
# this focused lane covers the two moved helpers plus every host-safe pidfd,
# signal-mask, signal-action, and lifecycle test without misreporting a skip.
container-process-runtime-test:
	@set -eu; \
	listed="$$( timeout 120s $(CARGO) test -p container --lib -- --list )"; \
	test -n "$$listed"; \
	for test in \
		process_runtime::launch_support::tests::clone_stack_has_a_non_accessible_guard_and_read_write_usable_mapping \
		process_runtime::launch_support::tests::error_transport_format_is_bounded_even_for_cyclic_and_huge_sources \
		tests::pidfd_wait_and_signal_preserve_exact_terminal_statuses \
		tests::valid_pidfd_cleanup_kills_and_reaps_without_numeric_wait \
		tests::pidfd_reap_deadline_is_finite_and_leaves_authority_recoverable \
		tests::successful_cgroup_drain_retry_reaps_by_pidfd_and_restores_primary_failure \
		tests::already_reaped_pidfd_cleanup_accepts_only_the_authoritative_terminal_pair \
		tests::dropping_unrecovered_pidfd_authority_aborts_an_isolated_process \
		tests::invalid_pidfd_cleanup_never_falls_back_and_retains_authority \
		tests::signal_override_restores_the_exact_previous_action \
		tests::blocked_clone_signal_mask_restores_the_exact_previous_mask \
		tests::raw_clone_child_guard_can_retain_blocked_mask_until_exit \
		tests::signal_overrides_are_serialized_across_concurrent_runs; do \
		timeout 10s grep -Fqx "$$test: test" <<<"$$listed"; \
		timeout 120s $(CARGO) test -p container --lib "$$test" -- --exact --test-threads=1; \
	done

gitwrap-repository-fs-test:
	@set -eu; \
	listed="$$( timeout 180s $(CARGO) test -p gitwrap --lib -- --list )"; \
	test -n "$$listed"; \
	for test in \
		tests::repository_limits_accept_exact_n_and_reject_n_plus_one \
		tests::strict_entry_quota_rejects_n_plus_one_without_sampling_slack \
		tests::live_scan_may_retry_a_vanished_name_but_strict_scan_fails_closed \
		tests::live_scan_allows_initial_absence_without_building_a_strict_inventory \
		tests::strict_relative_path_allocation_is_prechecked_against_snapshot_budget \
		tests::strict_two_snapshot_verification_rejects_same_name_inode_replacement \
		tests::descriptor_rooted_quota_scan_never_follows_nested_or_root_symlinks \
		tests::quota_scan_rejects_nesting_before_exhausting_parent_descriptors \
		tests::quota_scan_rejects_a_budget_too_small_for_one_cursor \
		tests::quota_scanner_reserves_descriptors_already_open_in_the_parent \
		tests::repository_rejects_a_replaced_public_path_while_root_is_pinned \
		tests::quota_scan_uses_the_subprocess_absolute_deadline \
		tests::oversized_cached_mirror_is_rejected_before_git_is_started \
		tests::remote_url_mutation_is_rejected_when_it_crosses_repository_quota \
		tests::failed_public_fetch_never_deletes_a_caller_owned_repository \
		tests::incremental_quota_scan_does_not_starve_a_full_stdout_pipe \
		tests::oversized_clone_is_rejected_without_final_or_staging_state \
		tests::published_mirror_and_credential_config_are_owner_private \
		tests::private_mirror_strips_hostile_local_config_before_open_and_fetch \
		tests::private_mirror_origin_is_checked_before_config_is_rewritten \
		tests::sha256_object_format_commit_ids_are_accepted; do \
		timeout 10s grep -Fqx "$$test: test" <<<"$$listed"; \
		timeout 180s $(CARGO) test -p gitwrap --lib "$$test" -- --exact --test-threads=1; \
	done

forge-security-fixture-test:
	@set -eu; \
	listed="$$( timeout 180s $(CARGO) test -p forge --lib -- --list )"; \
	test -n "$$listed"; \
	for test in \
		cli::repo::tests::authored_system_intent_rejects_imperative_repository_changes \
		client::tests::state_creation_records_and_exports_the_generated_snapshot \
		installation::tests::both_open_modes_defer_invalid_system_intent_but_frozen_skips_active_state \
		tree_marker::tests::canonical_marker_rejects_links_wrong_kinds_and_mutable_modes; do \
		timeout 10s grep -Fqx "$$test: test" <<<"$$listed"; \
		timeout 180s $(CARGO) test -p forge --lib "$$test" -- --exact --test-threads=1; \
	done
