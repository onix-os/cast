SHELL := /bin/bash

TOP_DIR := $(CURDIR)
CARGO ?= cargo
MODE ?= onboarding
PREFIX ?= $(HOME)/.local
BIN_DIR ?= $(PREFIX)/bin
DATA_DIR ?= $(PREFIX)/share
CONFIG_DIR ?= $(HOME)/.config
LICENSE_DIR ?= $(TOP_DIR)/target/license-list-data
EXAMPLE ?= read
STONE ?= $(TOP_DIR)/tests/fixtures/bash-completion-2.11-1-1-x86_64.stone
REQUIRE_EXECUTION ?= 0
FIXTURE ?= all
EXECUTION_FIXTURE_NAMES := autotools cargo cargo-vendored cmake custom daemon-generated factory-override hooks-patch meson split
VALID_EXECUTION_FIXTURES := all $(EXECUTION_FIXTURE_NAMES)
# Capture the literal command-line value once. A recursive make variable such
# as '$$(shell ...)' must never be re-expanded into a bootstrap shell recipe.
FIXTURE_SELECTION := $(strip $(value FIXTURE))
VALID_FIXTURE_SELECTION := $(if $(word 2,$(FIXTURE_SELECTION)),,$(filter $(VALID_EXECUTION_FIXTURES),$(FIXTURE_SELECTION)))
EXECUTION_REQUIREMENT := $(strip $(value REQUIRE_EXECUTION))
VALID_EXECUTION_REQUIREMENT := $(if $(word 2,$(EXECUTION_REQUIREMENT)),,$(filter 0 1,$(EXECUTION_REQUIREMENT)))
BOOTSTRAP_TMP_DIR := $(TOP_DIR)/target/bootstrap-fixtures/tmp
BOOTSTRAP_PACKAGE_STORE := $(TOP_DIR)/target/bootstrap-fixtures/packages

.DEFAULT_GOAL := cast

.PHONY: build cast get-started licenses fix lint test forge-transition-identity-test forge-previous-tree-move-test forge-archived-candidate-move-test forge-frozen-normalization-test forge-frozen-publication-test forge-frozen-discard-test cache-clean-test examples execution-fixtures delegated-execution-fixtures delegated-fixture-runner-test bootstrap-fixtures bootstrap-fixtures-prepare bootstrap-fixtures-offline bootstrap-fixtures-tmp bootstrap-fixture-selection bootstrap-execution-requirement fixtures-ci fixture-sources fixture-sources-check check fmt clean \
	binary-layout product-names config-formats config-formats-test migrate migrate-redo \
	libstone help

build:
	@$(CARGO) build --workspace

cast:
	@$(CARGO) build --profile $(MODE) -p cast

get-started: cast licenses
	@set -eu; \
	echo; \
	echo "Installing cast to $(BIN_DIR)..."; \
	install -d "$(BIN_DIR)"; \
	install -m 755 "$(TOP_DIR)/target/$(MODE)/cast" "$(BIN_DIR)/cast"; \
	rm -rf "$(DATA_DIR)/cast"; \
	install -d "$(DATA_DIR)/cast/licenses" "$(CONFIG_DIR)/cast"; \
	cp -R "$(TOP_DIR)/crates/mason/data/policy" "$(DATA_DIR)/cast/"; \
	cp "$(LICENSE_DIR)/text/"* "$(DATA_DIR)/cast/licenses/"; \
	cp -R "$(TOP_DIR)/crates/mason/data/profile.d" "$(CONFIG_DIR)/cast/"; \
	echo; \
	echo "Installed files:"; \
	ls -hlF "$(BIN_DIR)/cast" "$(DATA_DIR)/cast" "$(CONFIG_DIR)/cast"; \
	echo; \
	case ":$$PATH:" in \
		*:"$(BIN_DIR)":*) echo "$(BIN_DIR) is already in PATH." ;; \
		*) echo "$(BIN_DIR) is not in PATH yet; add it before running the tools." ;; \
	esac; \
	echo; \
	echo "Cast documentation lives at https://github.com/onix-os/os-tools"

