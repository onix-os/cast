SHELL := bash

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
EXECUTION_FIXTURE_NAMES := autotools autotools-options cargo cargo-features cargo-vendored cmake custom daemon-generated desktop-integration external-test-vectors factory-override font-family generated-config generated-shell gettext-localization go-module header-only-library hooks-patch meson multiple-sources pgo-workload plugin-output post-install-smoke-test python-module relation-policy split system-integration-assets userspace-profile
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

include misc/make/hardening-tests.mk
include misc/make/boot-publication-receipt-head-tests.mk
include misc/make/boot-publication-receipt-state-tests.mk
include misc/make/transition-journal-bound-delete-tests.mk
include misc/make/transition-journal-delete-residue-recovery-tests.mk
include misc/make/private-device-tests.mk
include misc/make/private-device-service-tests.mk
include misc/make/host-storage-safety-tests.mk
include misc/make/linux-descriptor-boot-filesystem-tests.mk
include misc/make/linux-descriptor-boot-namespace-tests.mk
include misc/make/linux-descriptor-boot-namespace-production-tests.mk
include misc/make/linux-descriptor-devtmpfs-filesystem-tests.mk
include misc/make/linux-gpt-partition-role-tests.mk
include misc/make/linux-gpt-partition-device-tests.mk
include misc/make/linux-mount-attachment-tests.mk
include misc/make/linux-mount-boot-policy-tests.mk
include misc/make/linux-mount-devtmpfs-policy-tests.mk
include misc/make/linux-mountinfo-parser-tests.mk
include misc/make/linux-mount-namespace-tests.mk
include misc/make/linux-authenticated-mountinfo-snapshot-tests.mk
include misc/make/linux-sysfs-block-parser-tests.mk
include misc/make/linux-sysfs-identity-tests.mk
include misc/make/draft-hardening-tests.mk
include misc/make/state-request-tests.mk
include misc/make/exact-fresh-transition-removal-tests.mk
include misc/make/transition-coordinator-tests.mk
include misc/make/transition-root-abi-publication-tests.mk
include misc/make/activation-namespace-tests.mk
include misc/make/startup-gate-tests.mk
include misc/make/mutable-system-capabilities-tests.mk
include misc/make/system-trigger-view-tests.mk
include misc/make/boot-synchronization-authority-tests.mk
include misc/make/active-reblit-boot-state-root-tests.mk
include misc/make/active-reblit-boot-schema-input-tests.mk
include misc/make/active-reblit-local-boot-policy-tests.mk
include misc/make/active-reblit-package-cmdline-input-tests.mk
include misc/make/active-reblit-boot-topology-intent-tests.mk
include misc/make/active-reblit-root-filesystem-intent-tests.mk
include misc/make/active-reblit-boot-render-input-tests.mk
include misc/make/active-reblit-bls-renderer-tests.mk
include misc/make/active-reblit-desired-publication-tests.mk
include misc/make/active-reblit-boot-publication-receipt-tests.mk
include misc/make/active-reblit-boot-namespace-input-tests.mk
include misc/make/startup-recovery-tests.mk
include misc/make/startup-root-abi-normalization-tests.mk
include misc/make/startup-exchange-durability-tests.mk
include misc/make/startup-rollback-resume-route-tests.mk
include misc/make/startup-candidate-preserve-admission-tests.mk
include misc/make/startup-candidate-preserve-target-tests.mk
include misc/make/startup-candidate-preserve-effect-tests.mk
include misc/make/startup-active-reblit-candidate-preserve-effect-tests.mk
include misc/make/startup-active-reblit-candidate-preserve-post-exchange-durability-tests.mk
include misc/make/startup-active-reblit-candidate-preserve-persistence-tests.mk
include misc/make/startup-candidate-preserve-post-move-durability-tests.mk
include misc/make/startup-candidate-preserve-persistence-tests.mk
include misc/make/startup-rollback-archived-candidate-preserve-foundation-tests.mk
include misc/make/startup-rollback-activate-archived-candidate-dispatch-tests.mk
include misc/make/startup-fresh-db-invalidation-route-tests.mk
include misc/make/startup-fresh-db-invalidation-effect-tests.mk
include misc/make/startup-fresh-db-invalidation-persistence-tests.mk
include misc/make/startup-rollback-complete-route-tests.mk
include misc/make/startup-rollback-finalization-tests.mk
include misc/make/startup-rollback-active-reblit-candidate-dispatch-tests.mk
include misc/make/startup-rollback-active-reblit-complete-route-tests.mk
include misc/make/startup-active-reblit-boot-repair-required-tests.mk
include misc/make/startup-active-reblit-boot-repair-unverified-tests.mk
include misc/make/startup-active-reblit-boot-repair-complete-tests.mk
include misc/make/startup-rollback-activate-archived-complete-route-tests.mk
include misc/make/startup-rollback-activate-archived-finalization-tests.mk
include misc/make/startup-rollback-active-reblit-finalization-tests.mk
include misc/make/startup-root-links-terminal-process-tests.mk
include misc/make/startup-rollback-new-state-dispatch-tests.mk
include misc/make/startup-rollback-reverse-admission-tests.mk
include misc/make/startup-rollback-reverse-projection-tests.mk
include misc/make/startup-rollback-reverse-effect-adapter-tests.mk
include misc/make/startup-rollback-reverse-reconciliation-tests.mk
include misc/make/startup-rollback-reverse-effect-tests.mk
include misc/make/startup-rollback-reverse-namespace-durability-tests.mk
include misc/make/startup-rollback-reverse-durability-tests.mk
include misc/make/startup-rollback-reverse-persistence-tests.mk
include misc/make/startup-rollback-reverse-dispatch-tests.mk
include misc/make/userspace-profile-fixture.mk
include misc/make/fixture-proof-tests.mk
include misc/make/git-source-hardening-tests.mk
include misc/make/mason-generated-routing-tests.mk
include misc/make/mason-elf-debug-route-tests.mk
include misc/make/multiple-sources-fixture-tests.mk
include misc/make/gettext-localization-fixture-tests.mk
include misc/make/go-module-fixture-tests.mk
include misc/make/python-module-fixture-tests.mk
include misc/make/pgo-workload-fixture-tests.mk
include misc/make/relation-policy-fixture-tests.mk
include misc/make/desktop-integration-fixture-tests.mk
include misc/make/font-family-fixture-tests.mk
include misc/make/system-integration-assets-fixture-tests.mk
include misc/make/external-test-vectors-fixture-tests.mk
include misc/make/help.mk

