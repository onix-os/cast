DESCRIPTOR_DEVTMPFS_FILESYSTEM_TOP_DIR := $(abspath $(dir $(lastword $(MAKEFILE_LIST)))/../..)
CARGO ?= cargo

.PHONY: forge-linux-descriptor-devtmpfs-filesystem-test

forge-linux-descriptor-devtmpfs-filesystem-test: host-storage-safety-test
	@set -euo pipefail; \
	timeout 10s mkdir -p "$(DESCRIPTOR_DEVTMPFS_FILESYSTEM_TOP_DIR)/target"; \
	listed="$$( timeout 10s mktemp "$(DESCRIPTOR_DEVTMPFS_FILESYSTEM_TOP_DIR)/target/linux-descriptor-devtmpfs-filesystem-test-list.XXXXXXXXXXXX" )"; \
	trap 'timeout 10s rm -f "$$listed"' EXIT; \
	timeout 300s $(CARGO) test --manifest-path "$(DESCRIPTOR_DEVTMPFS_FILESYSTEM_TOP_DIR)/Cargo.toml" -p forge --lib -- --list | timeout 300s tee "$$listed" >/dev/null; \
	timeout 10s grep -q . "$$listed"; \
	prefix='linux_fs::tests::descriptor_devtmpfs_filesystem::'; \
	timeout 10s test "$$( timeout 10s grep -Ec "^$$prefix.*: test$$" "$$listed" )" = 16; \
	for name in \
		stable_policy_and_six_phase_schedule_retain_closed_same_mount_evidence \
		tmpfs_magic_is_shared_and_wrong_magic_is_rejected \
		policy_identity_is_checked_canonically_before_observations \
		expected_identity_must_be_nonzero_before_observations \
		observed_identity_must_be_nonzero_stable_and_exact \
		directory_kind_must_be_stable_and_exact \
		descriptor_mount_id_must_be_nonzero_stable_and_exact \
		filesystem_magic_must_be_stable_before_family_admission \
		wrong_observation_variant_is_a_protocol_violation \
		observation_limit_admits_six_and_rejects_five_before_sixth_hook \
		work_limit_admits_exact_consumption_and_rejects_one_less \
		deadline_equality_is_admitted_and_expired_time_fails_before_observation \
		deadline_expiry_between_observations_rejects_evidence \
		terminal_deadline_checkpoint_rejects_evidence \
		zero_or_above_ceiling_limits_fail_before_observations \
		injected_observation_failure_propagates_without_fallback; do \
		timeout 10s grep -Fqx "$$prefix$$name: test" "$$listed"; \
	done; \
	module="$(DESCRIPTOR_DEVTMPFS_FILESYSTEM_TOP_DIR)/crates/forge/src/linux_fs/descriptor_devtmpfs_filesystem.rs"; \
	tests="$(DESCRIPTOR_DEVTMPFS_FILESYSTEM_TOP_DIR)/crates/forge/src/linux_fs/tests/descriptor_devtmpfs_filesystem.rs"; \
	timeout 10s grep -Fq 'const TMPFS_MAGIC: nix::libc::c_long = 0x0102_1994;' "$$module"; \
	timeout 10s grep -Fq 'pub(crate) fn authenticate_devtmpfs_same_mount_directory_until(' "$$module"; \
	timeout 10s grep -Fq 'directory: &std::fs::File,' "$$module"; \
	timeout 10s grep -Fq 'expected_device: u64,' "$$module"; \
	timeout 10s grep -Fq 'expected_inode: u64,' "$$module"; \
	timeout 10s grep -Fq 'expected_mount_id: u64,' "$$module"; \
	timeout 10s grep -Fq 'policy: ValidatedDevtmpfsMountInfoPolicy,' "$$module"; \
	timeout 10s grep -Fq 'deadline: Instant,' "$$module"; \
	timeout 10s grep -Fq 'pub(crate) struct ValidatedDevtmpfsSameMountDescriptorEvidence' "$$module"; \
	timeout 10s grep -Fq 'DevtmpfsDescriptorMagicFamily::LinuxTmpfs' "$$module"; \
	for phase in OpeningDirectoryIdentity OpeningDescriptorMountId OpeningFilesystemMagic ClosingFilesystemMagic ClosingDescriptorMountId ClosingDirectoryIdentity; do \
		timeout 10s grep -Fq "DevtmpfsDescriptorObservationPhase::$$phase" "$$module"; \
	done; \
	timeout 10s test "$$( timeout 10s grep -Fc 'nix::libc::fstat(directory.as_raw_fd(), &mut status)' "$$module" )" = 1; \
	timeout 10s test "$$( timeout 10s grep -Fc 'nix::libc::fstatfs(directory.as_raw_fd(), &mut status)' "$$module" )" = 1; \
	timeout 10s grep -Fq 'descriptor_mount_id_until(directory, deadline)' "$$module"; \
	timeout 10s grep -Fq 'caller-authored path' "$$module"; \
	timeout 10s grep -Fq 'authenticated current-thread procfs `fd`/`fdinfo` resolution' "$$module"; \
	timeout 10s grep -Fq 'does not prove that the descriptor names the exact `/dev` root' "$$module"; \
	timeout 10s grep -Fq 'whole-root bind provenance remains unprovable here' "$$module"; \
	witness_decl="$$( timeout 10s sed -n '/pub(crate) struct ValidatedDevtmpfsSameMountDescriptorEvidence {/,/^}/p' "$$module" )"; \
	timeout 10s test -n "$$witness_decl"; \
	if timeout 10s rg -n '\b(File|Path|OwnedFd|RawFd|BorrowedFd|AsRawFd|Reader|Writer)\b|fd:|descriptor:|closure' <<<"$$witness_decl"; then exit 1; else status="$$?"; timeout 10s test "$$status" = 1; fi; \
	if timeout 10s rg -n 'No path is accepted or opened|opens no path|TMPFS_MAGIC.*proves devtmpfs|exact `/dev` root[^\n]*(is|was) proven' "$$module" "$$tests"; then exit 1; else status="$$?"; timeout 10s test "$$status" = 1; fi; \
	if timeout 10s rg -n 'File::open|OpenOptions|create_dir|read_dir|canonicalize|std::env|std::process|process::Command|Command::new|write_all|set_len|remove_(file|dir)|rename\(|nix::mount|libc::mount|setns|unshare|chroot|pivot_root|umount|BLK[A-Z_]+|/dev/' "$$module" "$$tests"; then exit 1; else status="$$?"; timeout 10s test "$$status" = 1; fi; \
	if timeout 10s rg -n 'std::fs|tempfile|\b(Path|File|OpenOptions)\b' "$$tests"; then exit 1; else status="$$?"; timeout 10s test "$$status" = 1; fi; \
	for file in "$$module" "$$tests" "$(DESCRIPTOR_DEVTMPFS_FILESYSTEM_TOP_DIR)/misc/make/linux-descriptor-devtmpfs-filesystem-tests.mk"; do \
		timeout 10s test "$$( timeout 10s wc -l < "$$file" )" -le 1000; \
	done; \
	timeout 1200s $(CARGO) test --manifest-path "$(DESCRIPTOR_DEVTMPFS_FILESYSTEM_TOP_DIR)/Cargo.toml" -p forge --lib "$$prefix" -- --test-threads=1