licenses:
	@"$(TOP_DIR)/misc/scripts/fetch-licenses.sh" "$(LICENSE_DIR)"

fix:
	@echo "Applying clippy fixes..."
	@$(CARGO) clippy --fix --allow-dirty --allow-staged --workspace -- --no-deps
	@echo "Applying cargo fmt..."
	@$(CARGO) fmt --all
	@echo "Fixing typos..."
	@typos -w --exclude target/license-list-data/

lint: binary-layout product-names config-formats
	@echo "Running clippy..."
	@$(CARGO) clippy --workspace -- --no-deps
	@echo "Running clippy on the feature-gated harness-free cache-clean proof..."
	@$(CARGO) clippy -p mason --features cache-clean-test-support \
		--test cache_clean -- --no-deps
	@echo "Running clippy on the feature-gated harness-free Mason fixture..."
	@$(CARGO) clippy -p mason --features delegated-fixture-test-support \
		--test delegated_execution_fixture -- --no-deps
	@echo "Running cargo fmt..."
	@$(CARGO) fmt --all -- --check
	@echo "Checking for typos..."
	@typos --exclude target/license-list-data/

config-formats:
	@"$(TOP_DIR)/misc/scripts/check-config-formats.sh"

config-formats-test:
	@"$(TOP_DIR)/misc/scripts/test-check-config-formats.sh"

binary-layout:
	@"$(TOP_DIR)/misc/scripts/check-binary-layout.sh"

product-names:
	@"$(TOP_DIR)/misc/scripts/check-product-names.sh"

# Container activation uses fork-like namespace creation. Keep each libtest
# process to one active test worker; production single-task behavior is proved
# separately by harness-free container and delegated Mason integration targets.
test: lint config-formats-test delegated-fixture-runner-test cache-clean-test
	@echo "Running tests in all packages..."
	@$(CARGO) test --all --no-fail-fast -- --test-threads=1

forge-transition-identity-test:
	@set -eu; \
	listed="$$( $(CARGO) test -p forge --lib -- --list )"; \
	for test in \
		client::tests::stateful_tree_tokens_follow_their_logical_trees_through_exchange_and_archive \
		client::tests::retained_exchange_adopts_applied_forward_and_reverse_moves_when_the_syscall_reports_error \
		client::tests::retained_exchange_error_before_rename_preserves_both_exact_names \
		client::tests::retained_exchange_parent_replacement_is_rejected_before_the_syscall \
		client::tests::retained_exchange_child_substitution_is_rejected_before_the_syscall \
		client::tests::retained_exchange_post_move_faults_run_the_swapped_recovery_path \
		client::tests::retained_reverse_exchange_post_move_faults_finish_without_a_second_exchange \
		client::tests::recovery_never_recreates_a_missing_candidate_tree_marker \
		client::tests::recovery_rejects_same_content_marker_name_substitution_without_repair \
		client::tests::recovery_rejects_whole_directory_same_token_substitution_without_exchange \
		client::tests::missing_live_usr_between_identity_check_and_exchange_is_never_recreated \
		client::tests::unresolved_journal_evidence_blocks_marker_publication_before_activation \
		client::tests::orphan_transition_row_blocks_marker_publication_before_activation \
		client::tests::state_creation_records_and_exports_the_generated_snapshot \
		client::tests::two_failed_active_state_reblits_use_unique_non_state_quarantines \
		client::tests::quarantine_durability_faults_never_invalidate_the_fresh_candidate \
		client::tests::single_quarantine_durability_fault_is_resumed_before_invalidation \
		client::tests::quarantine_is_revalidated_after_the_invalidation_checkpoint \
		client::tests::deterministic_quarantine_name_collision_preserves_foreign_entry_and_database_row \
		client::tests::empty_deterministic_quarantine_collision_is_never_adopted \
		client::tests::quarantine_slot_creation_rejects_replacement_before_retention \
		client::tests::previous_archive_never_replaces_a_racing_empty_destination \
		client::tests::previous_restore_never_replaces_a_racing_empty_staging_destination \
		client::tests::first_install_synthesizes_syncs_marks_and_exchanges_an_empty_previous_usr \
		client::tests::failed_first_install_can_retry_the_exact_marker_only_previous_baseline \
		client::tests::first_install_marker_retry_rejects_marker_plus_foreign_content_unchanged \
		client::tests::first_install_rejects_a_hostile_live_usr_symlink_unchanged \
		client::tests::first_install_rejects_a_preexisting_nonempty_unmanaged_usr_unchanged \
		client::tests::first_install_rejects_a_racing_nonempty_usr_occupant_unchanged \
		client::tests::duplicate_permanent_tree_tokens_block_exchange_and_retain_both_trees; do \
		printf '%s\n' "$$listed" | grep -Fqx "$$test: test"; \
		$(CARGO) test -p forge --lib "$$test" -- --exact --test-threads=1; \
	done

