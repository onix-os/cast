SHELL := /bin/bash

MOUNT_ATTACHMENT_TOP_DIR := $(abspath $(dir $(lastword $(MAKEFILE_LIST)))/../..)
CARGO ?= cargo

.PHONY: forge-linux-mount-attachment-test

forge-linux-mount-attachment-test: host-storage-safety-test
	@set -euo pipefail; \
	timeout 10s mkdir -p "$(MOUNT_ATTACHMENT_TOP_DIR)/target"; \
	listed="$$( timeout 10s mktemp "$(MOUNT_ATTACHMENT_TOP_DIR)/target/linux-mount-attachment-test-list.XXXXXXXXXXXX" )"; \
	trap 'timeout 10s rm -f "$$listed"' EXIT; \
	timeout 300s $(CARGO) test --manifest-path "$(MOUNT_ATTACHMENT_TOP_DIR)/Cargo.toml" -p forge --lib -- --list | timeout 300s tee "$$listed" >/dev/null; \
	timeout 10s grep -q . "$$listed"; \
	prefix='linux_fs::tests::mount_attachment::'; \
	timeout 10s test "$$( timeout 10s grep -Ec "^$$prefix.*: test$$" "$$listed" )" = 17; \
	for name in \
		stable_snapshot_selects_only_exact_attachment_semantics \
		filesystem_type_source_and_options_are_descriptive_only \
		unrelated_mount_table_churn_does_not_change_the_selected_view \
		escaped_mount_point_is_compared_as_exact_decoded_bytes \
		invalid_utf8_mount_point_never_matches_or_gets_reinterpreted \
		missing_selector_is_rejected_without_fallback \
		full_scan_rejects_a_later_stacked_selector_match \
		selected_mount_id_must_equal_the_expected_unique_id \
		selected_root_must_be_exact_partition_root \
		selected_device_major_and_minor_are_both_exact \
		malformed_selectors_are_defense_rejected_before_lookup \
		selector_byte_component_and_component_count_bounds_are_exact \
		entry_ceiling_admits_n_and_rejects_n_plus_one \
		work_ceiling_admits_exact_consumption_and_rejects_one_less \
		deadline_equality_is_admitted_and_one_nanosecond_late_is_rejected \
		deadline_expiring_only_at_terminal_checkpoint_rejects_the_result \
		zero_entry_or_work_limits_fail_before_any_scan_clock; do \
		timeout 10s grep -Fqx "$$prefix$$name: test" "$$listed"; \
	done; \
	module="$(MOUNT_ATTACHMENT_TOP_DIR)/crates/forge/src/linux_fs/mountinfo_attachment.rs"; \
	tests="$(MOUNT_ATTACHMENT_TOP_DIR)/crates/forge/src/linux_fs/tests/mount_attachment.rs"; \
	timeout 10s grep -Fq 'pub(crate) fn select_mountinfo_attachment_until' "$$module"; \
	timeout 10s grep -Fq 'pub(crate) struct SelectedMountInfoAttachment' "$$module"; \
	timeout 10s grep -Fq 'MOUNTINFO_LIMITS' "$$module"; \
	timeout 10s grep -Fq 'expected_id_occurrences' "$$module"; \
	if timeout 10s rg -n 'filesystem_type\(|mount_source\(|mount_options\(|optional_fields\(|super_options\(' "$$module"; then exit 1; else status="$$?"; timeout 10s test "$$status" = 1; fi; \
	if timeout 10s rg -n '/proc|/sys|/dev|File::open|OpenOptions|read_to_end|read_dir|canonicalize|std::env|std::process|process::Command|Command::new|nix::mount|setns|unshare|chroot|pivot_root|mount\(|umount' "$$module" "$$tests"; then exit 1; else status="$$?"; timeout 10s test "$$status" = 1; fi; \
	for file in "$$module" "$$tests" "$(MOUNT_ATTACHMENT_TOP_DIR)/misc/make/linux-mount-attachment-tests.mk"; do \
		timeout 10s test "$$( timeout 10s wc -l < "$$file" )" -le 1000; \
	done; \
	timeout 1200s $(CARGO) test --manifest-path "$(MOUNT_ATTACHMENT_TOP_DIR)/Cargo.toml" -p forge --lib "$$prefix" -- --test-threads=1
