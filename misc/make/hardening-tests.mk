.PHONY: stone-read-test forge-read-only-installation-test forge-installation-test \
	forge-linux-fs-test forge-cache-test forge-client-direct-test \
	forge-database-adapter-test forge-read-only-substrate-test \
	forge-read-only-client-test forge-transition-journal-contract-test \
	forge-transition-runtime-evidence-test forge-transition-journal-test \
	stone-recipe-derivation-provenance-test \
	stone-recipe-derivation-validation-test stone-recipe-build-lock-test \
	stone-recipe-package-validation-test \
	stone-recipe-build-policy-validation-test stone-recipe-build-policy-contract-test \
	stone-recipe-build-policy-patch-test tools-buildinfo-semantic-fingerprint-test \
	container-cgroup-test \
	container-process-runtime-test container-mount-boundary-test \
	container-root-host-safe-test mason-package-collect-test \
	mason-package-collect-transaction-test mason-analysis-handler-test mason-emit-test \
	mason-archive-test mason-package-publication-test \
	mason-git-materialization-test mason-paths-test mason-executor-test \
	mason-build-context-test mason-recipe-explanation-test \
	mason-upstream-git-cache-test mason-build-root-test mason-profile-test \
	mason-planner-bootstrap-test mason-policy-test \
	config-gluon-store-test gitwrap-repository-fs-test gitwrap-all-test \
	forge-repository-manager-test \
	forge-security-fixture-test

forge-read-only-installation-test:
	@set -eu; \
	listed="$$( timeout 180s $(CARGO) test -p forge --lib -- --list )"; \
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
		timeout 10s grep -Fqx "$$test: test" <<<"$$listed"; \
		timeout 180s $(CARGO) test -p forge --lib "$$test" -- --exact --test-threads=1; \
	done

forge-installation-test:
	@set -eu; \
	listed="$$( timeout 300s $(CARGO) test -p forge --lib -- --list )"; \
	count="$$( timeout 10s grep -c '^installation::tests::.*: test$$' <<<"$$listed" )"; \
	timeout 10s test "$$count" = 28; \
	timeout 900s $(CARGO) test -p forge --lib "installation::tests::" -- --test-threads=1

forge-linux-fs-test:
	@set -eu; \
	listed="$$( timeout 300s $(CARGO) test -p forge --lib -- --list )"; \
	timeout 10s test -n "$$listed"; \
	count="$$( timeout 10s grep -c '^linux_fs::tests::.*: test$$' <<<"$$listed" )"; \
	timeout 10s test "$$count" = 16; \
	for test in \
		linux_fs::tests::interrupted_retry_limit_accepts_n_and_rejects_n_plus_one \
		linux_fs::tests::expired_retry_deadline_fails_before_another_syscall \
		linux_fs::tests::expired_rename_deadline_preserves_both_namespaces \
		linux_fs::tests::expired_sync_filesystem_deadline_fails_before_syncfs \
		linux_fs::tests::procfs_authentication_rejects_an_ordinary_filesystem \
		linux_fs::tests::proc_pid_parser_accepts_only_bounded_canonical_decimal \
		linux_fs::tests::thread_self_parser_requires_exact_current_process_and_thread \
		linux_fs::tests::chmod_revalidates_the_exact_opath_inode_and_mode \
		linux_fs::tests::descriptor_times_update_the_retained_regular_inode_not_its_replacement \
		linux_fs::tests::descriptor_read_uses_the_retained_inode_and_preserves_atime \
		linux_fs::tests::descriptor_read_rejects_non_regular_capabilities \
		linux_fs::tests::descriptor_times_support_a_mode_zero_directory \
		linux_fs::tests::descriptor_times_update_a_symlink_without_touching_its_target \
		linux_fs::tests::authenticated_procfs_links_an_unnamed_inode_without_privilege \
		linux_fs::tests::new_directory_normalization_retains_identity_and_rejects_name_substitution \
		linux_fs::tests::chmod_uses_the_calling_tasks_private_descriptor_table; do \
		timeout 10s grep -Fqx "$$test: test" <<<"$$listed"; \
		timeout 300s $(CARGO) test -p forge --lib "$$test" -- --exact --test-threads=1; \
	done