forge-previous-tree-move-test:
	@set -eu; \
	listed="$$( $(CARGO) test -p forge --lib -- --list )"; \
	for test in \
		client::tests::retained_previous_moves_reconcile_before_and_after_rename_faults \
		client::tests::retained_previous_archive_applied_faults_resume_only_the_sync_suffix \
		client::tests::retained_previous_restore_applied_faults_resume_only_the_sync_suffix \
		client::tests::retained_previous_slot_creation_faults_retire_the_state_name_before_retry \
		client::tests::retained_previous_parking_scan_skips_occupied_non_mount_file_types \
		client::tests::retained_previous_parking_scan_uses_the_final_bounded_candidate \
		client::tests::retained_previous_parking_exhaustion_preserves_both_namespaces \
		client::tests::retained_previous_restore_retirement_faults_resume_without_a_second_rename \
		client::tests::retained_previous_moves_adopt_exact_pre_syscall_archive_and_restore_layouts \
		client::tests::retained_previous_slot_retirement_preserves_a_racing_replacement \
		client::tests::previous_archive_abort_retirement_faults_resume_in_production_recovery \
		client::tests::applied_previous_archive_and_restore_faults_use_full_client_suffix_routing \
		client::tests::retained_previous_moves_reject_roots_and_restore_staging_substitution \
		client::tests::fresh_identity_can_archive_after_a_complete_compensating_recovery \
		client::tests::retained_previous_archive_never_adopts_an_ambient_empty_state_slot \
		client::tests::retained_previous_archive_rejects_slot_replacement_before_retention \
		client::tests::retained_previous_archive_rejects_state_slot_parent_substitution_before_rename \
		client::tests::retained_previous_archive_rejects_same_token_child_substitution_before_rename; do \
		printf '%s\n' "$$listed" | grep -Fqx "$$test: test"; \
		$(CARGO) test -p forge --lib "$$test" -- --exact --test-threads=1; \
	done