.PHONY: build cast get-started licenses fix lint test config-rooted-gluon-test forge-client-startup-gate-test forge-active-state-snapshot-test forge-transition-identity-test forge-state-prune-test forge-active-reblit-wrapper-test forge-archived-repair-test forge-stateful-candidate-metadata-test forge-ephemeral-candidate-metadata-test forge-fixed-staging-test forge-previous-tree-move-test forge-archived-candidate-move-test forge-frozen-normalization-test forge-frozen-publication-test forge-frozen-discard-test cache-clean-test cast-example-process-supervision-test examples examples-gate-test execution-fixtures execution-capability-preflight-test delegated-execution-preflight delegated-execution-fixtures delegated-fixture-runner-test bootstrap-fixtures bootstrap-fixtures-prepare bootstrap-fixtures-offline bootstrap-fixtures-tmp bootstrap-fixture-selection bootstrap-execution-requirement fixtures-ci fixture-sources fixture-sources-check make-shell-portability-test source-loc source-loc-test check fmt clean \
	binary-layout product-names config-formats config-formats-test migrate migrate-redo \
	libstone

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

make-shell-portability-test:
	@timeout 120s "$(TOP_DIR)/misc/scripts/test-make-shell-portability.sh"

source-loc:
	@timeout 120s "$(SHELL)" "$(TOP_DIR)/misc/scripts/check-source-loc.sh"

source-loc-test:
	@timeout 120s "$(SHELL)" "$(TOP_DIR)/misc/scripts/test-check-source-loc.sh"

# Container activation uses fork-like namespace creation. Keep each libtest
# process to one active test worker; production single-task behavior is proved
# separately by harness-free container and delegated Mason integration targets.
test: host-storage-safety-test lint config-formats-test examples-gate-test delegated-fixture-runner-test cache-clean-test execution-capability-preflight-test mason-generated-routing-test mason-elf-debug-route-test
	@echo "Running tests in all packages..."
	@$(CARGO) test --all --no-fail-fast -- --test-threads=1

config-rooted-gluon-test:
	@set -eu; \
	listed="$$( $(CARGO) test -p config --lib -- --list )"; \
	for test in \
		rooted_gluon::tests::rooted_load_uses_the_retained_tree_after_public_path_substitution \
		rooted_gluon::tests::rooted_load_rejects_nested_import_directory_substitution_during_decode; do \
		printf '%s\n' "$$listed" | grep -Fqx "$$test: test"; \
		$(CARGO) test -p config --lib "$$test" -- --exact --test-threads=1; \
	done; \
	chain_test=source::tests::descriptor_root_rejects_substitution_beneath_a_retained_import_directory; \
	gluon_listed="$$( $(CARGO) test -p gluon_config --lib -- --list )"; \
	printf '%s\n' "$$gluon_listed" | grep -Fqx "$$chain_test: test"; \
	$(CARGO) test -p gluon_config --lib "$$chain_test" -- --exact --test-threads=1

forge-active-state-snapshot-test:
	@set -eu; \
	listed="$$( $(CARGO) test -p forge --lib -- --list )"; \
	for test in \
		client::active_state_snapshot_tests::exact_empty_and_authenticated_marker_only_first_install_baselines_are_accepted \
		client::active_state_snapshot_tests::missing_state_id_rejects_nonempty_or_unauthenticated_marker_only_usr \
		client::active_state_snapshot_tests::clean_live_selection_changes_are_distinct_from_invalid_evidence \
		client::active_state_snapshot_tests::malformed_and_unsafe_state_ids_fail_closed_instead_of_becoming_absence \
		client::active_state_snapshot_tests::wrong_mode_and_nonregular_state_ids_are_rejected_and_preserved \
		client::active_state_snapshot_tests::usr_and_state_id_symlinks_are_never_followed \
		client::active_state_snapshot_tests::installation_discovery_is_bounded_and_never_follows_unsafe_state_entries \
		client::active_state_snapshot_tests::state_id_final_name_replacement_is_rejected_with_both_inodes_preserved \
		client::active_state_snapshot_tests::same_inode_state_id_rewrite_after_first_read_is_rejected \
		client::active_state_snapshot_tests::state_id_insertion_during_absence_proof_is_rejected_untouched \
		client::active_state_snapshot_tests::foreign_entry_inserted_after_first_empty_scan_is_rejected_untouched \
		client::active_state_snapshot_tests::retained_lease_rejects_same_inode_state_id_aba_after_acquisition \
		client::active_state_snapshot_tests::retained_lease_rejects_whole_usr_replacement_and_restore \
		client::active_state_snapshot_tests::malformed_live_state_after_installation_open_blocks_repositories_after_database_open \
		client::active_state_snapshot_tests::stale_builder_opens_databases_but_rejects_before_repository_construction \
		client::active_state_snapshot_tests::reused_client_rejects_a_second_state_before_database_allocation \
		client::active_state_snapshot_tests::state_id_aba_during_candidate_fill_fails_before_row_allocation_or_tree_identity \
		client::active_state_snapshot_tests::stale_cloned_client_cannot_activate_after_a_sibling_transition \
		client::active_state_snapshot_tests::stale_verify_prune_boot_and_read_apis_fail_before_authoritative_work \
		client::active_state_snapshot_tests::stale_registry_queries_fail_before_reading_the_construction_time_active_plugin \
		client::active_state_snapshot_tests::workflow_registry_reads_reject_a_sibling_transition_after_public_preflight \
		client::active_state_snapshot_tests::available_closure_rejects_a_sibling_transition_even_without_requests \
		cli::sync::tests::sync_import_cli_evaluates_authored_intent_for_an_ephemeral_target \
		client::tests::ephemeral_import_evaluates_intent_and_records_only_a_generated_snapshot \
		client::tests::ephemeral_blit_isolates_cached_asset_bytes_and_mode \
		client::tests::ephemeral_root_and_isolation_root_abi_conflicts_are_both_non_destructive \
		client::install::tests::frozen_resolution_uses_only_exact_ids_without_dependency_recomposition \
		client::install::tests::public_frozen_materialization_ignores_ambient_active_and_cobble_candidates \
		client::install::tests::metadata_only_frozen_closure_publishes_without_an_asset_pool \
		client::install::tests::frozen_client_rejects_other_mutating_apis_before_side_effects \
		client::active_state_snapshot_tests::stale_stateful_candidate_fails_before_fixed_staging_mutation \
		client::active_state_snapshot_tests::stale_ephemeral_candidate_fails_before_touching_its_external_target \
		client::active_state_authority_tests::restart_rejects_missing_or_malformed_active_metadata_before_client_construction \
		client::active_state_authority_tests::public_verify_rejects_damaged_active_metadata_without_repairing_it \
		client::active_state_authority_tests::matching_canonical_bytes_with_unsafe_metadata_fail_closed \
		client::active_state_authority_tests::suspended_strict_authority_rejects_same_inode_mutation_before_resume; do \
		grep -Fqx "$$test: test" <<<"$$listed"; \
		$(CARGO) test -p forge --lib "$$test" -- --exact --test-threads=1; \
	done