forge-cache-test:
	@set -eu; \
	listed="$$( timeout 300s $(CARGO) test -p forge --lib -- --list )"; \
	timeout 10s test -n "$$listed"; \
	count="$$( timeout 10s grep -c '^client::cache::download_limit_tests::.*: test$$' <<<"$$listed" )"; \
	timeout 10s test "$$count" = 11; \
	for test in \
		client::cache::download_limit_tests::declared_package_size_can_only_tighten_the_global_ceiling \
		client::cache::download_limit_tests::cached_package_symlink_is_rejected_without_reading_target \
		client::cache::download_limit_tests::cached_package_fifo_is_rejected_without_blocking \
		client::cache::download_limit_tests::cached_package_requires_the_exact_declared_size_at_n_and_n_plus_one \
		client::cache::download_limit_tests::retained_download_descriptor_defeats_path_substitution_before_unpack \
		client::cache::download_limit_tests::asset_publication_replaces_fifo_and_symlink_without_blocking_or_touching_target \
		client::cache::download_limit_tests::asset_authentication_rejects_truncated_and_n_plus_one_entries \
		client::cache::download_limit_tests::competing_asset_publishers_reuse_one_verified_winner \
		client::cache::download_limit_tests::competing_download_publishers_reuse_one_verified_winner \
		client::cache::download_limit_tests::armed_publication_cleanup_removes_only_the_exact_moved_inode \
		client::cache::download_limit_tests::random_stages_clean_failure_without_truncating_legacy_part_file; do \
		timeout 10s grep -Fqx "$$test: test" <<<"$$listed"; \
		timeout 300s $(CARGO) test -p forge --lib "$$test" -- --exact --test-threads=1; \
	done

forge-client-direct-test:
	@set -eu; \
	listed="$$( timeout 300s $(CARGO) test -p forge --lib -- --list )"; \
	timeout 10s test -n "$$listed"; \
	count="$$( timeout 10s grep -Ec '^client::tests::[^:]+: test$$' <<<"$$listed" )"; \
	timeout 10s test "$$count" = 211; \
	timeout 1200s $(CARGO) test -p forge --lib "client::tests::" -- --test-threads=1

forge-database-adapter-test:
	@set -eu; \
	listed="$$( timeout 300s $(CARGO) test -p forge --lib -- --list )"; \
	layout_count="$$( timeout 10s grep -c '^db::layout::test::.*: test$$' <<<"$$listed" )"; \
	meta_count="$$( timeout 10s grep -c '^db::meta::test::.*: test$$' <<<"$$listed" )"; \
	timeout 10s test "$$layout_count" = 11; \
	timeout 10s test "$$meta_count" = 10; \
	timeout 900s $(CARGO) test -p forge --lib "db::layout::test::" -- --test-threads=1; \
	timeout 900s $(CARGO) test -p forge --lib "db::meta::test::" -- --test-threads=1

forge-read-only-substrate-test:
	@set -eu; \
	listed="$$( timeout 180s $(CARGO) test -p forge --lib -- --list )"; \
	test -n "$$listed"; \
	for test in \
		db::read_only::tests::deserialized_adapters_query_exact_state_meta_and_selected_layout_without_mutation \
		db::read_only::tests::authorizer_denies_writes_and_functions_and_connection_remains_clean \
		db::read_only::tests::opcode_and_deadline_interruptions_are_deterministic_and_handlers_are_cleared \
		db::read_only::tests::connection_mutex_wait_uses_the_same_finite_query_deadline \
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
		timeout 10s grep -Fqx "$$test: test" <<<"$$listed"; \
		timeout 180s $(CARGO) test -p forge --lib "$$test" -- --exact --test-threads=1; \
	done

forge-read-only-client-test:
	@set -eu; \
	listed="$$( timeout 180s $(CARGO) test -p forge --lib -- --list )"; \
	count="$$( timeout 10s grep -c '^client::read_only::tests::.*: test$$' <<<"$$listed" )"; \
	test "$$count" = 12; \
	timeout 600s $(CARGO) test -p forge --lib "client::read_only::tests::" -- --test-threads=1