forge-archived-candidate-move-test:
	@set -eu; \
	listed="$$( $(CARGO) test -p forge --lib -- --list )"; \
	for test in \
		client::tests::retained_archived_candidate_move_classifies_and_resumes_exact_layouts \
		client::tests::retained_archived_candidate_move_adopts_only_the_exact_exchanged_wrappers \
		client::tests::displaced_archived_candidate_slot_retirement_preserves_racing_occupants \
		client::tests::archived_activation_resumes_applied_staging_suffix_before_full_recovery \
		client::tests::archived_activation_resumes_applied_rearchive_suffix_during_full_recovery \
		client::tests::archived_activation_keeps_rearchive_preparation_sticky_through_presync_faults \
		client::tests::forged_exact_tree_marker_hardlink_is_not_adopted_in_process \
		client::tests::exact_parked_tree_marker_hardlink_is_reauthorized_after_reopen \
		client::tests::retained_archived_candidate_move_rejects_substituted_roots_as_ambiguous \
		client::tests::retained_archived_candidate_move_rejects_a_substituted_source_wrapper \
		client::tests::retained_archived_candidate_move_rejects_a_substituted_fixed_staging_wrapper \
		client::tests::displaced_archived_candidate_restore_faults_are_exactly_classified_and_resumable \
		client::tests::archived_candidate_marker_transfer_faults_resume_without_a_second_wrapper_exchange \
		client::tests::externally_premoved_slot_marker_fast_path_still_finishes_durability \
		client::tests::archived_candidate_rearchive_marker_preparation_faults_are_resumable \
		client::tests::archived_candidate_parking_scan_skips_every_foreign_occupant_kind \
		client::tests::archived_candidate_restore_preparation_uses_one_bounded_client_retry \
		client::tests::archived_candidate_marker_preparation_after_restore_uses_one_bounded_client_retry \
		client::tests::multiple_structural_reusable_state_slot_links_fail_closed \
		client::tests::repeated_archived_activations_reuse_wrapper_slots_beyond_the_scan_bound \
		client::tests::displaced_archived_candidate_retirement_without_an_attempt_fails_closed \
		client::tests::displaced_archived_candidate_retirement_resumes_without_a_second_move \
		client::tests::archived_retirement_suffix_failure_restores_the_slot_during_full_recovery \
		client::tests::quarantined_archived_candidate_retries_only_retirement_durability \
		client::tests::archived_activation_archive_failure_reverses_usr_and_rearchives_the_candidate \
		transition_identity::reusable_previous_slot::tests::reusable_slot_scan_skips_only_proven_foreign_errors; do \
		printf '%s\n' "$$listed" | grep -Fqx "$$test: test"; \
		$(CARGO) test -p forge --lib "$$test" -- --exact --test-threads=1; \
	done

forge-frozen-normalization-test:
	@set -eu; \
	listed="$$( $(CARGO) test -p forge --lib -- --list )"; \
	for test in \
		linux_fs::tests::interrupted_retry_limit_accepts_n_and_rejects_n_plus_one \
		linux_fs::tests::expired_retry_deadline_fails_before_another_syscall \
		linux_fs::tests::descriptor_times_update_the_retained_regular_inode_not_its_replacement \
		linux_fs::tests::descriptor_times_support_a_mode_zero_directory \
		linux_fs::tests::descriptor_times_update_a_symlink_without_touching_its_target \
		linux_fs::tests::descriptor_read_uses_the_retained_inode_and_preserves_atime \
		linux_fs::tests::descriptor_read_rejects_non_regular_capabilities \
		client::tests::frozen_copy_manifest_counts_output_inodes_and_enforces_exact_byte_limit \
		client::tests::frozen_capability_retry_timeout_remains_a_materialization_timeout \
		client::tests::independent_copy_rejects_length_changed_after_byte_preflight_before_creation \
		client::tests::frozen_normalization_handles_mode_zero_entries_and_never_follows_symlinks \
		client::tests::frozen_normalization_rejects_unplanned_missing_and_extra_entries \
		client::tests::frozen_normalization_directory_to_symlink_race_cannot_escape_root \
		client::tests::frozen_normalization_hardlink_substitution_is_rejected_before_mutation \
		client::tests::frozen_normalization_rejects_stage_root_name_substitution \
		client::tests::frozen_normalization_limits_accept_n_and_reject_n_plus_one \
		client::tests::frozen_normalization_runtime_walk_enforces_the_inode_limit \
		client::tests::frozen_normalization_rejects_regular_content_outside_the_declaration \
		client::tests::frozen_normalization_detects_same_inode_mutation_before_final_revalidation \
		client::tests::frozen_normalization_final_pass_detects_deep_content_mutation \
		client::tests::frozen_normalization_root_inventory_detects_post_digest_child_mutation \
		client::tests::frozen_normalization_detects_entry_added_after_final_inventory \
		client::tests::frozen_normalization_orders_non_utf8_names_as_raw_bytes \
		client::tests::frozen_normalization_rejects_cross_mount_entries_before_mutation \
		client::tests::frozen_normalization_rejects_access_acl_after_active_mode_change \
		client::tests::frozen_normalization_rejects_default_acl_after_active_mode_change \
		client::tests::frozen_root_normalizes_enforceable_metadata_in_canonical_order \
		client::tests::frozen_root_normalizes_and_discards_a_mode_zero_directory; do \
		printf '%s\n' "$$listed" | grep -Fqx "$$test: test"; \
		CAST_REQUIRE_POSIX_ACL_TESTS=1 $(CARGO) test -p forge --lib "$$test" -- --exact --test-threads=1; \
	done

