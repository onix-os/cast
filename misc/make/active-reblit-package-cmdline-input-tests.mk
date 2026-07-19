SHELL := /bin/bash

PACKAGE_CMDLINE_INPUT_TOP_DIR := $(abspath $(dir $(lastword $(MAKEFILE_LIST)))/../..)
CARGO ?= cargo

.PHONY: forge-active-reblit-package-cmdline-input-test

forge-active-reblit-package-cmdline-input-test: host-storage-safety-test
	@set -euo pipefail; \
	timeout 10s mkdir -p "$(PACKAGE_CMDLINE_INPUT_TOP_DIR)/target"; \
	listed="$$( timeout 10s mktemp "$(PACKAGE_CMDLINE_INPUT_TOP_DIR)/target/active-reblit-package-cmdline-list.XXXXXXXXXXXX" )"; \
	trap 'timeout 10s rm -f "$$listed"' EXIT; \
	timeout 300s $(CARGO) test --manifest-path "$(PACKAGE_CMDLINE_INPUT_TOP_DIR)/Cargo.toml" -p forge --lib -- --list | timeout 300s tee "$$listed" >/dev/null; \
	timeout 10s grep -q . "$$listed"; \
	prefix='client::active_reblit_package_cmdline_inputs::tests::'; \
	timeout 10s test "$$( timeout 10s grep -Ec "^$$prefix.*: test$$" "$$listed" )" = 19; \
	for name in \
		semantics::semantic_inputs_are_state_scoped_versioned_sorted_and_normalized \
		semantics::non_cmdline_assets_never_enter_semantic_inputs \
		semantics::empty_and_comment_only_sources_are_retained_canonically \
		semantics::non_ascii_and_embedded_controls_fail_closed \
		semantics::preparation_uses_explicit_offsets_without_moving_the_shared_snapshot_cursor \
		source_binding::every_semantic_entry_rebinds_to_the_exact_retained_stone_owner \
		source_binding::substituted_binding_index_is_rejected_by_exact_coordinate_revalidation \
		source_binding::changed_digest_length_path_or_scope_is_rejected \
		source_binding::changed_normalized_semantics_are_rejected_after_source_reauthentication \
		source_binding::digest_mismatch_and_short_explicit_offset_read_fail_closed \
		bounds_and_deadlines::entry_and_per_source_bounds_admit_n_and_reject_n_plus_one \
		bounds_and_deadlines::aggregate_source_bound_counts_every_reference_to_a_shared_snapshot \
		bounds_and_deadlines::exact_preparation_work_is_admitted_and_n_plus_one_is_rejected \
		bounds_and_deadlines::expired_caller_deadline_is_rejected_at_entry_and_revalidation \
		bounds_and_deadlines::one_caller_deadline_is_not_replaced_between_sources_or_at_terminal_return \
		bounds_and_deadlines::normalized_scalar_deadline_is_checked_after_materialization \
		bounds_and_deadlines::production_limits_and_sort_reservation_are_explicit \
		bounds_and_deadlines::interrupted_reads_admit_the_exact_retry_limit_and_reject_the_next_attempt \
		bounds_and_deadlines::adversarial_last_state_position_lookup_is_precomputed_and_charged_exactly; do \
		timeout 10s grep -Fqx "$$prefix$$name: test" "$$listed"; \
	done; \
	root="$(PACKAGE_CMDLINE_INPUT_TOP_DIR)/crates/forge/src/client/boot/active_reblit_package_cmdline_inputs.rs"; \
	core="$(PACKAGE_CMDLINE_INPUT_TOP_DIR)/crates/forge/src/client/boot/active_reblit_package_cmdline_inputs"; \
	tests="$(PACKAGE_CMDLINE_INPUT_TOP_DIR)/crates/forge/src/client/boot/active_reblit_package_cmdline_inputs_tests.rs"; \
	test_core="$(PACKAGE_CMDLINE_INPUT_TOP_DIR)/crates/forge/src/client/boot/active_reblit_package_cmdline_inputs_tests"; \
	timeout 10s grep -Fq "pub(in crate::client) struct PreparedActiveReblitPackageCmdlineInputs<'stone>" "$$root"; \
	timeout 10s grep -Fq "source_owner: &'stone PreparedActiveReblitStoneBootInputs" "$$root"; \
	timeout 10s grep -Fq 'pub(in crate::client) fn prepare_until' "$$root"; \
	timeout 10s grep -Fq 'nix::libc::pread' "$$core/binding.rs"; \
	timeout 10s grep -Fq 'xxhash_rust::xxh3::xxh3_128' "$$core/binding.rs"; \
	timeout 10s grep -Fq 'normalize_package_cmdline' "$$core/normalization.rs"; \
	if timeout 10s rg -n 'impl Clone for PreparedActiveReblitPackageCmdlineInputs|std::process|process::Command|Command::new|blsforme|nix::mount|libc::mount|mount_partitions|canonicalize\(|std::fs|fs_err|OpenOptions|create_dir|create_dir_all|rename\(|remove_file|remove_dir|(?:fs::|File::)write\(|BLK[A-Z_]+|/dev/(disk|sd|hd|vd|xvd|nvme|mmcblk|loop|md|dm-|nbd|zram)' "$$root" "$$core"; then exit 1; else status="$$?"; timeout 10s test "$$status" = 1; fi; \
	host_root_pattern='/''(boot|efi|esp)(/|["[:space:]]|$$)'; \
	if timeout 10s rg -n "$$host_root_pattern" "$$root" "$$core"; then exit 1; else status="$$?"; timeout 10s test "$$status" = 1; fi; \
	if timeout 10s rg --pcre2 -U -n '#\[derive\([^\]]*Clone[^\]]*\)\]\s*pub\(in crate::client\) struct PreparedActiveReblitPackageCmdlineInputs' "$$root"; then exit 1; else status="$$?"; timeout 10s test "$$status" = 1; fi; \
	for file in "$$root" "$$core"/*.rs "$$tests" "$$test_core"/*.rs "$(PACKAGE_CMDLINE_INPUT_TOP_DIR)/misc/make/active-reblit-package-cmdline-input-tests.mk"; do \
		timeout 10s test "$$( timeout 10s wc -l < "$$file" )" -le 1000; \
	done; \
	timeout 1200s $(CARGO) test --manifest-path "$(PACKAGE_CMDLINE_INPUT_TOP_DIR)/Cargo.toml" -p forge --lib "$$prefix" -- --test-threads=1