forge-transition-journal-contract-test:
	@set -eu; \
	listed="$$( timeout 180s $(CARGO) test -p forge --lib -- --list )"; \
	test -n "$$listed"; \
	for test in \
		transition_journal::tests::canonical_round_trip_covers_every_phase \
		transition_journal::tests::canonical_v1_full_frame_and_json_order_are_locked_by_golden_bytes \
		transition_journal::tests::exact_record_limit_and_n_plus_one_are_distinguished \
		transition_journal::tests::checksum_covers_header_fields_and_payload \
		transition_journal::tests::unknown_frame_and_payload_versions_are_rejected \
		transition_journal::tests::unknown_phase_field_and_duplicate_field_are_rejected \
		transition_journal::tests::reboot_identity_schema_is_required_strict_and_has_no_v1_aliases \
		transition_journal::tests::record_trailing_bytes_and_noncanonical_json_are_rejected \
		transition_journal::tests::bounded_identifiers_and_obvious_semantic_mismatches_fail_closed \
		transition_journal::tests::generated_tree_tokens_are_canonical_and_distinct \
		transition_journal::tests::preparing_constructor_derives_wire_fields_and_rejects_invalid_operation_layouts \
		transition_journal::tests::preparing_pins_epoch_tokens_runtime_witnesses_and_operation_relationships_fail_closed \
		transition_journal::tests::disabled_forward_phases_and_rollback_plan_placement_fail_closed \
		transition_journal::tests::all_operations_and_forward_option_paths_have_exact_successors \
		transition_journal::tests::rollback_is_available_until_commit_except_after_verified_boot_sync \
		transition_journal::tests::conditional_advance_rejects_generation_transition_phase_and_layout_changes \
		transition_journal::tests::rollback_plan_requirements_are_derived_from_source_and_observation \
		transition_journal::tests::rollback_candidate_disposition_and_external_effects_are_derived \
		transition_journal::tests::rollback_recovery_order_and_status_updates_are_exact \
		transition_journal::tests::ambiguous_boot_repair_is_terminal_unverified_and_nondeletable \
		transition_journal::tests::shared_transition_id_is_the_only_journal_correlation_encoding; do \
		timeout 10s grep -Fqx "$$test: test" <<<"$$listed"; \
		timeout 180s $(CARGO) test -p forge --lib "$$test" -- --exact --test-threads=1; \
	done

forge-transition-runtime-evidence-test:
	@set -eu; \
	listed="$$( timeout 180s $(CARGO) test -p forge --lib -- --list )"; \
	test -n "$$listed"; \
	for test in \
		transition_journal::tests::runtime_epoch_capture_is_canonical_stable_and_current \
		transition_journal::tests::runtime_tree_identity_capture_binds_the_exact_directory_and_mount \
		transition_journal::tests::runtime_tree_identity_rejects_a_non_directory_descriptor \
		transition_journal::tests::boot_id_and_mount_namespace_parsers_reject_untrusted_or_noncanonical_inputs \
		transition_journal::tests::fdinfo_mount_id_parser_is_bounded_canonical_and_unique; do \
		timeout 10s grep -Fqx "$$test: test" <<<"$$listed"; \
		timeout 180s $(CARGO) test -p forge --lib "$$test" -- --exact --test-threads=1; \
	done

forge-transition-journal-test:
	@set -eu; \
	listed="$$( timeout 180s $(CARGO) test -p forge --lib -- --list )"; \
	count="$$( timeout 10s grep -c '^transition_journal::tests::.*: test$$' <<<"$$listed" )"; \
	timeout 10s test "$$count" = 56; \
	timeout 900s $(CARGO) test -p forge --lib "transition_journal::tests::" -- --test-threads=1