forge-frozen-publication-test:
	@set -eu; \
	listed="$$( $(CARGO) test -p forge --lib -- --list )"; \
	for test in \
		linux_fs::tests::expired_rename_deadline_preserves_both_namespaces \
		linux_fs::tests::expired_sync_filesystem_deadline_fails_before_syncfs \
		client::tests::frozen_root_publication_never_replaces_an_existing_destination \
		client::tests::frozen_publication_adopts_an_applied_rename_even_when_the_syscall_reports_error \
		client::tests::frozen_publication_reconciles_an_applied_rename_after_the_work_deadline_expires \
		client::tests::frozen_private_directory_setup_failures_remove_the_exact_provisional_wrapper \
		client::tests::frozen_private_directory_normalizes_setgid_inherited_from_its_parent \
		client::tests::frozen_publication_error_before_rename_preserves_the_retained_stage_for_bounded_cleanup \
		client::tests::frozen_publication_reconciles_a_racing_destination_without_replacing_it \
		client::tests::frozen_publication_detects_destination_substitution_and_never_deletes_the_foreign_tree \
		client::tests::frozen_publication_rejects_a_foreign_stage_name_without_publishing_or_deleting_it \
		client::tests::frozen_destination_lock_serializes_cooperating_publishers_with_a_finite_wait \
		client::tests::failed_frozen_root_blit_never_publishes_or_leaves_a_reusable_stage; do \
		printf '%s\n' "$$listed" | grep -Fqx "$$test: test"; \
		$(CARGO) test -p forge --lib "$$test" -- --exact --test-threads=1; \
	done

forge-frozen-discard-test:
	@set -eu; \
	listed="$$( $(CARGO) test -p forge --lib -- --list )"; \
	for test in \
		client::tests::frozen_discard_widens_unreadable_roots_for_detach_and_private_cleanup \
		client::tests::frozen_discard_restores_mode_when_post_chmod_identity_inspection_fails \
		client::tests::frozen_discard_is_idempotent_when_the_public_root_is_absent \
		client::tests::frozen_discard_unlinks_symlinks_without_touching_external_targets \
		client::tests::frozen_discard_depth_limit_accepts_n_and_preserves_n_plus_one_privately \
		client::tests::frozen_discard_entry_limit_rejects_n_plus_one_before_deletion \
		client::tests::frozen_discard_rejects_non_directory_roots_without_creating_quarantine \
		client::tests::frozen_discard_rename_failure_removes_only_its_exact_empty_quarantine \
		client::tests::frozen_discard_adopts_an_applied_detach_even_when_the_syscall_reports_error \
		client::tests::frozen_discard_completes_after_an_applied_detach_reports_error \
		client::tests::frozen_discard_reconciles_an_applied_detach_after_the_work_deadline_expires \
		client::tests::frozen_discard_unlink_reconciles_applied_errors_and_bounded_interrupts \
		client::tests::frozen_discard_unlink_never_retries_against_a_foreign_replacement \
		client::tests::frozen_discard_preserves_a_racing_quarantine_collision_and_the_public_root \
		client::tests::frozen_discard_detects_source_substitution_without_deleting_the_foreign_tree \
		client::tests::frozen_discard_preserves_a_replaced_quarantine_wrapper_and_the_detached_root \
		client::tests::frozen_discard_uses_the_same_finite_parent_lock_as_publication \
		client::tests::frozen_discard_rejects_destination_parent_replacement_without_touching_either_tree \
		client::tests::frozen_root_normalizes_enforceable_metadata_in_canonical_order \
		client::tests::frozen_root_normalizes_and_discards_a_mode_zero_directory; do \
		printf '%s\n' "$$listed" | grep -Fqx "$$test: test"; \
		$(CARGO) test -p forge --lib "$$test" -- --exact --test-threads=1; \
	done