forge-transition-identity-test:
	@set -eu; \
	timeout 10s grep -Fqx 'mod candidate_metadata;' crates/forge/src/transition_identity.rs; \
	if timeout 10s grep -Fqx 'pub(crate) mod candidate_metadata;' crates/forge/src/transition_identity.rs; then \
		timeout 10s printf '%s\n' 'candidate metadata core module visibility widened' >&2; exit 1; \
	else \
		status="$$?"; \
		if [ "$$status" -ne 1 ]; then \
			timeout 10s printf 'candidate metadata visibility audit failed with status %s\n' "$$status" >&2; exit 1; \
		fi; \
	fi; \
	timeout 10s grep -Fqx 'mod existing_verification;' crates/forge/src/transition_identity/candidate_metadata.rs; \
	if timeout 10s grep -Fqx 'pub(crate) mod existing_verification;' crates/forge/src/transition_identity/candidate_metadata.rs; then \
		timeout 10s printf '%s\n' 'existing metadata verifier module visibility widened' >&2; exit 1; \
	else \
		status="$$?"; timeout 10s test "$$status" = 1; \
	fi; \
	verification="crates/forge/src/transition_identity/candidate_metadata/existing_verification.rs"; \
	if timeout 10s grep -nE '\.sync_all\(|\.set_permissions\(|link_path_descriptor|renameat|O_TMPFILE|write_all_at|RetainedDirectory::retain_or_create' "$$verification"; then \
		timeout 10s printf '%s\n' 'read-only existing metadata verifier gained a mutation primitive' >&2; exit 1; \
	else \
		status="$$?"; timeout 10s test "$$status" = 1; \
	fi; \
	prove_signature="$$( timeout 10s sed -n '/pub(crate) fn prove(/,/) -> Result<CandidateMetadataProof, CandidateMetadataError> {/p' "$$verification" )"; \
	timeout 10s test -n "$$prove_signature"; \
	for required in 'pub(crate) fn prove(' 'outputs: CandidateMetadataOutputs' 'Result<CandidateMetadataProof, CandidateMetadataError>'; do \
		timeout 10s grep -Fq "$$required" <<<"$$prove_signature"; \
	done; \
	if timeout 10s grep -Eq 'os_release|system_model|&\[u8\]' <<<"$$prove_signature"; then \
		timeout 10s printf '%s\n' 'existing metadata verifier accepts untyped caller-supplied output buffers' >&2; exit 1; \
	else \
		status="$$?"; timeout 10s test "$$status" = 1; \
	fi; \
	if timeout 10s grep -RInF 'CandidateMetadataVerification' crates/forge/src/client; then \
		timeout 10s printf '%s\n' 'client bypasses coordinator-owned existing metadata verification' >&2; exit 1; \
	else \
		status="$$?"; timeout 10s test "$$status" = 1; \
	fi; \
	for forbidden in 'client::' 'MetadataContext' 'SystemModel'; do \
		if timeout 10s grep -R -n -F "$$forbidden" crates/forge/src/transition_identity/candidate_metadata.rs crates/forge/src/transition_identity/candidate_metadata; then \
			timeout 10s printf 'candidate metadata core depends on forbidden client policy: %s\n' "$$forbidden" >&2; exit 1; \
		else \
			status="$$?"; \
			if [ "$$status" -ne 1 ]; then \
				timeout 10s printf 'candidate metadata layering audit failed with status %s\n' "$$status" >&2; exit 1; \
			fi; \
		fi; \
	done; \
	listed="$$( timeout 300s $(CARGO) test -p forge --lib -- --list )"; \
	for test in \
		transition_identity::candidate_metadata::tests::same_candidate_proof_accepts_exact_inode_and_rejects_same_layout_foreign_candidate_without_mutation \
		transition_identity::candidate_metadata::tests::existing_metadata_verification_proves_independent_bytes_without_mutation \
		transition_identity::candidate_metadata::tests::existing_metadata_verification_rejects_wrong_independent_bytes_without_mutation \
		transition_identity::candidate_metadata::tests::existing_metadata_verification_rejects_same_byte_release_replacement_during_proof \
		transition_identity::candidate_metadata::tests::existing_metadata_verification_rejects_unsafe_canonical_outputs_without_repair \
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
		client::tests::root_abi_preflight::every_live_root_abi_conflict_precedes_candidate_trigger_and_exchange_mutation \
		client::tests::root_abi_preflight::retained_live_root_abi_rejects_replacement_at_the_exchange_boundary \
		client::tests::root_abi_preflight::retained_absent_root_abi_rejects_appearance_at_the_exchange_boundary \
		client::tests::root_abi_preflight::post_exchange_root_abi_publication_conflict_reverses_usr_and_preserves_foreign_entry \
		client::tests::archived_live_root_abi_conflict_precedes_staging_triggers_and_usr_exchange \
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
		timeout 10s grep -Fqx "$$test: test" <<<"$$listed"; \
		timeout 300s $(CARGO) test -p forge --lib "$$test" -- --exact --test-threads=1; \
	done