stone-read-test:
	@set -eu; \
	listed="$$( timeout 300s $(CARGO) test -p stone --all-features --lib -- --list )"; \
	timeout 10s test -n "$$listed"; \
	count="$$( timeout 10s grep -c '^read::test::.*: test$$' <<<"$$listed" )"; \
	timeout 10s test "$$count" = 20; \
	for test in \
		read::test::read_header \
		read::test::read_bash_completion \
		read::test::payload_count_limit_accepts_n_and_rejects_n_plus_one \
		read::test::record_count_limit_accepts_n_and_rejects_n_plus_one \
		read::test::record_byte_limit_accepts_n_and_rejects_n_plus_one_before_allocation \
		read::test::stored_and_plain_payload_limits_accept_n_and_reject_n_plus_one \
		read::test::aggregate_stored_plain_record_and_count_limits_are_enforced_at_n_plus_one \
		read::test::zstd_plain_size_must_match_exact_expansion \
		read::test::malformed_metadata_and_layout_lengths_are_rejected_without_panics \
		read::test::malformed_or_out_of_bounds_content_indices_are_rejected \
		read::test::huge_declared_attribute_length_fails_before_allocation \
		read::test::exact_length_strings_reject_truncation \
		read::test::record_payload_trailing_bytes_are_rejected_without_panicking \
		read::test::declared_payload_header_is_never_silently_truncated \
		read::test::multiple_content_payloads_are_rejected \
		read::test::unknown_payloads_are_skipped_with_exact_checksum_validation \
		read::test::trailing_bytes_and_truncated_payload_are_rejected \
		read::test::huge_sparse_archive_and_limit_arithmetic_fail_before_offset_seeks \
		read::test::content_output_never_exceeds_declared_plain_size \
		read::test::ffi_content_stream_is_bounded_and_validates_checksum_before_eof; do \
		timeout 10s grep -Fqx "$$test: test" <<<"$$listed"; \
		timeout 300s $(CARGO) test -p stone --all-features --lib "$$test" -- --exact --test-threads=1; \
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

stone-recipe-derivation-validation-test:
	@set -eu; \
	listed="$$( timeout 300s $(CARGO) test -p stone_recipe --lib -- --list )"; \
	count="$$( timeout 10s grep -c '^derivation::tests::.*: test$$' <<<"$$listed" )"; \
	test "$$count" = 66; \
	timeout 900s $(CARGO) test -p stone_recipe --lib "derivation::tests::" -- --test-threads=1

stone-recipe-build-lock-test:
	@set -eu; \
	listed="$$( timeout 300s $(CARGO) test -p stone_recipe --lib -- --list )"; \
	count="$$( timeout 10s grep -c '^derivation::build_lock::tests::.*: test$$' <<<"$$listed" )"; \
	timeout 10s test "$$count" = 13; \
	timeout 900s $(CARGO) test -p stone_recipe --lib "derivation::build_lock::tests::" -- --test-threads=1

stone-recipe-package-validation-test:
	@set -eu; \
	listed="$$( timeout 300s $(CARGO) test -p stone_recipe --lib -- --list )"; \
	count="$$( timeout 10s grep -c '^package::tests::.*: test$$' <<<"$$listed" )"; \
	test "$$count" = 25; \
	timeout 900s $(CARGO) test -p stone_recipe --lib "package::tests::" -- --test-threads=1

stone-recipe-build-policy-validation-test:
	@set -eu; \
	listed="$$( timeout 300s $(CARGO) test -p stone_recipe --lib -- --list )"; \
	count="$$( timeout 10s grep -c '^build_policy::tests::.*: test$$' <<<"$$listed" )"; \
	test "$$count" = 9; \
	timeout 900s $(CARGO) test -p stone_recipe --lib "build_policy::tests::" -- --test-threads=1

stone-recipe-build-policy-contract-test:
	@set -eu; \
	listed="$$( timeout 300s $(CARGO) test -p stone_recipe --test build_policy -- --list )"; \
	timeout 10s test -n "$$listed"; \
	count="$$( timeout 10s grep -c '^[^:][^:]*: test$$' <<<"$$listed" )"; \
	timeout 10s test "$$count" = 32; \
	timeout 900s $(CARGO) test -p stone_recipe --test build_policy -- --test-threads=1