cache-clean-test:
	@echo "Running the harness-free descriptor-anchored cache-clean proof..."
	@CAST_CACHE_CLEAN_TEST_RUNNER=1 $(CARGO) test -p mason \
		--features cache-clean-test-support --test cache_clean

examples:
	@echo "Checking every Gluon package example through the public Cast CLI..."
	@$(CARGO) test -p cast --test gluon_examples -- --list | \
		grep -Fqx 'every_gluon_package_example_passes_the_public_cast_cli: test'
	@$(CARGO) test -p cast --test gluon_examples \
		every_gluon_package_example_passes_the_public_cast_cli -- \
		--exact --nocapture
	@echo "Freezing every Gluon package example through the hermetic planner..."
	@$(CARGO) test -p mason --lib -- --list | \
		grep -Fqx 'planner::hermetic_tests::checked_in_package_examples_freeze_hermetically_and_reuse_exact_build_locks: test'
	@$(CARGO) test -p mason --lib \
		planner::hermetic_tests::checked_in_package_examples_freeze_hermetically_and_reuse_exact_build_locks -- \
		--exact --nocapture
	@echo "Proving metadata-only providers fail before frozen execution..."
	@$(CARGO) test -p mason --lib -- --list | \
		grep -Fqx 'planner::hermetic_tests::checked_in_metadata_only_example_fails_closed_before_execution: test'
	@$(CARGO) test -p mason --lib \
		planner::hermetic_tests::checked_in_metadata_only_example_fails_closed_before_execution -- \
		--exact --nocapture

fixture-sources:
	@"$(TOP_DIR)/misc/scripts/build-execution-fixtures.sh"

fixture-sources-check:
	@"$(TOP_DIR)/misc/scripts/build-execution-fixtures.sh" --check

execution-fixtures: fixture-sources-check
	@echo "Checking locked offline execution-source fixtures..."
	@$(CARGO) test -p mason --lib \
		planner::hermetic_tests::offline_execution_fixture_archives_are_real_locked_and_complete -- \
		--exact --list | \
		grep -Fqx 'planner::hermetic_tests::offline_execution_fixture_archives_are_real_locked_and_complete: test'
	@$(CARGO) test -p mason --lib \
		planner::hermetic_tests::offline_execution_fixture_archives_are_real_locked_and_complete -- \
		--exact --nocapture
	@echo "Checking the declarative pinned Stone bootstrap manifest and index..."
	@$(CARGO) test -p mason --lib \
		planner::hermetic_tests::bootstrap::pinned_bootstrap_manifest_is_bounded_and_index_authoritative -- \
		--exact --list | \
		grep -Fqx 'planner::hermetic_tests::bootstrap::pinned_bootstrap_manifest_is_bounded_and_index_authoritative: test'
	@$(CARGO) test -p mason --lib \
		planner::hermetic_tests::bootstrap::pinned_bootstrap_manifest_is_bounded_and_index_authoritative -- \
		--exact --nocapture
	@echo "Resolving all ten execution fixtures against the pinned real Stone index..."
	@$(CARGO) test -p mason --lib \
		planner::hermetic_tests::bootstrap::all_execution_fixtures_resolve_exactly_the_pinned_real_stone_closure -- \
		--exact --list | \
		grep -Fqx 'planner::hermetic_tests::bootstrap::all_execution_fixtures_resolve_exactly_the_pinned_real_stone_closure: test'
	@$(CARGO) test -p mason --lib \
		planner::hermetic_tests::bootstrap::all_execution_fixtures_resolve_exactly_the_pinned_real_stone_closure -- \
		--exact --nocapture

bootstrap-fixtures-tmp:
	@set -eu; \
	tmpdir="$(BOOTSTRAP_TMP_DIR)"; \
	if [[ -L "$$tmpdir" || -e "$$tmpdir" && ! -d "$$tmpdir" ]]; then \
		echo "Refusing unsafe bootstrap TMPDIR: $$tmpdir" >&2; \
		exit 1; \
	fi; \
	if [[ -e "$$tmpdir" && ! -O "$$tmpdir" ]]; then \
		echo "Refusing bootstrap TMPDIR not owned by the current user: $$tmpdir" >&2; \
		exit 1; \
	fi; \
	install -d -m 700 "$$tmpdir"; \
	chmod 700 "$$tmpdir"; \
	[[ "$$(stat -c '%a' "$$tmpdir")" == 700 ]]

