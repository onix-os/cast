SHELL := /bin/bash

MOUNT_BOOT_POLICY_TOP_DIR := $(abspath $(dir $(lastword $(MAKEFILE_LIST)))/../..)
CARGO ?= cargo

.PHONY: forge-linux-mount-boot-policy-test

forge-linux-mount-boot-policy-test: host-storage-safety-test
	@set -euo pipefail; \
	timeout 10s mkdir -p "$(MOUNT_BOOT_POLICY_TOP_DIR)/target"; \
	listed="$$( timeout 10s mktemp "$(MOUNT_BOOT_POLICY_TOP_DIR)/target/linux-mount-boot-policy-test-list.XXXXXXXXXXXX" )"; \
	trap 'timeout 10s rm -f "$$listed"' EXIT; \
	timeout 300s $(CARGO) test --manifest-path "$(MOUNT_BOOT_POLICY_TOP_DIR)/Cargo.toml" -p forge --lib -- --list | timeout 300s tee "$$listed" >/dev/null; \
	timeout 10s grep -q . "$$listed"; \
	prefix='linux_fs::tests::boot_mount_policy::'; \
	timeout 10s test "$$( timeout 10s grep -Ec "^$$prefix.*: test$$" "$$listed" )" = 11; \
	for name in \
		exact_vfat_policy_retains_every_required_fact \
		filesystem_type_is_exact_and_closed \
		mount_and_superblock_each_require_one_unopposed_rw \
		each_security_flag_is_required_once_without_inverse \
		superblock_security_options_cannot_satisfy_per_mount_policy \
		unrelated_options_source_and_optional_fields_do_not_change_policy \
		option_limit_admits_n_and_rejects_n_plus_one \
		work_limit_admits_exact_consumption_and_rejects_one_less \
		deadline_equality_is_admitted_and_later_time_is_rejected \
		deadline_expiring_only_at_terminal_checkpoint_rejects_policy \
		zero_option_or_work_limits_fail_before_policy_scan; do \
		timeout 10s grep -Fqx "$$prefix$$name: test" "$$listed"; \
	done; \
	selector="$(MOUNT_BOOT_POLICY_TOP_DIR)/crates/forge/src/linux_fs/mountinfo_attachment.rs"; \
	policy="$(MOUNT_BOOT_POLICY_TOP_DIR)/crates/forge/src/linux_fs/mountinfo_boot_policy.rs"; \
	tests="$(MOUNT_BOOT_POLICY_TOP_DIR)/crates/forge/src/linux_fs/tests/mountinfo_boot_policy.rs"; \
	timeout 10s grep -Fq 'pub(crate) enum BootFilesystemKind' "$$policy"; \
	timeout 10s grep -Fq 'pub(crate) struct ValidatedBootMountInfoPolicy' "$$policy"; \
	timeout 10s grep -Fq 'pub(crate) fn validate_selected_boot_mount_policy_until' "$$policy"; \
	timeout 10s grep -Fq 'MountOptionDomain::Mount' "$$policy"; \
	timeout 10s grep -Fq 'MountOptionDomain::Superblock' "$$policy"; \
	for token in 'b"vfat"' 'b"rw"' 'b"ro"' 'b"nosuid"' 'b"nodev"' 'b"noexec"' 'b"nosymfollow"'; do \
		timeout 10s grep -Fq "$$token" "$$policy"; \
	done; \
	timeout 10s grep -Fq 'pub(super) fn policy_filesystem_type' "$$selector"; \
	timeout 10s grep -Fq 'pub(super) fn policy_mount_options' "$$selector"; \
	timeout 10s grep -Fq 'pub(super) fn policy_super_options' "$$selector"; \
	if timeout 10s rg -n 'mount_source|optional_fields' "$$policy"; then exit 1; else status="$$?"; timeout 10s test "$$status" = 1; fi; \
	if timeout 10s rg -n 'pub\(crate\).*policy_(filesystem_type|mount_options|super_options)|pub\(crate\).*selected_entry' "$$selector"; then exit 1; else status="$$?"; timeout 10s test "$$status" = 1; fi; \
	if timeout 10s rg -n 'std::fs|File::open|OpenOptions|read_to_end|read_dir|canonicalize|std::env|std::process|process::Command|Command::new|nix::mount|libc::mount|setns|unshare|chroot|pivot_root|mount\(|umount|BLK[A-Z_]+' "$$policy" "$$tests"; then exit 1; else status="$$?"; timeout 10s test "$$status" = 1; fi; \
	for file in "$$selector" "$$policy" "$$tests" "$(MOUNT_BOOT_POLICY_TOP_DIR)/misc/make/linux-mount-boot-policy-tests.mk"; do \
		timeout 10s test "$$( timeout 10s wc -l < "$$file" )" -le 1000; \
	done; \
	timeout 1200s $(CARGO) test --manifest-path "$(MOUNT_BOOT_POLICY_TOP_DIR)/Cargo.toml" -p forge --lib "$$prefix" -- --test-threads=1