forge-state-prune-test:
	@set -euo pipefail; \
	timeout 10s mkdir -p "$(TOP_DIR)/target"; \
	listed="$$( timeout 10s mktemp "$(TOP_DIR)/target/state-prune-list.XXXXXXXXXXXX" )"; \
	trap 'timeout 10s rm -f "$$listed"' EXIT; \
	timeout 300s $(CARGO) test -p forge --lib -- --list | timeout 300s tee "$$listed" >/dev/null; \
	timeout 10s grep -q . "$$listed"; \
	for test in \
		client::boot::tests::prune_exclusions_are_applied_before_bounded_rollback_selection \
		client::boot::tests::excluded_state_is_ineligible_even_when_its_canonical_path_exists \
		client::tests::state_prune::fresh_exact_wrapper_is_detached_committed_and_deleted \
		client::tests::state_prune::boot_projection::production_client_prune_completes_detach_boot_db_delete_and_gc_order \
		client::tests::state_prune::boot_projection::definitely_not_applied_boot_fault_restores_archives_without_touching_boot \
		client::tests::state_prune::boot_projection::ambiguous_post_projection_is_compensated_before_reservations_are_retired \
		client::tests::state_prune::boot_projection::failed_ambiguous_boot_compensation_leaves_restart_blocking_residue \
		client::tests::state_prune::boot_projection::active_state_failure_after_prepare_or_detach_restores_archives_before_boot \
		client::tests::state_prune::boot_projection::active_state_failure_after_boot_restores_prior_projection_before_retiring_reservations \
		client::tests::state_prune::startup_residue::prepared_process_loss_blocks_reopened_client_without_changing_evidence \
		client::tests::state_prune::startup_residue::detached_pre_database_process_loss_blocks_reopened_client_without_changing_evidence \
		client::tests::state_prune::startup_residue::post_database_process_loss_blocks_reopened_client_without_changing_stranded_evidence \
		client::tests::state_prune::authorized_two_link_state_slot_wrapper_is_pruned_exactly \
		client::tests::state_prune::deletion_batches_more_than_256_entries_without_retaining_one_fd_per_entry \
		client::tests::state_prune::empty_batch_is_rejected_before_any_reservation \
		client::tests::state_prune::oversized_batch_is_rejected_before_any_reservation_or_wrapper_open \
		client::tests::state_prune::client_rejects_oversized_selection_before_loading_or_printing_snapshots \
		client::tests::state_prune::applied_detach_fault_keeps_database_evidence_and_retries_suffix \
		client::tests::state_prune::multi_state_partial_detach_restores_every_wrapper_then_retires_reservations \
		client::tests::state_prune::preexisting_prune_residue_blocks_the_batch_before_its_first_reservation \
		client::tests::state_prune::restored_phase_retries_a_partially_applied_reservation_unlink \
		client::tests::state_prune::canonical_wrapper_substitution_before_move_is_never_adopted \
		client::tests::state_prune::foreign_quarantine_wrapper_occupant_is_preserved_without_moving_canonical \
		client::tests::state_prune::deterministic_quarantine_residue_is_preserved_and_rejected \
		client::tests::state_prune::metadata_corruption_is_rejected_without_reservation \
		client::tests::state_prune::phase_api_forbids_delete_before_database_and_restore_after_database \
		client::tests::state_prune::mode_zero_directories_and_symlinks_are_deleted_without_following_targets \
		client::tests::state_prune::child_substitution_in_final_check_syscall_window_preserves_foreign_entry \
		client::tests::state_prune::partial_private_unlink_and_sync_faults_retry_in_process \
		client::tests::state_prune::aggregate_delete_boundaries_fail_closed \
		client::tests::state_prune::mounted_descendant_is_rejected_or_test_is_skipped_only_for_unavailable_mounting \
		db::state::test::exact_archived_removal_rejects_an_empty_batch \
		db::state::test::exact_archived_removal_rejects_duplicate_changed_and_transition_rows \
		db::state::test::exact_archived_removal_reconciles_not_applied_applied_and_ambiguous_reports; do \
		timeout 10s grep -Fqx "$$test: test" "$$listed"; \
		timeout 300s $(CARGO) test -p forge --lib "$$test" -- --exact --test-threads=1; \
	done

forge-active-reblit-wrapper-test:
	@set -eu; \
	listed="$$( $(CARGO) test -p forge --lib -- --list )"; \
	for test in \
		client::active_reblit_tests::active_reblit_rotates_the_whole_old_wrapper_and_leaves_exact_empty_staging \
		client::active_reblit_tests::active_reblit_refuses_missing_or_malformed_live_state_id_without_staging_mutation \
		client::active_reblit_tests::active_reblit_rejects_same_inode_state_id_rewrite_before_exchange \
		client::active_reblit_tests::active_reblit_rejects_same_content_new_state_id_inode \
		client::active_reblit_tests::active_reblit_exchange_preflight_rejects_last_moment_state_id_replacement \
		client::active_reblit_tests::active_reblit_system_boundary_corruption_reverses_and_preserves_bad_candidate \
		client::active_reblit_tests::active_reblit_pre_boot_checkpoint_state_id_mutation_is_rejected_before_boot \
		client::active_reblit_tests::every_single_staging_wrapper_fault_is_resumed_without_tree_loss \
		client::active_reblit_tests::queued_not_applied_rotation_faults_reverse_then_preserve_one_whole_wrapper \
		client::active_reblit_tests::queued_applied_suffix_faults_never_exchange_the_wrapper_twice \
		client::active_reblit_tests::staging_wrapper_substitution_is_ambiguous_and_never_retried \
		client::active_reblit_tests::staging_wrapper_scan_skips_foreign_types_and_uses_next_index \
		client::active_reblit_tests::staging_wrapper_name_exhaustion_falls_back_without_touching_live_or_occupants \
		client::active_reblit_tests::staging_wrapper_pre_retention_substitution_uses_marker_authenticated_fallback \
		client::active_reblit_tests::two_successful_active_reblits_on_one_client_use_distinct_wrapper_slots \
		client::active_reblit_tests::active_reblit_preserves_authorized_two_link_previous_marker_pair \
		client::active_reblit_tests::every_single_active_previous_slot_parking_fault_resumes_without_a_second_move \
		client::active_reblit_tests::active_previous_slot_parking_exhaustion_preserves_every_name_and_old_live_tree \
		client::active_reblit_tests::active_previous_slot_scan_skips_every_foreign_occupant_kind \
		client::active_reblit_tests::queued_active_previous_slot_suffix_faults_keep_the_move_applied \
		client::active_reblit_tests::active_previous_slot_substitution_never_moves_or_adopts_the_foreign_wrapper \
		client::active_reblit_tests::active_previous_slot_parking_adopts_an_exact_externally_applied_move \
		client::active_reblit_tests::already_parked_previous_slot_with_foreign_canonical_name_fails_closed \
		client::active_reblit_tests::active_reblit_rejects_a_slot_moved_back_to_canonical_after_triggers \
		client::active_reblit_tests::active_reblit_reversal_cannot_report_success_after_parked_slot_is_moved_back; do \
		printf '%s\n' "$$listed" | grep -Fqx "$$test: test"; \
		$(CARGO) test -p forge --lib "$$test" -- --exact --test-threads=1; \
	done