bootstrap-fixture-selection:
	@$(if $(VALID_FIXTURE_SELECTION),:,$(error FIXTURE must be exactly 'all' or one of: $(EXECUTION_FIXTURE_NAMES)))

bootstrap-execution-requirement:
	@$(if $(VALID_EXECUTION_REQUIREMENT),:,$(error REQUIRE_EXECUTION must be exactly '0' or '1'))

bootstrap-fixtures-prepare: bootstrap-fixtures-tmp
	@echo "Fetching and verifying the exact contentful Stone bootstrap closure..."
	@set -o pipefail; TMPDIR="$(BOOTSTRAP_TMP_DIR)" $(CARGO) test -p mason --lib \
		planner::hermetic_tests::bootstrap::fetch_pinned_bootstrap_package_files -- \
		--ignored --exact --list | \
		grep -Fqx 'planner::hermetic_tests::bootstrap::fetch_pinned_bootstrap_package_files: test'
	@TMPDIR="$(BOOTSTRAP_TMP_DIR)" $(CARGO) test -p mason --lib \
		planner::hermetic_tests::bootstrap::fetch_pinned_bootstrap_package_files -- \
		--ignored --exact --nocapture

bootstrap-fixtures-offline: bootstrap-fixture-selection bootstrap-execution-requirement bootstrap-fixtures-tmp
	@echo "Requiring the complete verified bootstrap store; this lane performs no downloads..."
	@echo "Materializing the complete closure as a production-format offline root mirror..."
	@set -o pipefail; TMPDIR="$(BOOTSTRAP_TMP_DIR)" $(CARGO) test -p mason --lib \
		planner::hermetic_tests::bootstrap::contentful_bootstrap_materializes_a_complete_offline_root_mirror -- \
		--ignored --exact --list | \
		grep -Fqx 'planner::hermetic_tests::bootstrap::contentful_bootstrap_materializes_a_complete_offline_root_mirror: test'
	@TMPDIR="$(BOOTSTRAP_TMP_DIR)" $(CARGO) test -p mason --lib \
		planner::hermetic_tests::bootstrap::contentful_bootstrap_materializes_a_complete_offline_root_mirror -- \
		--ignored --exact --nocapture
	@$(MAKE) --no-print-directory delegated-execution-fixtures \
		FIXTURE=$(FIXTURE_SELECTION) REQUIRE_EXECUTION=$(EXECUTION_REQUIREMENT)

delegated-execution-fixtures: bootstrap-fixture-selection bootstrap-execution-requirement bootstrap-fixtures-tmp
	@echo "Building, packaging, and reproducing fixture selection '$(FIXTURE_SELECTION)' in an explicit delegated unit..."
	@TMPDIR="$(BOOTSTRAP_TMP_DIR)" \
		CAST_BOOTSTRAP_PACKAGE_STORE="$(BOOTSTRAP_PACKAGE_STORE)" \
		CAST_REQUIRE_EXECUTION="$(EXECUTION_REQUIREMENT)" \
		CARGO="$(CARGO)" \
		"$(TOP_DIR)/misc/scripts/run-delegated-execution-fixture.sh" "$(FIXTURE_SELECTION)"

delegated-fixture-runner-test:
	@"$(TOP_DIR)/misc/scripts/test-run-delegated-execution-fixture.sh"

bootstrap-fixtures: bootstrap-fixture-selection bootstrap-execution-requirement bootstrap-fixtures-prepare
	@$(MAKE) --no-print-directory bootstrap-fixtures-offline \
		FIXTURE=$(FIXTURE_SELECTION) REQUIRE_EXECUTION=$(EXECUTION_REQUIREMENT)

fixtures-ci: execution-fixtures
	@$(MAKE) --no-print-directory bootstrap-fixtures-prepare
	@$(MAKE) --no-print-directory bootstrap-fixtures-offline REQUIRE_EXECUTION=1 FIXTURE=all

