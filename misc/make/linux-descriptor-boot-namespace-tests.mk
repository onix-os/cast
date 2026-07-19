SHELL := /bin/bash

DESCRIPTOR_BOOT_NAMESPACE_TOP_DIR := $(abspath $(dir $(lastword $(MAKEFILE_LIST)))/../..)
CARGO ?= cargo

.PHONY: forge-linux-descriptor-boot-namespace-test

forge-linux-descriptor-boot-namespace-test: host-storage-safety-test
	@set -euo pipefail; \
	timeout 10s mkdir -p "$(DESCRIPTOR_BOOT_NAMESPACE_TOP_DIR)/target"; \
	listed="$$( timeout 10s mktemp "$(DESCRIPTOR_BOOT_NAMESPACE_TOP_DIR)/target/linux-descriptor-boot-namespace-test-list.XXXXXXXXXXXX" )"; \
	trap 'timeout 10s rm -f "$$listed"' EXIT; \
	timeout 300s $(CARGO) test --manifest-path "$(DESCRIPTOR_BOOT_NAMESPACE_TOP_DIR)/Cargo.toml" -p forge --lib -- --list | timeout 300s tee "$$listed" >/dev/null; \
	timeout 10s grep -q . "$$listed"; \
	prefix='linux_fs::tests::descriptor_boot_namespace::'; \
	timeout 10s test "$$( timeout 10s grep -Ec "^$$prefix.*: test$$" "$$listed" )" = 51; \
	for test_case in \
		classification:stable_missing_leaf_is_absent \
		classification:stable_missing_ancestor_marks_nested_request_absent \
		classification:stable_regular_bytes_are_exact_across_short_reads \
		classification:stable_nested_regular_bytes_are_exact \
		classification:stable_large_regular_crosses_fixed_stream_buffers \
		classification:stable_length_mismatch_is_different \
		classification:stable_equal_length_byte_mismatch_is_different \
		classification:shared_trie_preserves_original_request_order \
		protocol:nested_protocol_carries_indices_and_releases_in_lifo_order \
		protocol:failed_content_protocol_releases_every_retained_node \
		protocol:descriptor_limit_preflight_blocks_n_plus_one_lookup_and_unwinds \
		protocol:late_deadline_after_retained_callback_releases_root_state \
		aliases_and_types:raw_ascii_case_alias_is_rejected \
		aliases_and_types:kernel_short_alias_cannot_hide_a_different_raw_name \
		aliases_and_types:duplicate_raw_inventory_name_is_rejected \
		aliases_and_types:duplicate_inventory_identity_is_rejected \
		aliases_and_types:duplicate_requested_identity_across_directories_is_rejected \
		aliases_and_types:symlink_destination_is_rejected \
		aliases_and_types:wrong_ancestor_node_kind_is_rejected \
		aliases_and_types:cross_mount_lookup_is_rejected \
		aliases_and_types:lookup_kind_must_match_raw_inventory_kind \
		aliases_and_types:invalid_inventory_identity_is_rejected \
		races:absent_to_present_lookup_is_rejected_as_unstable_absence \
		races:lookup_absence_cannot_disagree_with_raw_inventory \
		races:changing_complete_inventory_is_rejected \
		races:changing_present_lookup_identity_is_rejected \
		races:changing_regular_witness_is_rejected \
		races:regular_witness_must_match_lookup_identity \
		races:actual_stream_must_match_stable_witness_digest \
		races:expected_stream_must_match_declared_digest \
		races:stalled_actual_and_expected_streams_are_rejected \
		bounds_and_deadlines:request_count_limit_rejects_n_plus_one \
		bounds_and_deadlines:path_and_component_limits_are_independent \
		bounds_and_deadlines:component_count_limit_rejects_n_plus_one \
		bounds_and_deadlines:hard_production_ceilings_reject_n_plus_one \
		bounds_and_deadlines:single_request_path_ceiling_accepts_4095_and_rejects_4096 \
		bounds_and_deadlines:aggregate_request_path_limit_accepts_exact_n_and_rejects_n_plus_one \
		bounds_and_deadlines:noncanonical_relative_requests_are_rejected \
		bounds_and_deadlines:exact_casefold_and_hierarchy_request_collisions_are_rejected \
		bounds_and_deadlines:directory_entry_and_total_entry_limits_are_independent \
		bounds_and_deadlines:raw_name_and_total_name_byte_limits_are_independent \
		bounds_and_deadlines:read_limit_accepts_exact_n_and_rejects_n_minus_one \
		bounds_and_deadlines:read_budget_preflight_blocks_data_and_eof_observer_calls \
		bounds_and_deadlines:work_limit_accepts_exact_n_and_rejects_n_minus_one \
		bounds_and_deadlines:inventory_sort_work_accepts_exact_n_and_rejects_n_minus_one \
		bounds_and_deadlines:allocation_limit_accepts_exact_n_and_rejects_n_minus_one \
		bounds_and_deadlines:descriptor_limit_accepts_exact_n_and_rejects_n_minus_one \
		bounds_and_deadlines:injected_allocation_and_observation_failures_are_typed \
		bounds_and_deadlines:zero_limits_are_rejected_before_namespace_observation \
		bounds_and_deadlines:deadline_equality_is_admitted_but_first_expired_checkpoint_fails \
		bounds_and_deadlines:terminal_deadline_checkpoint_catches_late_expiry; do \
		timeout 10s grep -Fqx "$$prefix$${test_case/:/::}: test" "$$listed"; \
	done; \
	module_root="$(DESCRIPTOR_BOOT_NAMESPACE_TOP_DIR)/crates/forge/src/linux_fs/descriptor_boot_namespace.rs"; \
	module_dir="$(DESCRIPTOR_BOOT_NAMESPACE_TOP_DIR)/crates/forge/src/linux_fs/descriptor_boot_namespace"; \
	test_root="$(DESCRIPTOR_BOOT_NAMESPACE_TOP_DIR)/crates/forge/src/linux_fs/tests/descriptor_boot_namespace.rs"; \
	test_dir="$(DESCRIPTOR_BOOT_NAMESPACE_TOP_DIR)/crates/forge/src/linux_fs/tests/descriptor_boot_namespace"; \
	files=( \
		"$$module_root" \
		"$$module_dir/budget.rs" \
		"$$module_dir/classifier.rs" \
		"$$module_dir/error.rs" \
		"$$module_dir/fixture.rs" \
		"$$module_dir/model.rs" \
		"$$module_dir/observer.rs" \
		"$$module_dir/trie.rs" \
		"$$test_root" \
		"$$test_dir/support.rs" \
		"$$test_dir/classification.rs" \
		"$$test_dir/protocol.rs" \
		"$$test_dir/aliases_and_types.rs" \
		"$$test_dir/races.rs" \
		"$$test_dir/bounds_and_deadlines.rs" \
	); \
	timeout 10s grep -Fq 'pub(crate) enum BootNamespaceDestinationState' "$$module_dir/model.rs"; \
	timeout 10s grep -Fq 'Absent,' "$$module_dir/model.rs"; \
	timeout 10s grep -Fq 'Exact,' "$$module_dir/model.rs"; \
	timeout 10s grep -Fq 'Different,' "$$module_dir/model.rs"; \
	timeout 10s grep -Fq 'pub(crate) struct ValidatedBootNamespaceAssessment' "$$module_dir/model.rs"; \
	timeout 10s grep -Fq 'try_reserve_exact(additional)' "$$module_dir/budget.rs"; \
	timeout 10s grep -Fq 'const STREAM_BUFFER_BYTES: usize = 4 * 1024;' "$$module_dir/budget.rs"; \
	timeout 10s grep -Fq 'const HARD_MAX_PATH_BYTES: usize = 4_095;' "$$module_dir/model.rs"; \
	timeout 10s grep -Fq 'const HARD_MAX_TOTAL_PATH_BYTES: usize = 8 * 1024 * 1024;' "$$module_dir/model.rs"; \
	timeout 10s grep -Fq 'charge_unstable_sort(entries.len()' "$$module_dir/classifier.rs"; \
	timeout 10s grep -Fq 'entries.sort_unstable_by(' "$$module_dir/classifier.rs"; \
	if timeout 10s rg -n '\.sort_by\(' "$$module_dir"; then exit 1; else status="$$?"; timeout 10s test "$$status" = 1; fi; \
	timeout 10s grep -Fq 'fn read_actual(' "$$module_dir/observer.rs"; \
	timeout 10s grep -Fq 'fn read_expected(' "$$module_dir/observer.rs"; \
	timeout 10s grep -Fq 'Xxh3::new()' "$$module_dir/classifier.rs"; \
	timeout 10s grep -Fq 'BootNamespaceObservationBoundary::Opening' "$$module_dir/classifier.rs"; \
	timeout 10s grep -Fq 'BootNamespaceObservationBoundary::Closing' "$$module_dir/classifier.rs"; \
	result_decl="$$( timeout 10s sed -n '/pub(crate) struct ValidatedBootNamespaceAssessment {/,/^}/p' "$$module_dir/model.rs" )"; \
	timeout 10s test -n "$$result_decl"; \
	if timeout 10s rg -n '\b(File|Path|OwnedFd|RawFd|BorrowedFd|AsRawFd|Reader|Writer)\b|fd:|descriptor:|closure|authority' <<<"$$result_decl"; then exit 1; else status="$$?"; timeout 10s test "$$status" = 1; fi; \
	if timeout 10s rg -n 'std::fs|tempfile|\b(Path|File|OpenOptions|OwnedFd|RawFd|BorrowedFd|AsRawFd)\b|nix::|libc::|File::open|OpenOptions|create_dir|read_dir|canonicalize|std::env|std::process|process::Command|Command::new|write_all|set_len|remove_(file|dir)|rename\(|mount\(|setns|unshare|chroot|pivot_root|umount|BLK[A-Z_]+|/dev/|/(boot|efi|esp)(/|`)' "$${files[@]}"; then exit 1; else status="$$?"; timeout 10s test "$$status" = 1; fi; \
	for file in "$${files[@]}" "$(DESCRIPTOR_BOOT_NAMESPACE_TOP_DIR)/misc/make/linux-descriptor-boot-namespace-tests.mk"; do \
		timeout 10s test "$$( timeout 10s wc -l < "$$file" )" -le 1000; \
	done; \
	timeout 1200s $(CARGO) test --manifest-path "$(DESCRIPTOR_BOOT_NAMESPACE_TOP_DIR)/Cargo.toml" -p forge --lib "$$prefix" -- --test-threads=1