forge-archived-repair-test:
	@set -eu; \
	listed="$$( $(CARGO) test -p forge --lib -- --list )"; \
	for test in \
		client::postblit::retained_trigger_discovery::tests::transaction_codec_loads_from_retained_descriptor_after_public_path_substitution \
		client::postblit::retained_trigger_discovery::tests::system_codec_loads_from_retained_descriptor_after_public_path_substitution \
		client::postblit::retained_transaction::tests::container_rejects_an_isolation_root_replacement_after_abi_provisioning \
		client::postblit::retained_transaction::tests::writable_bind_substitution_fails_closed_before_payload \
		client::archived_repair_tests::archived_repair_replaces_the_whole_wrapper_and_preserves_old_payload_opaquely \
		client::archived_repair_tests::archived_repair_publishes_missing_wrapper_directly_and_restores_empty_staging \
		client::archived_repair_tests::archived_repair_runs_only_transaction_scope_and_never_mutates_live_namespaces \
		client::archived_repair_tests::archived_repair_preserves_a_trigger_corrupted_candidate_as_one_opaque_wrapper \
		client::archived_repair_tests::archived_repair_rejects_a_foreign_canonical_file_without_touching_it \
		client::archived_repair_tests::archived_repair_preserves_an_old_wrapper_with_no_state_id_without_repairing_it \
		client::archived_repair_tests::archived_repair_detects_active_row_deletion_and_reports_preservation_incomplete \
		client::archived_repair_tests::archived_repair_rejects_a_target_selected_active_before_guard_preparation \
		client::archived_repair_tests::metadata::metadata_decoration_never_follows_a_candidate_lib_symlink \
		client::archived_repair_tests::metadata::metadata_decoration_never_follows_an_os_info_symlink \
		client::archived_repair_tests::metadata::existing_metadata_outputs_are_preserved_without_mutating_regular_or_hardlinked_inodes \
		client::archived_repair_tests::metadata::successful_metadata_publication_creates_independent_sealed_files \
		client::archived_repair_tests::metadata::a_second_metadata_name_collision_preserves_the_partial_candidate_without_replacing_the_occupant \
		client::archived_repair_tests::metadata::deleting_the_first_metadata_output_during_pair_publication_preserves_the_partial_candidate \
		client::archived_repair_tests::metadata::replacing_the_first_metadata_output_during_pair_publication_never_adopts_the_occupant \
		client::archived_repair_tests::materialization::the_public_low_level_blitter_rejects_fixed_staging_unchanged \
		client::archived_repair_tests::materialization::archived_repair_materialization_uses_a_private_inode_and_never_chmods_the_asset_pool \
		client::archived_repair_tests::materialization::a_write_at_the_transaction_trigger_boundary_cannot_mutate_cache_or_live_aliases \
		client::archived_repair_tests::materialization::identical_digest_outputs_keep_distinct_package_modes_without_chmodding_aliases \
		client::archived_repair_tests::materialization::nonempty_fixed_staging_crash_residue_is_refused_without_traversal_or_deletion \
		client::archived_repair_tests::materialization::an_exact_empty_private_staging_baseline_is_reused_without_replacement \
		client::archived_repair_tests::materialization::an_empty_staging_name_substitution_is_refused_without_removing_either_inode \
		client::archived_repair_tests::identity::same_content_state_id_inode_replacement_is_preserved_but_never_published \
		client::archived_repair_tests::identity::same_content_tree_marker_inode_replacement_is_preserved_but_never_published \
		client::archived_repair_tests::semantics::live_active_selection_change_is_detected_without_trusting_the_cached_field \
		client::archived_repair_tests::semantics::repaired_target_row_deletion_is_detected_and_the_candidate_is_still_preserved \
		client::archived_repair_tests::semantics::same_content_live_state_id_inode_replacement_fails_closed \
		client::archived_repair_tests::semantics::same_content_live_tree_marker_inode_replacement_fails_closed \
		client::archived_repair_tests::semantics::same_content_whole_live_usr_replacement_fails_closed \
		client::archived_repair_tests::faults::every_single_archived_repair_publication_fault_resumes_without_tree_loss \
		client::archived_repair_tests::faults::archived_repair_preparation_faults_leave_candidate_staged_and_archive_unchanged \
		client::archived_repair_tests::faults::preparation_reports_primary_and_exact_reservation_cleanup_failures \
		client::archived_repair_tests::faults::queued_not_applied_publication_faults_preserve_candidate_once \
		client::archived_repair_tests::faults::queued_applied_suffix_faults_never_reverse_committed_candidate \
		client::archived_repair_tests::faults::substituted_staging_before_publication_is_ambiguous_and_never_adopted \
		client::archived_repair_tests::faults::substituted_staging_before_preservation_is_ambiguous_and_never_overwritten \
		client::archived_repair_tests::faults::substituted_roots_parent_proof_is_ambiguous_without_a_rename_or_retry \
		client::archived_repair_tests::faults::substituted_quarantine_parent_proof_is_ambiguous_without_a_rename_or_retry \
		client::archived_repair_tests::faults::replacement_content_substitution_is_ambiguous_without_a_rename_or_retry \
		client::archived_repair_tests::faults::replacement_inode_substitution_is_ambiguous_without_a_rename_or_retry \
		client::archived_repair_tests::faults::archived_repair_quarantine_scan_skips_foreign_file_types \
		client::archived_repair_tests::faults::archived_repair_quarantine_exhaustion_preserves_every_namespace \
		client::archived_repair_tests::faults::externally_completed_cleanup_is_adopted_without_a_second_exchange \
		client::archived_repair_tests::faults::a_layout_change_between_sandwich_reads_is_ambiguous_and_never_reversed \
		client::archived_repair_tests::faults::publication_suffix_retry_reconciles_staging_substitution_as_ambiguous \
		client::archived_repair_tests::faults::preservation_suffix_retry_reconciles_staging_substitution_as_ambiguous \
		client::archived_repair_tests::namespace_races::successful_existing_publication_reversal_is_ambiguous_without_a_second_exchange \
		client::archived_repair_tests::namespace_races::externally_published_missing_candidate_is_adopted_without_a_second_publish \
		client::archived_repair_tests::namespace_races::successful_existing_cleanup_reversal_is_ambiguous_without_a_second_exchange \
		client::archived_repair_tests::namespace_races::externally_completed_missing_cleanup_is_adopted_without_a_second_restore \
		client::archived_repair_tests::namespace_races::successful_preservation_reversal_is_ambiguous_without_a_second_exchange; do \
		printf '%s\n' "$$listed" | grep -Fx "$$test: test" >/dev/null; \
		$(CARGO) test -p forge --lib "$$test" -- --exact --test-threads=1; \
	done