stone-recipe-build-policy-patch-test:
	@set -eu; \
	listed="$$( timeout 300s $(CARGO) test -p stone_recipe --test build_policy_patch -- --list )"; \
	timeout 10s test -n "$$listed"; \
	count="$$( timeout 10s grep -c '^[^:][^:]*: test$$' <<<"$$listed" )"; \
	timeout 10s test "$$count" = 7; \
	timeout 900s $(CARGO) test -p stone_recipe --test build_policy_patch -- --test-threads=1

tools-buildinfo-semantic-fingerprint-test:
	@set -eu; \
	listed="$$( timeout 300s $(CARGO) test -p tools_buildinfo --test semantic_fingerprint -- --list )"; \
	timeout 10s test -n "$$listed"; \
	count="$$( timeout 10s grep -c '^[^:][^:]*: test$$' <<<"$$listed" )"; \
	timeout 10s test "$$count" = 29; \
	timeout 900s $(CARGO) test -p tools_buildinfo --test semantic_fingerprint -- --test-threads=1

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

container-mount-boundary-test:
	@set -eu; \
	listed="$$( timeout 180s $(CARGO) test -p container --lib -- --list )"; \
	test -n "$$listed"; \
	for test in \
		tests::anchored_constructor_owns_a_cloexec_opath_directory_duplicate \
		tests::anchored_constructor_rejects_every_non_opath_or_non_directory_descriptor \
		tests::anchored_bind_source_is_pinned_before_clone_and_survives_path_substitution \
		tests::anchored_mount_targets_must_preexist_and_reject_symlink_traversal \
		tests::anchored_mount_target_normalization_rejects_escape_and_root_aliases \
		tests::anchored_mount_topology_rejects_duplicate_and_nested_targets \
		tests::anchored_execution_rejects_pathname_and_special_file_bind_sources_before_clone \
		tests::anchored_bind_apis_require_absolute_source_and_guest_paths \
		tests::sealed_resolver_file_has_exact_metadata_seals_and_cleanup \
		tests::resolver_stability_witness_detects_content_metadata_change \
		tests::anchored_resolver_target_uses_the_descriptor_not_the_replaced_label \
		tests::anchored_resolver_rejects_fifo_and_device_targets_without_opening_data \
		tests::default_policy_preserves_historical_mounts \
		tests::bounded_tmpfs_limits_reject_each_zero_ceiling \
		tests::bounded_tmpfs_emits_exact_mount_and_fsconfig_values \
		tests::bounded_tmpfs_verification_reports_fstatfs_failure \
		tests::bounded_tmpfs_readback_rejects_wrong_filesystem_magic \
		tests::bounded_tmpfs_readback_rejects_representation_and_multiplication_overflow \
		tests::bounded_tmpfs_readback_reports_size_and_inode_normalization_exactly \
		tests::pseudo_mount_targets_are_prepared_before_a_root_can_be_sealed \
		tests::disabled_policy_produces_no_mount_decisions \
		tests::policy_maps_to_ordered_mount_decisions \
		tests::deterministic_loopback_policy_is_explicit \
		tests::read_only_root_reopens_only_explicit_read_write_binds \
		tests::bounded_tmpfs_on_a_read_only_root_enforces_exact_byte_and_inode_ceilings \
		tests::anchored_bounded_tmpfs_enforces_the_same_exact_ceilings \
		tests::non_page_aligned_bounded_tmpfs_is_rejected_on_path_activation \
		tests::non_page_aligned_bounded_tmpfs_is_rejected_on_anchored_activation \
		tests::minimal_dev_is_read_only_and_exact_on_the_path_activation \
		tests::minimal_dev_is_read_only_and_exact_on_anchored_activation \
		tests::read_only_root_is_enforced_by_the_live_kernel_mount_and_capability_paths \
		tests::anchored_root_path_substitution_cannot_redirect_payload \
		tests::anchored_root_relative_install_is_exact_writable_exception_after_label_substitution \
		tests::anchored_payload_error_transport_is_bounded_and_completes \
		tests::anchored_root_clone_excludes_undeclared_nested_mounts \
		tests::anchored_directory_bind_excludes_undeclared_nested_mounts \
		tests::minimal_dev_has_an_exact_non_entropy_device_set \
		tests::minimal_dev_accepts_only_exact_linux_character_device_identities \
		tests::special_file_bind_gets_a_file_mountpoint; do \
		timeout 10s grep -Fqx "$$test: test" <<<"$$listed"; \
		timeout 180s $(CARGO) test -p container --lib "$$test" -- --exact --test-threads=1; \
	done

