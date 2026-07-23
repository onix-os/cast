.PHONY: forge-active-reblit-boot-state-root-test

forge-active-reblit-boot-state-root-test:
	@set -euo pipefail; \
	timeout 10s mkdir -p "$(TOP_DIR)/target"; \
	listed="$$( timeout 10s mktemp "$(TOP_DIR)/target/active-reblit-boot-state-roots-list.XXXXXXXXXXXX" )"; \
	trap 'timeout 10s rm -f "$$listed"' EXIT; \
	timeout 300s $(CARGO) test -p forge --lib -- --list | timeout 300s tee "$$listed" >/dev/null; \
	timeout 10s grep -q . "$$listed"; \
	prefix='transition_identity::active_reblit_boot_state_roots::tests::'; \
	timeout 10s test "$$( timeout 10s grep -Ec "^$$prefix.*: test$$" "$$listed" )" = 20; \
	for name in \
		bounds_and_read_only::projection_count_head_order_duplicate_and_positive_id_bounds_are_typed \
		bounds_and_read_only::diagnostic_path_byte_and_component_bounds_admit_n_and_reject_n_plus_one \
		bounds_and_read_only::exact_head_work_boundary_admits_n_and_rejects_n_minus_one \
		bounds_and_read_only::expired_deadline_fails_before_state_root_admission \
		bounds_and_read_only::caller_owned_deadline_is_rejected_at_prepare_and_revalidate_entry \
		bounds_and_read_only::caller_owned_deadline_is_rechecked_after_prepared_and_view_materialization \
		bounds_and_read_only::preparation_revalidation_and_bound_views_are_read_only \
		exclusions_and_revalidation::absent_archive_remains_excluded_when_an_exact_wrapper_appears_later \
		exclusions_and_revalidation::inexact_archive_remains_excluded_after_its_layout_is_repaired \
		exclusions_and_revalidation::admitted_archive_substitution_is_a_hard_revalidation_failure \
		exclusions_and_revalidation::archive_substitution_between_global_revalidation_passes_is_caught \
		exclusions_and_revalidation::intermediate_permission_failure_is_never_downgraded_to_an_archive_exclusion \
		exact_head_and_order::mandatory_head_descriptor_is_exactly_bound_to_the_named_live_usr \
		exact_head_and_order::mandatory_head_missing_marker_is_a_hard_failure \
		exact_head_and_order::eligible_roots_preserve_projected_archive_order_and_kind \
		runtime_and_identity::duplicate_permanent_tree_token_across_states_is_a_hard_failure \
		runtime_and_identity::operational_and_retry_errors_are_never_structural_exclusions \
		runtime_and_identity::retained_archived_slot_marker_reads_preserve_atime \
		runtime_and_identity::retained_state_id_reads_preserve_atime \
		runtime_and_identity::runtime_epoch_change_blocks_descriptor_view_creation; do \
		timeout 10s grep -Fqx "$$prefix$$name: test" "$$listed"; \
	done; \
	timeout 10s grep -Fqx 'tree_marker::integrity::tests::interrupted_read_budget_admits_n_and_rejects_n_plus_one: test' "$$listed"; \
	tests=crates/forge/src/transition_identity/active_reblit_boot_state_roots_tests; \
	for module in bounds_and_read_only exclusions_and_revalidation exact_head_and_order runtime_and_identity support; do \
		timeout 10s grep -Fqx "#[path = \"active_reblit_boot_state_roots_tests/$$module.rs\"]" crates/forge/src/transition_identity/active_reblit_boot_state_roots_tests.rs; \
		timeout 10s grep -Fqx "mod $$module;" crates/forge/src/transition_identity/active_reblit_boot_state_roots_tests.rs; \
	done; \
	for file in \
		crates/forge/src/transition_identity/active_reblit_boot_state_roots.rs \
		crates/forge/src/transition_identity/active_reblit_boot_state_roots/*.rs \
		crates/forge/src/transition_identity/active_reblit_boot_state_roots_tests.rs \
		"$$tests"/*.rs \
		crates/forge/src/transition_identity/archived_state_identity.rs \
		crates/forge/src/transition_identity/state_tree_metadata.rs \
		crates/forge/src/tree_marker/integrity.rs \
		crates/forge/src/tree_marker/integrity_tests.rs \
		misc/make/active-reblit-boot-state-root-tests.mk; do \
		timeout 10s test "$$( timeout 10s wc -l < "$$file" )" -le 1000; \
	done; \
	timeout 900s $(CARGO) test -p forge --lib "$$prefix" -- --test-threads=1; \
	timeout 60s $(CARGO) test -p forge --lib tree_marker::integrity::tests::interrupted_read_budget_admits_n_and_rejects_n_plus_one -- --exact --test-threads=1
