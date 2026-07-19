SHELL := /bin/bash

DESCRIPTOR_BOOT_FILESYSTEM_TOP_DIR := $(abspath $(dir $(lastword $(MAKEFILE_LIST)))/../..)
CARGO ?= cargo

.PHONY: forge-linux-descriptor-boot-filesystem-test

forge-linux-descriptor-boot-filesystem-test: host-storage-safety-test
	@set -euo pipefail; \
	timeout 10s mkdir -p "$(DESCRIPTOR_BOOT_FILESYSTEM_TOP_DIR)/target"; \
	listed="$$( timeout 10s mktemp "$(DESCRIPTOR_BOOT_FILESYSTEM_TOP_DIR)/target/linux-descriptor-boot-filesystem-test-list.XXXXXXXXXXXX" )"; \
	trap 'timeout 10s rm -f "$$listed"' EXIT; \
	timeout 300s $(CARGO) test --manifest-path "$(DESCRIPTOR_BOOT_FILESYSTEM_TOP_DIR)/Cargo.toml" -p forge --lib -- --list | timeout 300s tee "$$listed" >/dev/null; \
	timeout 10s grep -q . "$$listed"; \
	prefix='linux_fs::tests::descriptor_boot_filesystem::'; \
	timeout 10s test "$$( timeout 10s grep -Ec "^$$prefix.*: test$$" "$$listed" )" = 12; \
	for name in \
		stable_msdos_family_directory_retains_only_expected_scalar_evidence \
		stable_wrong_magic_is_rejected_without_claiming_exact_vfat \
		filesystem_magic_drift_is_rejected_before_family_admission \
		expected_identity_is_exact_nonzero_and_checked_before_observations \
		observed_identity_must_be_nonzero_and_stable \
		directory_kind_must_be_stable_and_exact \
		observation_limit_admits_four_and_rejects_three_before_the_fourth_hook \
		work_limit_admits_exact_consumption_and_rejects_one_less \
		deadline_equality_is_admitted_and_later_time_fails_before_observation \
		deadline_expiring_at_terminal_checkpoint_rejects_evidence \
		zero_limits_fail_before_fixture_observations \
		injected_fixture_observation_failure_propagates_without_fallback; do \
		timeout 10s grep -Fqx "$$prefix$$name: test" "$$listed"; \
	done; \
	module="$(DESCRIPTOR_BOOT_FILESYSTEM_TOP_DIR)/crates/forge/src/linux_fs/descriptor_boot_filesystem.rs"; \
	tests="$(DESCRIPTOR_BOOT_FILESYSTEM_TOP_DIR)/crates/forge/src/linux_fs/tests/descriptor_boot_filesystem.rs"; \
	timeout 10s grep -Fq 'const MSDOS_SUPER_MAGIC: nix::libc::c_long = 0x4d44;' "$$module"; \
	timeout 10s grep -Fq 'pub(crate) fn authenticate_boot_filesystem_directory_until(' "$$module"; \
	timeout 10s grep -Fq 'directory: &std::fs::File,' "$$module"; \
	timeout 10s grep -Fq 'expected_device: u64,' "$$module"; \
	timeout 10s grep -Fq 'expected_inode: u64,' "$$module"; \
	timeout 10s grep -Fq 'deadline: Instant,' "$$module"; \
	timeout 10s grep -Fq 'pub(crate) struct ValidatedBootFilesystemDescriptorEvidence' "$$module"; \
	timeout 10s grep -Fq 'BootFilesystemMagicFamily::LinuxMsdos' "$$module"; \
	timeout 10s grep -Fq 'BootFilesystemObservationPhase::OpeningDirectoryIdentity' "$$module"; \
	timeout 10s grep -Fq 'BootFilesystemObservationPhase::OpeningFilesystemMagic' "$$module"; \
	timeout 10s grep -Fq 'BootFilesystemObservationPhase::ClosingFilesystemMagic' "$$module"; \
	timeout 10s grep -Fq 'BootFilesystemObservationPhase::ClosingDirectoryIdentity' "$$module"; \
	timeout 10s test "$$( timeout 10s grep -Fc 'nix::libc::fstat(directory.as_raw_fd(), &mut status)' "$$module" )" = 1; \
	timeout 10s test "$$( timeout 10s grep -Fc 'nix::libc::fstatfs(directory.as_raw_fd(), &mut status)' "$$module" )" = 1; \
	witness_decl="$$( timeout 10s sed -n '/pub(crate) struct ValidatedBootFilesystemDescriptorEvidence {/,/^}/p' "$$module" )"; \
	timeout 10s test -n "$$witness_decl"; \
	if timeout 10s rg -n '\b(File|Path|OwnedFd|RawFd|BorrowedFd|AsRawFd|Reader|Writer)\b|fd:|descriptor:|closure' <<<"$$witness_decl"; then exit 1; else status="$$?"; timeout 10s test "$$status" = 1; fi; \
	if timeout 10s rg -n 'BootFilesystemMagicFamily::Vfat|BootFilesystemMagicFamily::VFAT' "$$module" "$$tests"; then exit 1; else status="$$?"; timeout 10s test "$$status" = 1; fi; \
	if timeout 10s rg -n 'File::open|OpenOptions|create_dir|read_dir|canonicalize|std::env|std::process|process::Command|Command::new|write_all|set_len|remove_(file|dir)|rename\(|nix::mount|libc::mount|setns|unshare|chroot|pivot_root|umount|BLK[A-Z_]+|/dev/|/(boot|efi|esp)(/|`)' "$$module" "$$tests"; then exit 1; else status="$$?"; timeout 10s test "$$status" = 1; fi; \
	if timeout 10s rg -n 'std::fs|tempfile|\b(Path|File|OpenOptions)\b' "$$tests"; then exit 1; else status="$$?"; timeout 10s test "$$status" = 1; fi; \
	for file in "$$module" "$$tests" "$(DESCRIPTOR_BOOT_FILESYSTEM_TOP_DIR)/misc/make/linux-descriptor-boot-filesystem-tests.mk"; do \
		timeout 10s test "$$( timeout 10s wc -l < "$$file" )" -le 1000; \
	done; \
	timeout 1200s $(CARGO) test --manifest-path "$(DESCRIPTOR_BOOT_FILESYSTEM_TOP_DIR)/Cargo.toml" -p forge --lib "$$prefix" -- --test-threads=1
