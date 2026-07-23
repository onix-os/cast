MOUNT_DEVTMPFS_POLICY_TOP_DIR := $(abspath $(dir $(lastword $(MAKEFILE_LIST)))/../..)
CARGO ?= cargo

.PHONY: forge-linux-mount-devtmpfs-policy-test

forge-linux-mount-devtmpfs-policy-test: host-storage-safety-test
	@set -euo pipefail; \
	timeout 10s mkdir -p "$(MOUNT_DEVTMPFS_POLICY_TOP_DIR)/target"; \
	listed="$$( timeout 10s mktemp "$(MOUNT_DEVTMPFS_POLICY_TOP_DIR)/target/linux-mount-devtmpfs-policy-test-list.XXXXXXXXXXXX" )"; \
	trap 'timeout 10s rm -f "$$listed"' EXIT; \
	timeout 300s $(CARGO) test --manifest-path "$(MOUNT_DEVTMPFS_POLICY_TOP_DIR)/Cargo.toml" -p forge --lib -- --list | timeout 300s tee "$$listed" >/dev/null; \
	timeout 10s grep -q . "$$listed"; \
	prefix='linux_fs::tests::devtmpfs_mount_policy::'; \
	timeout 10s test "$$( timeout 10s grep -Ec "^$$prefix.*: test$$" "$$listed" )" = 16; \
	for name in \
		exact_devtmpfs_policy_retains_only_closed_scalar_identity_and_mode \
		exact_read_only_devtmpfs_is_admitted_for_a_reader \
		filesystem_type_is_exactly_devtmpfs_and_never_tmpfs \
		policy_accepts_no_arbitrary_mount_point_argument_or_attachment \
		subroot_and_explicit_bind_semantics_are_rejected \
		access_mode_tokens_are_unique_unopposed_and_cross_domain_consistent \
		bind_and_rbind_tokens_are_rejected_in_both_option_domains \
		nodev_and_duplicate_or_opposed_dev_tokens_are_rejected \
		unrelated_options_source_and_optional_fields_do_not_change_policy \
		selected_mount_id_and_device_identity_are_preserved_as_scalars \
		option_limit_admits_exact_n_and_rejects_n_minus_one \
		work_limit_admits_exact_n_and_rejects_n_minus_one \
		zero_and_overproduction_limits_fail_before_policy_scan \
		deadline_equality_is_admitted_and_one_nanosecond_late_is_rejected \
		deadline_expiring_only_at_terminal_checkpoint_rejects_policy \
		malformed_mountinfo_and_ambiguous_dev_attachments_fail_before_policy; do \
		timeout 10s grep -Fqx "$$prefix$$name: test" "$$listed"; \
	done; \
	selector="$(MOUNT_DEVTMPFS_POLICY_TOP_DIR)/crates/forge/src/linux_fs/mountinfo_attachment.rs"; \
	policy="$(MOUNT_DEVTMPFS_POLICY_TOP_DIR)/crates/forge/src/linux_fs/mountinfo_devtmpfs_policy.rs"; \
	tests="$(MOUNT_DEVTMPFS_POLICY_TOP_DIR)/crates/forge/src/linux_fs/tests/mountinfo_devtmpfs_policy.rs"; \
	timeout 10s grep -Fq 'pub(crate) enum DevtmpfsFilesystemKind' "$$policy"; \
	timeout 10s grep -Fq 'pub(crate) enum DevtmpfsAccessMode' "$$policy"; \
	timeout 10s grep -Fq 'pub(crate) struct ValidatedDevtmpfsMountInfoPolicy' "$$policy"; \
	timeout 10s grep -Fq 'pub(crate) fn validate_selected_devtmpfs_mount_policy_until' "$$policy"; \
	timeout 10s grep -Fq 'selected.mount_point() != b"/dev"' "$$policy"; \
	timeout 10s grep -Fq 'selected.root() != b"/"' "$$policy"; \
	timeout 10s grep -Fq 'filesystem_type != b"devtmpfs"' "$$policy"; \
	timeout 10s grep -Fq 'limits.max_options > MAX_POLICY_OPTIONS' "$$policy"; \
	timeout 10s grep -Fq 'limits.max_work > MAX_POLICY_WORK' "$$policy"; \
	for token in 'b"rw"' 'b"ro"' 'b"bind"' 'b"rbind"' 'b"dev"' 'b"nodev"'; do \
		timeout 10s grep -Fq "$$token" "$$policy"; \
	done; \
	timeout 10s grep -Fq 'pub(super) fn policy_filesystem_type' "$$selector"; \
	timeout 10s grep -Fq 'pub(super) fn policy_mount_options' "$$selector"; \
	timeout 10s grep -Fq 'pub(super) fn policy_super_options' "$$selector"; \
	witness_decl="$$( timeout 10s sed -n '/pub(crate) struct ValidatedDevtmpfsMountInfoPolicy {/,/^}/p' "$$policy" )"; \
	timeout 10s test -n "$$witness_decl"; \
	if timeout 10s rg -n '\b(Vec|String|File|Path|Fd)\b|\[u8\]|descriptor|selected_entry' <<<"$$witness_decl"; then exit 1; else status="$$?"; timeout 10s test "$$status" = 1; fi; \
	if timeout 10s rg -n 'mount_source|optional_fields' "$$policy"; then exit 1; else status="$$?"; timeout 10s test "$$status" = 1; fi; \
	if timeout 10s rg -n 'std::fs|File::open|OpenOptions|read_to_end|read_dir|canonicalize|std::env|std::process|process::Command|Command::new|nix::mount|libc::mount|setns|unshare|chroot|pivot_root|mount\(|umount|BLK[A-Z_]+|/proc/|/sys/' "$$policy" "$$tests"; then exit 1; else status="$$?"; timeout 10s test "$$status" = 1; fi; \
	for file in "$$policy" "$$tests" "$(MOUNT_DEVTMPFS_POLICY_TOP_DIR)/misc/make/linux-mount-devtmpfs-policy-tests.mk"; do \
		timeout 10s test "$$( timeout 10s wc -l < "$$file" )" -le 1000; \
	done; \
	timeout 1200s $(CARGO) test --manifest-path "$(MOUNT_DEVTMPFS_POLICY_TOP_DIR)/Cargo.toml" -p forge --lib "$$prefix" -- --test-threads=1
