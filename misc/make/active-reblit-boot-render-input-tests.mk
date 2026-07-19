SHELL := /bin/bash

BOOT_RENDER_INPUT_TOP_DIR := $(abspath $(dir $(lastword $(MAKEFILE_LIST)))/../..)
CARGO ?= cargo

.PHONY: forge-active-reblit-boot-render-input-test

forge-active-reblit-boot-render-input-test: host-storage-safety-test
	@set -euo pipefail; \
	timeout 10s mkdir -p "$(BOOT_RENDER_INPUT_TOP_DIR)/target"; \
	listed="$$( timeout 10s mktemp "$(BOOT_RENDER_INPUT_TOP_DIR)/target/active-reblit-boot-render-input-list.XXXXXXXXXXXX" )"; \
	trap 'timeout 10s rm -f "$$listed"' EXIT; \
	timeout 300s $(CARGO) test --manifest-path "$(BOOT_RENDER_INPUT_TOP_DIR)/Cargo.toml" -p forge --lib -- --list | timeout 300s tee "$$listed" >/dev/null; \
	timeout 10s grep -q . "$$listed"; \
	prefix='client::active_reblit_boot_render_inputs::tests::'; \
	timeout 10s test "$$( timeout 10s grep -Ec "^$$prefix.*: test$$" "$$listed" )" = 11; \
	for name in \
		ownership::exact_owners_coordinates_and_namespace_are_retained_without_mutation \
		state_join::excluded_history_is_not_promoted_from_stone_or_schema_history \
		state_join::sole_excluded_history_kernel_cannot_produce_an_empty_render_attempt \
		cmdline_semantics::exact_state_version_scope_masks_and_source_order_are_deterministic \
		cmdline_semantics::same_name_regular_local_entry_appends_instead_of_overriding_package_data \
		reserved_keys::masked_package_root_and_cast_keys_are_rejected_before_masking \
		reserved_keys::package_and_local_quotes_or_backslashes_fail_with_exact_origin \
		bounds_and_deadlines::command_line_bytes_admit_2047_reject_2048_before_output_and_bound_aggregate_bytes \
		bounds_and_deadlines::command_line_tokens_admit_1024_reject_1025_and_bound_aggregate_tokens \
		bounds_and_deadlines::expired_entry_and_injected_terminal_deadlines_fail_closed \
		bounds_and_deadlines::trailing_stone_sandwich_rejects_database_change_after_semantic_materialization; do \
		timeout 10s grep -Fqx "$$prefix$$name: test" "$$listed"; \
	done; \
	root="$(BOOT_RENDER_INPUT_TOP_DIR)/crates/forge/src/client/boot/active_reblit_boot_render_inputs.rs"; \
	core="$(BOOT_RENDER_INPUT_TOP_DIR)/crates/forge/src/client/boot/active_reblit_boot_render_inputs"; \
	tests="$(BOOT_RENDER_INPUT_TOP_DIR)/crates/forge/src/client/boot/active_reblit_boot_render_inputs_tests.rs"; \
	test_core="$(BOOT_RENDER_INPUT_TOP_DIR)/crates/forge/src/client/boot/active_reblit_boot_render_inputs_tests"; \
	timeout 10s grep -Fq "struct PreparedActiveReblitBootRenderInputs<'stone, 'roots>" "$$root"; \
	timeout 10s grep -Fq "source_owner: &'stone PreparedActiveReblitStoneBootInputs" "$$root"; \
	timeout 10s grep -Fq "roots_owner: &'roots PreparedActiveReblitBootStateRoots" "$$root"; \
	if timeout 10s rg -n 'fn (source_owner|roots_owner)\(' "$$root" "$$core"; then exit 1; else status="$$?"; timeout 10s test "$$status" = 1; fi; \
	timeout 10s grep -Fq "struct RevalidatedActiveReblitBootRenderInputs<'attempt, 'stone, 'roots>" "$$root"; \
	timeout 10s grep -Fq "impl<'attempt, 'stone, 'roots> RevalidatedActiveReblitBootRenderInputs<'attempt, 'stone, 'roots>" "$$root"; \
	timeout 10s grep -Fq "pub(in crate::client) fn kernels<'a>(" "$$root"; \
	timeout 10s grep -Fq "Item = BoundActiveReblitKernelRenderInput<'a>" "$$root"; \
	timeout 10s grep -Fq "impl<'a> BoundActiveReblitKernelRenderInput<'a>" "$$root"; \
	timeout 10s grep -Fq "fn kernel_asset(&self) -> BoundActiveReblitBootAsset<'a>" "$$root"; \
	timeout 10s grep -Fq "Item = BoundActiveReblitInitrdRenderInput<'a>" "$$root"; \
	timeout 10s grep -Fq "impl<'a> BoundActiveReblitInitrdRenderInput<'a>" "$$root"; \
	timeout 10s grep -Fq "fn asset(&self) -> BoundActiveReblitBootAsset<'a>" "$$root"; \
	timeout 10s grep -Fq "fn kernel_asset_from_temporary_view<'a, 'attempt, 'stone, 'roots>(" "$$test_core/ownership.rs"; \
	timeout 10s grep -Fq "fn initrd_asset_from_temporary_views<'a, 'attempt, 'stone, 'roots>(" "$$test_core/ownership.rs"; \
	timeout 10s grep -Fq 'deadline: Instant' "$$root"; \
	timeout 10s grep -Fq 'pub(in crate::client) fn deadline(&self) -> Instant' "$$root"; \
	timeout 10s grep -Fq "struct BoundActiveReblitInitrdRenderInput<'a>" "$$root"; \
	timeout 10s grep -Fq 'pub(in crate::client) fn binding_index(&self) -> u16' "$$root"; \
	timeout 10s grep -Fq 'pub(in crate::client) fn logical_basename(&self) -> &OsStr' "$$root"; \
	timeout 10s grep -Fq 'prepared.source_owner.revalidate_until(state_db, layout_db, deadline)?;' "$$root"; \
	timeout 10s test "$$( timeout 10s grep -Fc 'prepared.source_owner.revalidate_until(state_db, layout_db, deadline)?;' "$$root" )" = 2; \
	timeout 10s grep -Fq 'admitted(cmdline_bytes, selected.len());' "$$core/cmdline.rs"; \
	if timeout 10s rg --pcre2 -U -n 'impl Clone for ((Prepared|Revalidated)ActiveReblitBootRenderInputs|BoundActiveReblitInitrdRenderInput)|#\[derive\([^]]*Clone[^]]*\)\]\s*(pub\(in crate::client\) )?struct ((Prepared|Revalidated)ActiveReblitBootRenderInputs|BoundActiveReblitInitrdRenderInput)|blsforme|active_reblit_publication_plan|active_reblit_mounted_boot_topology|std::process|process::Command|Command::new|nix::mount|libc::mount|mount_partitions|canonicalize\(|OpenOptions|create_dir|create_dir_all|rename\(|remove_file|remove_dir|(?:fs::|File::)write\(|BLK[A-Z_]+|/dev/(disk|sd|hd|vd|xvd|nvme|mmcblk|loop|md|dm-|nbd|zram)' "$$root" "$$core"; then exit 1; else status="$$?"; timeout 10s test "$$status" = 1; fi; \
	host_root_pattern='/''(boot|efi|esp)(/|["[:space:]]|$$)'; \
	if timeout 10s rg -n "$$host_root_pattern" "$$root" "$$core"; then exit 1; else status="$$?"; timeout 10s test "$$status" = 1; fi; \
	for file in "$$root" "$$core"/*.rs "$$tests" "$$test_core"/*.rs "$(BOOT_RENDER_INPUT_TOP_DIR)/misc/make/active-reblit-boot-render-input-tests.mk"; do \
		timeout 10s test "$$( timeout 10s wc -l < "$$file" )" -le 1000; \
	done; \
	timeout 1200s $(CARGO) test --manifest-path "$(BOOT_RENDER_INPUT_TOP_DIR)/Cargo.toml" -p forge --lib "$$prefix" -- --test-threads=1