forge-stateful-candidate-metadata-test:
	@set -eu; \
	listed="$$( $(CARGO) test -p forge --lib -- --list )"; \
	for test in \
		client::tests::stateful_candidate_metadata::stateful_candidate_metadata_never_follows_lib_or_os_info_symlinks \
		client::tests::stateful_candidate_metadata::stateful_candidate_metadata_never_follows_output_symlinks \
		client::tests::stateful_candidate_metadata::stateful_candidate_metadata_preserves_existing_output_inodes \
		client::tests::stateful_candidate_metadata::stateful_candidate_metadata_final_name_races_are_no_replace \
		client::tests::stateful_candidate_metadata::retained_metadata_proof_rejects_post_trigger_mutation \
		client::tests::stateful_candidate_metadata::retained_metadata_proof_rejects_post_system_trigger_mutation \
		client::tests::stateful_candidate_metadata::candidate_usr_clone_failure_precedes_all_metadata_decoration \
		client::tests::stateful_candidate_metadata::owned_metadata_proof_outlives_source_identity_and_rejects_named_substitution \
		client::tests::stateful_candidate_metadata::candidate_usr_substitution_before_metadata_never_decorates_replacement \
		client::postblit::retained_trigger_discovery::tests::stateful_system_scope_compiles_intent_from_the_retained_live_usr \
		client::postblit::system_trigger_container::tests::non_live_system_root_and_source_substitutions_fail_before_payload_mutation \
		client::tests::stateful_candidate_metadata::successful_stateful_metadata_is_sealed_and_rollback_capable; do \
		timeout 10s grep -Fqx "$$test: test" <<<"$$listed"; \
		$(CARGO) test -p forge --lib "$$test" -- --exact --test-threads=1; \
	done

forge-ephemeral-candidate-metadata-test:
	@set -eu; \
	listed="$$( $(CARGO) test -p forge --lib -- --list )"; \
	for test in \
		client::tests::ephemeral_candidate_metadata::ephemeral_candidate_metadata_never_follows_lib_or_os_info_symlinks \
		client::tests::ephemeral_candidate_metadata::ephemeral_candidate_metadata_never_follows_output_symlinks \
		client::tests::ephemeral_candidate_metadata::ephemeral_candidate_metadata_preserves_existing_output_inodes \
		client::tests::ephemeral_candidate_metadata::ephemeral_candidate_metadata_final_name_races_are_no_replace \
		client::tests::ephemeral_candidate_metadata::ephemeral_candidate_metadata_rejects_first_output_deletion_or_replacement \
		client::tests::ephemeral_candidate_metadata::retained_ephemeral_metadata_proof_rejects_post_transaction_mutation \
		client::tests::ephemeral_candidate_metadata::retained_ephemeral_metadata_proof_rejects_post_system_mutation \
		client::tests::ephemeral_candidate_metadata::target_or_usr_substitution_before_transaction_discovery_cannot_reach_replacement_triggers \
		client::tests::ephemeral_candidate_metadata::target_or_usr_substitution_between_phases_cannot_reach_replacement_system_triggers \
		client::postblit::retained_ephemeral::tests::retained_ephemeral_phase_policies_keep_transaction_etc_read_only \
		client::postblit::retained_ephemeral::tests::transaction_container_mounts_usr_read_write_and_etc_read_only \
		client::postblit::retained_ephemeral::tests::system_container_mounts_usr_and_etc_read_write \
		client::postblit::retained_ephemeral::tests::public_root_usr_and_etc_substitution_fails_closed_before_payload \
		client::postblit::retained_ephemeral::tests::container_rejects_an_isolation_root_substitution \
		client::postblit::retained_ephemeral::tests::retained_ephemeral_system_scope_never_uses_live_root_direct_execution \
		client::tests::external_materialization::ephemeral_trigger_view_retains_exact_usr_and_publishes_exact_etc \
		client::tests::external_materialization::ephemeral_trigger_etc_publication_never_adopts_a_racing_occupant \
		client::tests::external_materialization::ephemeral_trigger_view_rejects_named_etc_replacement \
		client::tests::external_materialization::retained_root_abi_replacement_fails_before_any_name_mutation \
		client::tests::ephemeral_candidate_metadata::successful_ephemeral_metadata_is_exact_evaluable_and_root_abi_complete; do \
		printf '%s\n' "$$listed" | grep -Fqx "$$test: test"; \
		$(CARGO) test -p forge --lib "$$test" -- --exact --test-threads=1; \
	done

forge-fixed-staging-test:
	@set -eu; \
	listed="$$( $(CARGO) test -p forge --lib -- --list )"; \
	for test in \
		client::tests::fixed_staging_transition::exact_empty_staging_is_reused_and_returns_the_published_usr_inode \
		client::tests::fixed_staging_transition::exact_empty_legacy_staging_is_normalized_without_replacing_its_inode \
		client::tests::fixed_staging_transition::nonempty_legacy_staging_residue_is_preserved_byte_for_byte \
		client::tests::fixed_staging_transition::an_entry_inserted_before_fill_is_rejected_without_traversal \
		client::tests::fixed_staging_transition::candidate_usr_publication_collision_preserves_private_and_public_trees \
		client::tests::fixed_staging_transition::filled_private_usr_is_not_replaced_by_a_last_moment_public_occupant \
		client::tests::fixed_staging_transition::stateful_candidate_same_digest_modes_and_writes_are_isolated_from_cache \
		client::tests::fixed_staging_transition::stateful_candidate_rejects_corrupt_cache_bytes_without_publishing_usr \
		client::tests::fixed_staging_transition::retained_state_id_write_never_targets_a_substituted_usr \
		client::tests::fixed_staging_transition::stateful_trigger_preparation_never_follows_a_replaced_isolation_root \
		client::tests::fixed_staging_transition::archived_repair_state_id_write_uses_the_same_retained_usr \
		client::tests::fixed_staging_transition::coordinator_lease_spans_state_allocation_and_retained_identity_preparation \
		client::tests::fixed_staging_transition::public_and_cross_install_blitters_cannot_target_fixed_staging \
		client::tests::fixed_staging_transition::frozen_client_rejects_destination_beneath_installation_root \
		client::tests::fixed_staging_transition::ephemeral_materialization_rechecks_empty_target_under_the_lease \
		client::tests::fixed_staging_transition::ephemeral_activate_and_boot_sync_fail_before_fixed_namespace_mutation \
		client::tests::fixed_staging_transition::frozen_activate_boot_verify_and_prune_fail_before_installation_mutation \
		client::tests::external_materialization::exact_empty_external_target_keeps_its_inode_and_retains_an_empty_usr \
		client::tests::external_materialization::an_absent_empty_closure_publishes_a_root_with_one_retained_usr \
		client::tests::external_materialization::parent_path_replacement_after_retention_cannot_redirect_target_creation \
		client::tests::external_materialization::directory_replacement_between_admission_and_preparation_is_rejected \
		client::tests::external_materialization::symlink_replacement_between_admission_and_preparation_cannot_reach_a_safe_victim \
		client::tests::external_materialization::absent_admitted_target_rejects_an_inserted_empty_directory_untouched \
		client::tests::external_materialization::present_admitted_target_rejects_an_empty_inode_replacement_untouched \
		client::tests::external_materialization::present_admitted_target_rejects_removal_without_recreating_its_name \
		client::tests::external_materialization::absent_target_collision_is_never_adopted_or_removed \
		client::tests::external_materialization::target_substitution_before_fill_preserves_both_inodes_without_writing_either \
		client::tests::external_materialization::final_name_substitution_never_turns_a_filled_retained_root_into_success \
		client::tests::external_materialization::symlink_and_nonempty_targets_are_left_untouched \
		client::tests::external_materialization::world_writable_direct_parent_is_rejected_without_creating_or_removing_a_target \
		client::transaction_root::tests::created_local_etc_is_normalized_and_authenticated \
		client::transaction_root::tests::private_name_substitution_is_rejected_without_chmodding_the_replacement \
		client::transaction_root::tests::preexisting_group_writable_or_symlink_local_etc_is_preserved_and_rejected \
		client::transaction_root::tests::final_name_substitution_during_local_etc_proof_is_rejected \
		client::tests::verify_reblits_and_preserves_the_existing_normalized_snapshot; do \
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
		client::tests::materialization_rejects_decodable_special_layouts_with_exact_package_and_path \
		client::tests::frozen_root_rejects_every_special_layout_before_touching_destination \
		client::tests::external_materialization::low_level_blitter_rejects_special_layout_instead_of_silent_success \
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
		client::tests::frozen_blit_returns_an_opath_guard_accepted_by_anchored_container \
		client::tests::frozen_publication_rejects_a_readable_activation_descriptor_before_rename \
		client::tests::frozen_publication_rejects_a_foreign_opath_activation_anchor_before_rename \
		client::tests::frozen_publication_rejects_an_inheritable_opath_activation_anchor_before_rename \
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

