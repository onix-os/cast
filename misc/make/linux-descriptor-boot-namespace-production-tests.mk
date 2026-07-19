SHELL := /bin/bash

DESCRIPTOR_BOOT_NAMESPACE_PRODUCTION_TOP_DIR := $(abspath $(dir $(lastword $(MAKEFILE_LIST)))/../..)
CARGO ?= cargo

.PHONY: forge-linux-descriptor-boot-namespace-production-test

forge-linux-descriptor-boot-namespace-production-test: host-storage-safety-test
	@set -euo pipefail; \
	timeout 10s mkdir -p "$(DESCRIPTOR_BOOT_NAMESPACE_PRODUCTION_TOP_DIR)/target"; \
	listed="$$( timeout 10s mktemp "$(DESCRIPTOR_BOOT_NAMESPACE_PRODUCTION_TOP_DIR)/target/linux-descriptor-boot-namespace-production-test-list.XXXXXXXXXXXX" )"; \
	trap 'timeout 10s rm -f "$$listed"' EXIT; \
	timeout 300s $(CARGO) test --manifest-path "$(DESCRIPTOR_BOOT_NAMESPACE_PRODUCTION_TOP_DIR)/Cargo.toml" -p forge --lib -- --list | timeout 300s tee "$$listed" >/dev/null; \
	timeout 10s grep -q . "$$listed"; \
	prefix='linux_fs::tests::descriptor_boot_namespace_production::'; \
	timeout 10s test "$$( timeout 10s grep -Ec "^$$prefix.*: test$$" "$$listed" )" = 34; \
	for test_case in \
		records:valid_inventory_filters_dot_entries_and_ignores_kernel_identity_hints \
		records:complete_syscall_chunks_preserve_raw_record_order \
		records:truncated_headers_and_records_split_across_chunks_are_rejected \
		records:too_small_and_unaligned_record_lengths_are_rejected \
		records:missing_empty_overlong_and_slash_names_are_rejected \
		records:source_failure_and_impossible_return_count_are_typed \
		bounds_and_deadlines:zero_and_above_ceiling_limits_fail_before_source_observation \
		bounds_and_deadlines:record_and_name_ledgers_accept_exact_n_and_reject_n_minus_one \
		bounds_and_deadlines:read_byte_and_call_ledgers_include_the_terminal_eof_call \
		bounds_and_deadlines:work_ledger_accepts_exact_n_and_rejects_n_minus_one \
		bounds_and_deadlines:allocation_ledgers_and_injected_failure_are_fail_closed \
		bounds_and_deadlines:deadline_equality_is_admitted_and_expiry_is_checked_around_source_calls \
		live:injected_driver_receives_one_bounded_call_per_source_call \
		live:interrupted_syscall_is_failed_closed_without_retry \
		live:deadline_expiry_during_call_is_detected_immediately_after_return \
		live:native_getdents64_reads_only_an_ordinary_target_fixture \
		retained:ordinary_target_fixture_classifies_exact_different_and_absent \
		retained:nonempty_result_exposes_exact_retained_root_identity \
		retained:empty_result_has_no_observed_root_identity \
		retained:root_protocol_failure_cannot_emit_validated_result \
		retained:every_inventory_pass_starts_from_a_fresh_offset_zero_description \
		retained:nested_nodes_release_in_strict_lifo_order_before_completion \
		retained:injected_change_after_opening_hash_is_failed_closed \
		retained:aggregate_inventory_pass_budget_accepts_n_and_rejects_n_minus_one \
		retained:live_observation_io_attempt_budget_accepts_exact_n_and_rejects_n_minus_one \
		retained:descriptor_slot_budget_accepts_exact_peak_and_rejects_one_less \
		retained:failed_open_reserves_and_releases_one_descriptor_slot \
		retained:empty_request_uses_no_descriptor_slots \
		retained:logical_node_budget_remains_separate_from_descriptor_slots \
		retained:late_deadline_after_opening_lookup_releases_retained_descriptors \
		retained:hook_failure_after_child_open_is_not_masked_by_cleanup \
		retained:size_mismatch_skips_all_content_hashes_and_actual_reads \
		retained:expected_digest_mismatch_fails_before_root_observation \
		retained:non_opath_root_is_rejected_without_fallback; do \
		timeout 10s grep -Fqx "$$prefix$${test_case/:/::}: test" "$$listed"; \
	done; \
	module_root="$(DESCRIPTOR_BOOT_NAMESPACE_PRODUCTION_TOP_DIR)/crates/forge/src/linux_fs/descriptor_boot_namespace/production.rs"; \
	module_dir="$(DESCRIPTOR_BOOT_NAMESPACE_PRODUCTION_TOP_DIR)/crates/forge/src/linux_fs/descriptor_boot_namespace/production"; \
	test_root="$(DESCRIPTOR_BOOT_NAMESPACE_PRODUCTION_TOP_DIR)/crates/forge/src/linux_fs/tests/descriptor_boot_namespace_production.rs"; \
	test_dir="$(DESCRIPTOR_BOOT_NAMESPACE_PRODUCTION_TOP_DIR)/crates/forge/src/linux_fs/tests/descriptor_boot_namespace_production"; \
	production_files=( \
		"$$module_root" \
		"$$module_dir/budget.rs" \
		"$$module_dir/error.rs" \
		"$$module_dir/inventory.rs" \
		"$$module_dir/live.rs" \
		"$$module_dir/live/abi.rs" \
		"$$module_dir/live/source.rs" \
		"$$module_dir/live/syscall.rs" \
		"$$module_dir/model.rs" \
		"$$module_dir/parser.rs" \
		"$$module_dir/retained.rs" \
		"$$module_dir/retained/content.rs" \
		"$$module_dir/retained/error.rs" \
		"$$module_dir/retained/hook.rs" \
		"$$module_dir/retained/inventory.rs" \
		"$$module_dir/retained/limits.rs" \
		"$$module_dir/retained/node.rs" \
		"$$module_dir/retained/observer.rs" \
		"$$module_dir/retained/syscall.rs" \
		"$$module_dir/source.rs" \
	); \
	pure_test_files=( \
		"$$test_root" \
		"$$test_dir/support.rs" \
		"$$test_dir/records.rs" \
		"$$test_dir/bounds_and_deadlines.rs" \
	); \
	live_test_files=( \
		"$$test_dir/live.rs" \
	); \
	retained_test_files=( \
		"$$test_dir/retained.rs" \
	); \
	test_files=( "$${pure_test_files[@]}" "$${live_test_files[@]}" "$${retained_test_files[@]}" ); \
	files=( "$${production_files[@]}" "$${test_files[@]}" ); \
	timeout 10s grep -Fq 'const RECORD_LENGTH_OFFSET: usize = 16;' "$$module_dir/parser.rs"; \
	timeout 10s grep -Fq 'const RAW_NAME_OFFSET: usize = 19;' "$$module_dir/parser.rs"; \
	timeout 10s grep -Fq 'const RAW_DIRECTORY_RECORD_ALIGNMENT_BYTES: usize = size_of::<usize>();' "$$module_dir/model.rs"; \
	timeout 10s grep -Fq 'const RAW_DIRECTORY_MAXIMUM_NAME_BYTES: usize = 255;' "$$module_dir/model.rs"; \
	timeout 10s grep -Fq 'raw_name != b"." && raw_name != b".."' "$$module_dir/parser.rs"; \
	timeout 10s grep -Fq 'try_reserve_exact(additional_items)' "$$module_dir/budget.rs"; \
	timeout 10s grep -Fq '.read_chunk(&mut output[..offered])' "$$module_dir/budget.rs"; \
	timeout 10s grep -Fq '.probe_end(&mut output[..offered])' "$$module_dir/budget.rs"; \
	timeout 10s grep -Fq 'pub(crate) struct ProductionRawDirectoryInventory' "$$module_dir/inventory.rs"; \
	timeout 10s grep -Fq 'nix::libc::SYS_getdents64' "$$module_dir/live/syscall.rs"; \
	timeout 10s grep -Fq 'offset_of!(NativeLinuxDirent64Prefix, name) == 19' "$$module_dir/live/abi.rs"; \
	timeout 10s grep -Fq 'tempdir_in(&target)' "$$test_dir/live.rs"; \
	timeout 10s grep -Fq 'RESOLVE_BENEATH | nix::libc::RESOLVE_NO_MAGICLINKS | nix::libc::RESOLVE_NO_SYMLINKS' "$$module_dir/retained/node.rs"; \
	timeout 10s grep -Fq 'RETAINED_LOOKUP_RESOLUTION & nix::libc::RESOLVE_NO_XDEV as u64 == 0' "$$module_dir/retained/node.rs"; \
	timeout 10s grep -Fq 'open_relative(directory, c".", DIRECTORY_READ_FLAGS' "$$module_dir/retained/node.rs"; \
	timeout 10s grep -Fq 'nix::libc::O_NOATIME' "$$module_dir/retained/node.rs"; \
	timeout 10s grep -Fq 'nix::libc::pread(' "$$module_dir/retained/syscall.rs"; \
	timeout 10s grep -Fq 'nix::libc::SYS_statx' "$$module_dir/retained/syscall.rs"; \
	timeout 10s grep -Fq 'nix::libc::AT_EMPTY_PATH' "$$module_dir/retained/syscall.rs"; \
	timeout 10s grep -Fq 'status.stx_mask & nix::libc::STATX_MNT_ID == 0' "$$module_dir/retained/syscall.rs"; \
	timeout 10s grep -Fq 'pub(crate) max_observation_io_attempts: usize' "$$module_dir/retained/limits.rs"; \
	timeout 10s grep -Fq 'pub(crate) observation_io_attempts: usize' "$$module_dir/retained/limits.rs"; \
	timeout 10s grep -Fq 'pub(crate) max_descriptor_slots: usize' "$$module_dir/retained/limits.rs"; \
	timeout 10s grep -Fq 'pub(crate) peak_descriptor_slots: usize' "$$module_dir/retained/limits.rs"; \
	timeout 10s grep -Fq 'eof_probe_capacity_bytes' "$$module_dir/retained/limits.rs"; \
	timeout 10s grep -Fq 'pub(crate) struct ValidatedRetainedBootNamespaceAssessment' "$$module_dir/retained.rs"; \
	timeout 10s grep -Fq 'observed_root_identity: Option<BootNamespaceNodeIdentity>' "$$module_dir/retained.rs"; \
	timeout 10s grep -Fq 'successful nonempty classification omitted retained-root evidence' "$$module_dir/retained.rs"; \
	timeout 10s grep -Fq 'self.observed_root_identity = Some(identity);' "$$module_dir/retained/observer.rs"; \
	if timeout 10s rg -n 'F_DUPFD|dup3?\(|try_clone\(' "$$module_dir/retained.rs" "$$module_dir/retained"; then exit 1; else status="$$?"; timeout 10s test "$$status" = 1; fi; \
	if timeout 10s rg -n 'descriptor_mount_id_until|retry_interrupted|openat2_file_until|AT_FDCWD|/proc/' "$$module_dir/retained.rs" "$$module_dir/retained"; then exit 1; else status="$$?"; timeout 10s test "$$status" = 1; fi; \
	if timeout 10s rg -n 'max_syscalls|\bsyscalls\b|max_descriptors|peak_descriptors|physical descriptor' "$$module_dir/retained.rs" "$$module_dir/retained" "$$test_dir/retained.rs"; then exit 1; else status="$$?"; timeout 10s test "$$status" = 1; fi; \
	if timeout 10s rg -n 'ProductionBorrowedLinuxRawDirectorySource|read_borrowed_fresh_linux_directory_with_usage_until' "$$module_dir/live.rs" "$$module_dir/live/source.rs"; then exit 1; else status="$$?"; timeout 10s test "$$status" = 1; fi; \
	if timeout 10s rg -n 'tempfile|OpenOptions|File::open|Path::|AT_FDCWD|read_dir|canonicalize|create_dir|write_all|set_len|remove_(file|dir)|rename\(|mount\(|setns|unshare|chroot|pivot_root|umount|/dev/|/(boot|efi|esp)(/|`)' "$${production_files[@]}"; then exit 1; else status="$$?"; timeout 10s test "$$status" = 1; fi; \
	if timeout 10s rg -n 'std::fs|tempfile|OpenOptions|File::open|Path::|AT_FDCWD|openat|openat2|read_dir|canonicalize|create_dir|write_all|set_len|remove_(file|dir)|rename\(|mount\(|setns|unshare|chroot|pivot_root|umount|/dev/|/(boot|efi|esp)(/|`)' "$${pure_test_files[@]}"; then exit 1; else status="$$?"; timeout 10s test "$$status" = 1; fi; \
	if timeout 10s rg -n 'tempdir\(|OpenOptions|AT_FDCWD|openat|openat2|read_dir|canonicalize|write_all|set_len|remove_(file|dir)|rename\(|mount\(|setns|unshare|chroot|pivot_root|umount|/dev/|/(boot|efi|esp)(/|`)' "$${live_test_files[@]}"; then exit 1; else status="$$?"; timeout 10s test "$$status" = 1; fi; \
	if timeout 10s rg -n 'AT_FDCWD|read_dir|canonicalize|set_len|remove_(file|dir)|rename\(|mount\(|setns|unshare|chroot|pivot_root|umount|/dev/|/(boot|efi|esp)(/|`)' "$${retained_test_files[@]}"; then exit 1; else status="$$?"; timeout 10s test "$$status" = 1; fi; \
	if timeout 10s rg -n '/dev/|/(boot|efi|esp)(/|`)|mount\(|setns|unshare|chroot|pivot_root|umount' "$${test_files[@]}"; then exit 1; else status="$$?"; timeout 10s test "$$status" = 1; fi; \
	for file in "$${files[@]}" "$(DESCRIPTOR_BOOT_NAMESPACE_PRODUCTION_TOP_DIR)/misc/make/linux-descriptor-boot-namespace-production-tests.mk"; do \
		timeout 10s test "$$( timeout 10s wc -l < "$$file" )" -le 1000; \
	done; \
	timeout 1200s $(CARGO) test --manifest-path "$(DESCRIPTOR_BOOT_NAMESPACE_PRODUCTION_TOP_DIR)/Cargo.toml" -p forge --lib "$$prefix" -- --test-threads=1