check:
	@$(CARGO) check --workspace --all-targets
	@$(CARGO) check -p mason --features cache-clean-test-support \
		--test cache_clean
	@$(CARGO) check -p mason --features delegated-fixture-test-support \
		--test delegated_execution_fixture

fmt:
	@$(CARGO) fmt --all

clean:
	@$(CARGO) clean

migrate:
	@set -eu; \
	for db in meta layout state; do \
		diesel \
			--config-file "$(TOP_DIR)/crates/forge/src/db/$$db/diesel.toml" \
			--database-url "sqlite://$(TOP_DIR)/crates/forge/src/db/$$db/test.db" \
			migration run; \
	done

migrate-redo:
	@set -eu; \
	for db in meta layout state; do \
		diesel \
			--config-file "$(TOP_DIR)/crates/forge/src/db/$$db/diesel.toml" \
			--database-url "sqlite://$(TOP_DIR)/crates/forge/src/db/$$db/test.db" \
			migration redo; \
	done

libstone:
	@set -eu; \
	output="$$(mktemp)"; \
	trap 'rm -f "$$output"' EXIT; \
	$(CARGO) build -p libstone --release; \
	clang "$(TOP_DIR)/crates/libstone/examples/$(EXAMPLE).c" \
		-o "$$output" \
		-I"$(TOP_DIR)/crates/libstone/src" \
		-lstone -L"$(TOP_DIR)/target/release" \
		-Wl,-rpath,"$(TOP_DIR)/target/release"; \
	if [[ "$${USE_VALGRIND:-0}" == 1 ]]; then \
		time valgrind --track-origins=yes "$$output" "$(STONE)"; \
	else \
		time "$$output" "$(STONE)"; \
	fi

help:
	@echo
	@echo "Usage: make [target]"
	@echo
	@echo "Available targets:"
	@echo "  build         Build the complete workspace"
	@echo "  cast          Build Cast with MODE=$(MODE) (default)"
	@echo "  get-started   Build and install Cast and its data"
	@echo "  test          Run lints and all workspace tests"
	@echo "  forge-transition-identity-test  Run focused durable /usr identity and recovery tests"
	@echo "  forge-previous-tree-move-test  Run retained previous-tree archive and restore tests"
	@echo "  forge-archived-candidate-move-test  Run retained archived-candidate move and recovery tests"
	@echo "  examples      Check, evaluate, freeze, and fail-close the Gluon examples"
	@echo "  execution-fixtures  Verify real offline source archives and Gluon locks"
	@echo "  delegated-execution-fixtures  Run selected contentful fixtures in a harness-free delegated unit"
	@echo "  delegated-fixture-runner-test  Test delegated-unit timeout and interruption cleanup"
	@echo "  bootstrap-fixtures  Prepare the pinned closure, then run the offline fixture lane"
	@echo "  bootstrap-fixtures-prepare  Fetch and verify the pinned 107-package Stone closure"
	@echo "  bootstrap-fixtures-offline  Build selected fixtures twice without downloading"
	@echo "                    Set FIXTURE=all (default) or one of the ten fixture names"
	@echo "                    Set REQUIRE_EXECUTION=1 to reject namespace-capability skips"
	@echo "  fixtures-ci    Required-capability ten-fixture execution and reproduction gate"
	@echo "  fixture-sources  Rebuild deterministic offline execution-source archives"
	@echo "  check         Check all workspace targets"
	@echo "  fix           Apply clippy, formatting, and typo fixes"
	@echo "  fmt           Format the workspace"
	@echo "  binary-layout  Require Cast to be the sole executable target"
	@echo "  product-names  Reject active references to retired product names"
	@echo "  config-formats  Reject YAML/KDL outside external-service interfaces"
	@echo "  config-formats-test  Test the configuration-format gate"
	@echo "  migrate       Apply all Forge database migrations"
	@echo "  migrate-redo  Reapply all Forge database migrations"
	@echo "  libstone      Build and run the C libstone example"
	@echo "  clean         Remove Cargo build artifacts"
	@echo
