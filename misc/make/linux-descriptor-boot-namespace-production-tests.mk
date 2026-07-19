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
	timeout 10s test "$$( timeout 10s grep -Ec "^$$prefix.*: test$$" "$$listed" )" = 12; \
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
		bounds_and_deadlines:deadline_equality_is_admitted_and_expiry_is_checked_around_source_calls; do \
		timeout 10s grep -Fqx "$$prefix$${test_case/:/::}: test" "$$listed"; \
	done; \
	module_root="$(DESCRIPTOR_BOOT_NAMESPACE_PRODUCTION_TOP_DIR)/crates/forge/src/linux_fs/descriptor_boot_namespace/production.rs"; \
	module_dir="$(DESCRIPTOR_BOOT_NAMESPACE_PRODUCTION_TOP_DIR)/crates/forge/src/linux_fs/descriptor_boot_namespace/production"; \
	test_root="$(DESCRIPTOR_BOOT_NAMESPACE_PRODUCTION_TOP_DIR)/crates/forge/src/linux_fs/tests/descriptor_boot_namespace_production.rs"; \
	test_dir="$(DESCRIPTOR_BOOT_NAMESPACE_PRODUCTION_TOP_DIR)/crates/forge/src/linux_fs/tests/descriptor_boot_namespace_production"; \
	files=( \
		"$$module_root" \
		"$$module_dir/budget.rs" \
		"$$module_dir/error.rs" \
		"$$module_dir/inventory.rs" \
		"$$module_dir/model.rs" \
		"$$module_dir/parser.rs" \
		"$$module_dir/source.rs" \
		"$$test_root" \
		"$$test_dir/support.rs" \
		"$$test_dir/records.rs" \
		"$$test_dir/bounds_and_deadlines.rs" \
	); \
	timeout 10s grep -Fq 'const RECORD_LENGTH_OFFSET: usize = 16;' "$$module_dir/parser.rs"; \
	timeout 10s grep -Fq 'const RAW_NAME_OFFSET: usize = 19;' "$$module_dir/parser.rs"; \
	timeout 10s grep -Fq 'const RAW_DIRECTORY_RECORD_ALIGNMENT_BYTES: usize = size_of::<usize>();' "$$module_dir/model.rs"; \
	timeout 10s grep -Fq 'const RAW_DIRECTORY_MAXIMUM_NAME_BYTES: usize = 255;' "$$module_dir/model.rs"; \
	timeout 10s grep -Fq 'raw_name != b"." && raw_name != b".."' "$$module_dir/parser.rs"; \
	timeout 10s grep -Fq 'try_reserve_exact(additional_items)' "$$module_dir/budget.rs"; \
	timeout 10s grep -Fq '.read_chunk(&mut output[..offered])' "$$module_dir/budget.rs"; \
	timeout 10s grep -Fq '.probe_end(&mut output[..offered])' "$$module_dir/budget.rs"; \
	timeout 10s grep -Fq 'pub(crate) struct ProductionRawDirectoryInventory' "$$module_dir/inventory.rs"; \
	if timeout 10s rg -n 'std::fs|tempfile|OpenOptions|File::open|Path::|AT_FDCWD|openat|openat2|read_dir|canonicalize|create_dir|write_all|set_len|remove_(file|dir)|rename\(|mount\(|setns|unshare|chroot|pivot_root|umount|/dev/|/(boot|efi|esp)(/|`)' "$${files[@]}"; then exit 1; else status="$$?"; timeout 10s test "$$status" = 1; fi; \
	for file in "$${files[@]}" "$(DESCRIPTOR_BOOT_NAMESPACE_PRODUCTION_TOP_DIR)/misc/make/linux-descriptor-boot-namespace-production-tests.mk"; do \
		timeout 10s test "$$( timeout 10s wc -l < "$$file" )" -le 1000; \
	done; \
	timeout 1200s $(CARGO) test --manifest-path "$(DESCRIPTOR_BOOT_NAMESPACE_PRODUCTION_TOP_DIR)/Cargo.toml" -p forge --lib "$$prefix" -- --test-threads=1