cast-example-process-supervision-test:
	@echo "Proving every Cast example child is bounded and group-cleaned..."
	@set -eu; \
	if matches="$$( timeout 10s rg -n '\.output\s*\(\s*\)' \
		"$(TOP_DIR)/bin/cast/tests/gluon_examples.rs" \
		"$(TOP_DIR)/bin/cast/tests/gluon_examples" )"; then \
		timeout 10s printf '%s\n' "bare Command::output() is forbidden in the Cast Gluon example harness:" >&2; \
		timeout 10s printf '%s\n' "$$matches" >&2; \
		exit 1; \
	else \
		status=$$?; \
		timeout 10s test "$$status" -eq 1 || exit "$$status"; \
	fi; \
	listed="$$( timeout 300s $(CARGO) test -p cast --test gluon_examples -- --list )"; \
	for test in \
		'every_gluon_package_example_passes_the_public_cast_cli' \
		'process_supervision::bounded_cast_child_supervisor_drop_kills_and_reaps_group' \
		'process_supervision::bounded_cast_child_supervisor_escalates_ignored_term_to_kill' \
		'process_supervision::bounded_cast_child_supervisor_kills_and_reaps_descendant_tree' \
		'process_supervision::bounded_cast_child_supervisor_rejects_exited_leader_with_descendant' \
		'process_supervision::bounded_cast_child_supervisor_times_out_and_reaps_group' \
		'process_supervision::bounded_cast_child_supervisor_rejects_stdout_overflow_and_reaps_group' \
		'process_supervision::bounded_cast_child_supervisor_reuses_one_cleanup_deadline' \
		'process_supervision::cast_child_supervisor_helper'; do \
		timeout 10s grep -Fqx "$$test: test" <<<"$$listed"; \
	done
	@timeout 120s $(CARGO) test -p cast --test gluon_examples \
		process_supervision::bounded_cast_child_supervisor_ -- \
		--test-threads=1 --nocapture

examples: cast-example-process-supervision-test
	@echo "Checking every Gluon package example through the public Cast CLI..."
	@timeout 1200s $(CARGO) test -p cast --test gluon_examples \
		every_gluon_package_example_passes_the_public_cast_cli -- \
		--exact --nocapture
	@echo "Freezing every Gluon package example through the hermetic planner..."
	@set -eu; \
	listed="$$( timeout 300s $(CARGO) test -p mason --lib -- --list )"; \
	timeout 10s grep -Fqx 'planner::hermetic_tests::checked_in_package_examples_freeze_hermetically_and_reuse_exact_build_locks: test' <<<"$$listed"
	@timeout 1200s $(CARGO) test -p mason --lib \
		planner::hermetic_tests::checked_in_package_examples_freeze_hermetically_and_reuse_exact_build_locks -- \
		--exact --nocapture
	@echo "Proving metadata-only providers fail before frozen execution..."
	@set -eu; \
	listed="$$( timeout 300s $(CARGO) test -p mason --lib -- --list )"; \
	timeout 10s grep -Fqx 'planner::hermetic_tests::checked_in_metadata_only_example_fails_closed_before_execution: test' <<<"$$listed"
	@timeout 1200s $(CARGO) test -p mason --lib \
		planner::hermetic_tests::checked_in_metadata_only_example_fails_closed_before_execution -- \
		--exact --nocapture

examples-gate-test:
	@timeout 180s "$(TOP_DIR)/misc/scripts/test-examples-gate.sh"

fixture-sources:
	@"$(TOP_DIR)/misc/scripts/build-execution-fixtures.sh"

fixture-sources-check:
	@"$(TOP_DIR)/misc/scripts/build-execution-fixtures.sh" --check