# The complete lane intentionally retains five socket-diagnostic tests which
# this local sandbox denies with EPERM. Run every other direct root test here,
# one-by-one, so the extraction is still covered without converting a denial
# into a false skip or weakening the tests themselves.
container-root-host-safe-test:
	@set -eu; \
	listed="$$( timeout 180s $(CARGO) test -p container --lib -- --list )"; \
	count="$$( timeout 10s grep -c '^tests::.*: test$$' <<<"$$listed" )"; \
	timeout 10s test "$$count" = 64; \
	ran=0; \
	for test in $$( timeout 10s grep '^tests::.*: test$$' <<<"$$listed" | timeout 10s sed 's/: test$$//' ); do \
		case "$$test" in \
			tests::child_error_read_does_not_wait_for_a_leaked_descendant_socket|\
			tests::raw_clone_child_panic_is_contained_and_reported|\
			tests::synchronization_socket_blocks_child_until_one_atomic_release|\
			tests::synchronization_socket_is_close_on_exec_blocking_and_nosignal|\
			tests::synchronization_socket_preserves_the_maximum_diagnostic_packet) continue ;; \
		esac; \
		timeout 180s $(CARGO) test -p container --lib "$$test" -- --exact --test-threads=1; \
		ran=$$((ran + 1)); \
	done; \
	timeout 10s test "$$ran" = 59

mason-package-collect-test:
	@set -eu; \
	listed="$$( timeout 300s $(CARGO) test -p mason --lib -- --list )"; \
	count="$$( timeout 10s grep -c '^package::collect::tests::.*: test$$' <<<"$$listed" )"; \
	test "$$count" = 27; \
	timeout 900s $(CARGO) test -p mason --lib "package::collect::tests::" -- --test-threads=1

mason-package-collect-transaction-test:
	@set -eu; \
	listed="$$( timeout 300s $(CARGO) test -p mason --lib -- --list )"; \
	mutation_count="$$( timeout 10s grep -c '^package::collect::mutation::tests::.*: test$$' <<<"$$listed" )"; \
	publication_count="$$( timeout 10s grep -c '^package::collect::publication::tests::.*: test$$' <<<"$$listed" )"; \
	timeout 10s test "$$mutation_count" = 11; \
	timeout 10s test "$$publication_count" = 11; \
	timeout 900s $(CARGO) test -p mason --lib "package::collect::mutation::tests::" -- --test-threads=1; \
	timeout 900s $(CARGO) test -p mason --lib "package::collect::publication::tests::" -- --test-threads=1

mason-analysis-handler-test:
	@set -eu; \
	listed="$$( timeout 300s $(CARGO) test -p mason --lib -- --list )"; \
	count="$$( timeout 10s grep -c '^package::analysis::handler::tests::.*: test$$' <<<"$$listed" )"; \
	timeout 10s test "$$count" = 30; \
	timeout 900s $(CARGO) test -p mason --lib "package::analysis::handler::tests::" -- --test-threads=1

mason-emit-test:
	@set -eu; \
	listed="$$( timeout 300s $(CARGO) test -p mason --lib -- --list )"; \
	timeout 10s test -n "$$listed"; \
	count="$$( timeout 10s grep -c '^package::emit::verification_tests::.*: test$$' <<<"$$listed" )"; \
	timeout 10s test "$$count" = 18; \
	for test in \
		package::emit::verification_tests::artifact_sink_publishes_only_the_exact_read_only_set \
		package::emit::verification_tests::real_contentful_stone_emission_survives_transactional_staging \
		package::emit::verification_tests::bounded_artifact_failure_removes_every_owned_name \
		package::emit::verification_tests::bounded_artifact_seek_accepts_exact_limit_and_rejects_limit_plus_one \
		package::emit::verification_tests::publication_collision_after_one_rename_rolls_back_owned_final \
		package::emit::verification_tests::staged_same_size_mutation_immediately_before_rename_is_rejected \
		package::emit::verification_tests::final_inode_swap_is_detected_without_deleting_the_replacement \
		package::emit::verification_tests::same_inode_truncation_after_publication_is_detected_and_removed \
		package::emit::verification_tests::same_inode_same_size_overwrite_after_publication_is_detected_and_removed \
		package::emit::verification_tests::replaced_public_root_is_rejected_and_only_the_pinned_root_is_cleaned \
		package::emit::verification_tests::preexisting_artifact_root_entries_are_never_reused_or_removed \
		package::emit::verification_tests::emitter_rejects_a_path_replaced_after_collection \
		package::emit::verification_tests::duplicate_normalized_layout_targets_are_rejected_before_emission \
		package::emit::verification_tests::reserved_system_metadata_target_is_rejected_before_artifact_sink_creation \
		package::emit::verification_tests::near_system_metadata_names_remain_legal_for_mason_layouts \
		package::emit::verification_tests::non_directory_normalized_ancestor_is_rejected_before_emission \
		package::emit::verification_tests::directory_normalized_ancestor_may_own_descendants \
		package::emit::verification_tests::content_emission_preserves_the_primary_writer_error; do \
		timeout 10s grep -Fqx "$$test: test" <<<"$$listed"; \
		timeout 300s $(CARGO) test -p mason --lib "$$test" -- --exact --test-threads=1; \
	done

mason-archive-test:
	@set -eu; \
	listed="$$( timeout 300s $(CARGO) test -p mason --lib -- --list )"; \
	timeout 10s test -n "$$listed"; \
	count="$$( timeout 10s grep -c '^archive::tests::.*: test$$' <<<"$$listed" )"; \
	timeout 10s test "$$count" = 24; \
	timeout 900s $(CARGO) test -p mason --lib "archive::tests::" -- --test-threads=1

mason-package-publication-test:
	@set -eu; \
	listed="$$( timeout 300s $(CARGO) test -p mason --lib -- --list )"; \
	timeout 10s test -n "$$listed"; \
	count="$$( timeout 10s grep -c '^package::tests::.*: test$$' <<<"$$listed" )"; \
	timeout 10s test "$$count" = 42; \
	timeout 900s $(CARGO) test -p mason --lib "package::tests::" -- --test-threads=1

mason-git-materialization-test:
	@set -eu; \
	listed="$$( timeout 300s $(CARGO) test -p mason --lib -- --list )"; \
	timeout 10s test -n "$$listed"; \
	count="$$( timeout 10s grep -c '^upstream::git::materialization::tests::.*: test$$' <<<"$$listed" )"; \
	timeout 10s test "$$count" = 20; \
	timeout 900s $(CARGO) test -p mason --lib "upstream::git::materialization::tests::" -- --test-threads=1

mason-paths-test:
	@set -eu; \
	listed="$$( timeout 300s $(CARGO) test -p mason --lib -- --list )"; \
	timeout 10s test -n "$$listed"; \
	count="$$( timeout 10s grep -c '^paths::tests::.*: test$$' <<<"$$listed" )"; \
	timeout 10s test "$$count" = 25; \
	timeout 900s $(CARGO) test -p mason --lib "paths::tests::" -- --test-threads=1

mason-executor-test:
	@set -eu; \
	listed="$$( timeout 300s $(CARGO) test -p mason --lib -- --list )"; \
	timeout 10s test -n "$$listed"; \
	count="$$( timeout 10s grep -c '^executor::tests::.*: test$$' <<<"$$listed" )"; \
	timeout 10s test "$$count" = 25; \
	timeout 900s $(CARGO) test -p mason --lib "executor::tests::" -- --test-threads=1

mason-build-context-test:
	@set -eu; \
	listed="$$( timeout 300s $(CARGO) test -p mason --lib -- --list )"; \
	timeout 10s test -n "$$listed"; \
	count="$$( timeout 10s grep -c '^build::context::tests::.*: test$$' <<<"$$listed" )"; \
	timeout 10s test "$$count" = 19; \
	timeout 900s $(CARGO) test -p mason --lib "build::context::tests::" -- --test-threads=1