execution-fixtures: fixture-sources-check multiple-sources-fixture-test gettext-localization-fixture-test go-module-fixture-test python-module-fixture-test pgo-workload-fixture-test relation-policy-fixture-test desktop-integration-fixture-test font-family-fixture-test system-integration-assets-fixture-test external-test-vectors-fixture-test
	@echo "Checking locked offline execution-source fixtures..."
	@set -eu; \
	listed="$$( $(CARGO) test -p mason --lib \
		planner::hermetic_tests::offline_execution_fixture_archives_are_real_locked_and_complete -- \
		--exact --list )"; \
	printf '%s\n' "$$listed" | \
		grep -Fqx 'planner::hermetic_tests::offline_execution_fixture_archives_are_real_locked_and_complete: test'
	@$(CARGO) test -p mason --lib \
		planner::hermetic_tests::offline_execution_fixture_archives_are_real_locked_and_complete -- \
		--exact --nocapture
	@echo "Checking fail-closed external patch-source admission..."
	@set -eu; \
	listed="$$( $(CARGO) test -p mason --lib \
		planner::hermetic_tests::hooks_patch_external_source_contract_fails_closed -- \
		--exact --list )"; \
	printf '%s\n' "$$listed" | \
		grep -Fqx 'planner::hermetic_tests::hooks_patch_external_source_contract_fails_closed: test'
	@$(CARGO) test -p mason --lib \
		planner::hermetic_tests::hooks_patch_external_source_contract_fails_closed -- \
		--exact --nocapture
	@echo "Checking the declarative pinned Stone bootstrap manifest and index..."
	@set -eu; \
	listed="$$( $(CARGO) test -p mason --lib \
		planner::hermetic_tests::bootstrap::pinned_bootstrap_manifest_is_bounded_and_index_authoritative -- \
		--exact --list )"; \
	printf '%s\n' "$$listed" | \
		grep -Fqx 'planner::hermetic_tests::bootstrap::pinned_bootstrap_manifest_is_bounded_and_index_authoritative: test'
	@$(CARGO) test -p mason --lib \
		planner::hermetic_tests::bootstrap::pinned_bootstrap_manifest_is_bounded_and_index_authoritative -- \
		--exact --nocapture
	@echo "Resolving all twenty-eight execution fixtures against the pinned real Stone index..."
	@set -eu; \
	listed="$$( $(CARGO) test -p mason --lib \
		planner::hermetic_tests::bootstrap::all_execution_fixtures_resolve_exactly_the_pinned_real_stone_closure -- \
		--exact --list )"; \
	printf '%s\n' "$$listed" | \
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

execution-capability-preflight-test:
	@$(CARGO) check -p mason --features delegated-fixture-test-support \
		--test delegated_execution_fixture
	@set -eu; \
	listed="$$( $(CARGO) test -p mason --lib -- --list )"; \
	for test in \
		delegated_preflight_tests::execution_requirement_rejects_missing_or_invalid_values \
		delegated_preflight_tests::successful_preflight_executes_fixture_materialization_once_for_both_policies \
		delegated_preflight_tests::optional_capability_denial_short_circuits_before_fixture_materialization \
		delegated_preflight_tests::required_capability_denial_fails_before_fixture_materialization \
		container::preflight::tests::execution_preflight_root_is_an_opath_directory_capability \
		container::preflight::tests::execution_preflight_classifies_only_known_namespace_setup_denials \
		planner::hermetic_tests::frozen_execution_capability_skip_never_hides_payload_or_ambiguous_nix_failures \
		planner::hermetic_tests::bootstrap::all_execution_fixtures_resolve_exactly_the_pinned_real_stone_closure; do \
		grep -Fqx "$$test: test" <<<"$$listed"; \
		$(CARGO) test -p mason --lib "$$test" -- --exact --test-threads=1; \
	done

delegated-execution-preflight: bootstrap-fixtures-tmp
	@echo "Checking the exact production execution capability before bootstrap download..."
	@TMPDIR="$(BOOTSTRAP_TMP_DIR)" \
		CAST_REQUIRE_EXECUTION=1 \
		CARGO="$(CARGO)" \
		"$(TOP_DIR)/misc/scripts/run-delegated-execution-fixture.sh" --preflight-only

bootstrap-fixtures-prepare: bootstrap-fixtures-tmp
	@echo "Fetching and verifying the exact contentful Stone bootstrap closure..."
	@set -eu; \
	listed="$$( TMPDIR="$(BOOTSTRAP_TMP_DIR)" $(CARGO) test -p mason --lib \
		planner::hermetic_tests::bootstrap::fetch_pinned_bootstrap_package_files -- \
		--ignored --exact --list )"; \
	printf '%s\n' "$$listed" | \
		grep -Fqx 'planner::hermetic_tests::bootstrap::fetch_pinned_bootstrap_package_files: test'
	@TMPDIR="$(BOOTSTRAP_TMP_DIR)" $(CARGO) test -p mason --lib \
		planner::hermetic_tests::bootstrap::fetch_pinned_bootstrap_package_files -- \
		--ignored --exact --nocapture

bootstrap-fixtures-offline: bootstrap-fixture-selection bootstrap-execution-requirement bootstrap-fixtures-tmp
	@echo "Requiring the complete verified bootstrap store; this lane performs no downloads..."
	@echo "Materializing the complete closure as a production-format offline root mirror..."
	@set -eu; \
	listed="$$( TMPDIR="$(BOOTSTRAP_TMP_DIR)" $(CARGO) test -p mason --lib \
		planner::hermetic_tests::bootstrap::contentful_bootstrap_materializes_a_complete_offline_root_mirror -- \
		--ignored --exact --list )"; \
	printf '%s\n' "$$listed" | \
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
		CAST_FIXTURE_EVIDENCE_DIR="$${CAST_FIXTURE_EVIDENCE_DIR:-$(TOP_DIR)/target/fixture-evidence}" \
		CAST_REQUIRE_EXECUTION="$(EXECUTION_REQUIREMENT)" \
		CARGO="$(CARGO)" \
		"$(TOP_DIR)/misc/scripts/run-delegated-execution-fixture.sh" "$(FIXTURE_SELECTION)"

delegated-fixture-runner-test: fixture-proof-test
	@"$(TOP_DIR)/misc/scripts/test-fixture-runtime-budgets.sh"
	@"$(TOP_DIR)/misc/scripts/test-stop-owned-fixture-unit.sh"
	@"$(TOP_DIR)/misc/scripts/test-latched-command-faults.sh"
	@"$(TOP_DIR)/misc/scripts/test-run-delegated-execution-fixture.sh"
	@"$(TOP_DIR)/misc/scripts/test-run-fixtures-ci-with-evidence.sh"

bootstrap-fixtures: bootstrap-fixture-selection bootstrap-execution-requirement bootstrap-fixtures-prepare
	@$(MAKE) --no-print-directory bootstrap-fixtures-offline \
		FIXTURE=$(FIXTURE_SELECTION) REQUIRE_EXECUTION=$(EXECUTION_REQUIREMENT)

fixtures-ci: execution-fixtures
	@$(MAKE) --no-print-directory bootstrap-fixtures-prepare
	@$(MAKE) --no-print-directory bootstrap-fixtures-offline REQUIRE_EXECUTION=1 FIXTURE=all

check: host-storage-safety-test make-shell-portability-test
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