mason-recipe-explanation-test:
	@set -eu; \
	listed="$$( timeout 300s $(CARGO) test -p mason --lib -- --list )"; \
	timeout 10s test -n "$$listed"; \
	count="$$( timeout 10s grep -c '^cli::recipe::explanation::tests::.*: test$$' <<<"$$listed" )"; \
	timeout 10s test "$$count" = 2; \
	timeout 900s $(CARGO) test -p mason --lib "cli::recipe::explanation::tests::" -- --test-threads=1

mason-upstream-git-cache-test:
	@set -eu; \
	listed="$$( timeout 300s $(CARGO) test -p mason --lib -- --list )"; \
	timeout 10s test -n "$$listed"; \
	count="$$( timeout 10s grep -c '^upstream::git::tests::.*: test$$' <<<"$$listed" )"; \
	timeout 10s test "$$count" = 13; \
	timeout 900s $(CARGO) test -p mason --lib "upstream::git::tests::" -- --test-threads=1

mason-build-root-test:
	@set -eu; \
	listed="$$( timeout 300s $(CARGO) test -p mason --lib -- --list )"; \
	timeout 10s test -n "$$listed"; \
	count="$$( timeout 10s grep -c '^build::root::tests::.*: test$$' <<<"$$listed" )"; \
	timeout 10s test "$$count" = 12; \
	timeout 900s $(CARGO) test -p mason --lib "build::root::tests::" -- --test-threads=1

mason-profile-test:
	@set -eu; \
	listed="$$( timeout 300s $(CARGO) test -p mason --lib -- --list )"; \
	timeout 10s test -n "$$listed"; \
	count="$$( timeout 10s grep -c '^profile::tests::.*: test$$' <<<"$$listed" )"; \
	timeout 10s test "$$count" = 9; \
	timeout 900s $(CARGO) test -p mason --lib "profile::tests::" -- --test-threads=1

mason-planner-bootstrap-test:
	@set -eu; \
	listed="$$( timeout 300s $(CARGO) test -p mason --lib -- --list )"; \
	timeout 10s test -n "$$listed"; \
	count="$$( timeout 10s grep -c '^planner::hermetic_tests::bootstrap::.*: test$$' <<<"$$listed" )"; \
	timeout 10s test "$$count" = 11; \
	timeout 1200s $(CARGO) test -p mason --lib "planner::hermetic_tests::bootstrap::" -- --test-threads=1

mason-policy-test:
	@set -eu; \
	listed="$$( timeout 300s $(CARGO) test -p mason --lib -- --list )"; \
	timeout 10s test -n "$$listed"; \
	count="$$( timeout 10s grep -c '^policy::tests::.*: test$$' <<<"$$listed" )"; \
	timeout 10s test "$$count" = 11; \
	timeout 900s $(CARGO) test -p mason --lib "policy::tests::" -- --test-threads=1

config-gluon-store-test:
	@set -eu; \
	listed="$$( timeout 300s $(CARGO) test -p config --lib -- --list )"; \
	count="$$( timeout 10s grep -c '^gluon::tests::.*: test$$' <<<"$$listed" )"; \
	timeout 10s test "$$count" = 28; \
	timeout 900s $(CARGO) test -p config --lib "gluon::tests::" -- --test-threads=1

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

gitwrap-all-test:
	@set -eu; \
	listed="$$( timeout 300s $(CARGO) test -p gitwrap --lib -- --list )"; \
	count="$$( timeout 10s grep -c '^tests::.*: test$$' <<<"$$listed" )"; \
	timeout 10s test "$$count" = 32; \
	timeout 900s $(CARGO) test -p gitwrap --lib "tests::" -- --test-threads=1

forge-repository-manager-test:
	@set -eu; \
	listed="$$( timeout 300s $(CARGO) test -p forge --lib -- --list )"; \
	count="$$( timeout 10s grep -c '^repository::manager::tests::.*: test$$' <<<"$$listed" )"; \
	test "$$count" = 19; \
	timeout 900s $(CARGO) test -p forge --lib "repository::manager::tests::" -- --test-threads=1

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
