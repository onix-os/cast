SHELL := /bin/bash

MOUNT_NAMESPACE_TOP_DIR := $(abspath $(dir $(lastword $(MAKEFILE_LIST)))/../..)
CARGO ?= cargo

.PHONY: forge-linux-mount-namespace-test

forge-linux-mount-namespace-test: host-storage-safety-test
	@set -euo pipefail; \
	timeout 10s mkdir -p "$(MOUNT_NAMESPACE_TOP_DIR)/target"; \
	listed="$$( timeout 10s mktemp "$(MOUNT_NAMESPACE_TOP_DIR)/target/linux-mount-namespace-test-list.XXXXXXXXXXXX" )"; \
	trap 'timeout 10s rm -f "$$listed"' EXIT; \
	timeout 300s $(CARGO) test --manifest-path "$(MOUNT_NAMESPACE_TOP_DIR)/Cargo.toml" -p forge --lib -- --list | timeout 30s tee "$$listed" >/dev/null; \
	timeout 10s grep -q . "$$listed"; \
	prefix='linux_fs::tests::mount_namespace::'; \
	timeout 10s test "$$( timeout 10s grep -Ec "^$$prefix.*: test$$" "$$listed" )" = 17; \
	for name in \
		bounds::zero_limits_and_expired_deadline_fail_before_fixture_hooks \
		bounds::checkpoint_hooks_are_finite_and_injected_failure_is_propagated \
		bounds::preparation_work_and_descriptor_budgets_have_exact_boundaries \
		bounds::revalidation_budgets_are_global_across_both_passes_and_terminal_checks \
		malformed::fixture_admission_requires_one_safe_named_ordinary_tree \
		malformed::namespace_authentication_rejects_wrong_filesystem_magic_and_namespace_type \
		malformed::every_required_fixture_entry_must_exist \
		malformed::symlink_entries_are_never_followed \
		malformed::fifo_and_wrong_entry_kinds_fail_without_opening_special_files \
		races::first_pass_tree_namespace_marker_and_root_replacements_fail_closed \
		races::second_pass_descriptor_replacements_fail_closed \
		races::terminal_namespace_and_root_replacements_fail_closed \
		races::prepared_anchor_rejects_across_call_tree_namespace_and_root_replacements \
		races::revalidation_repeats_both_complete_passes_and_terminal_rebinds \
		stable::stable_fixture_prepares_and_revalidates_exact_capability_identities \
		stable::namespace_marker_contents_are_not_semantic_authority \
		stable::distinct_fixtures_retain_distinct_marker_and_root_identities; do \
		timeout 10s grep -Fqx "$$prefix$$name: test" "$$listed"; \
	done; \
	root="$(MOUNT_NAMESPACE_TOP_DIR)/crates/forge/src/linux_fs/mount_namespace.rs"; \
	core="$(MOUNT_NAMESPACE_TOP_DIR)/crates/forge/src/linux_fs/mount_namespace"; \
	runtime="$(MOUNT_NAMESPACE_TOP_DIR)/crates/forge/src/transition_journal/runtime_evidence.rs"; \
	timeout 10s grep -Fq 'pub(crate) fn prepare() -> io::Result<Self>' "$$root"; \
	timeout 10s grep -Fq 'pub(crate) fn revalidate(&self) -> io::Result<RevalidatedMountNamespaceAnchor<' "$$root"; \
	timeout 10s grep -Fq 'NS_GET_NSTYPE' "$$root"; \
	timeout 10s grep -Fq 'CLONE_NEWNS' "$$root"; \
	timeout 10s grep -Fq 'NSFS_MAGIC' "$$root"; \
	timeout 10s grep -Fq 'validate_fixture_namespace_authentication' "$$root"; \
	timeout 10s test "$$( timeout 10s grep -Fc 'PhantomData<Rc<()>>' "$$root" )" -ge 2; \
	if timeout 10s rg -n 'mount_namespace_mount_id' "$$root" "$(MOUNT_NAMESPACE_TOP_DIR)/crates/forge/src/linux_fs/tests/mount_namespace"; then exit 1; else status="$$?"; timeout 10s test "$$status" = 1; fi; \
	if timeout 10s rg -n 'unsafe[[:space:]]+impl[[:space:]]+(Send|Sync)' "$$root"; then exit 1; else status="$$?"; timeout 10s test "$$status" = 1; fi; \
	timeout 10s grep -Fq 'validate_namespace_authentication(filesystem_magic, namespace_type)' "$$root"; \
	timeout 10s grep -Fq 'validate_namespace_authentication(status.f_type, namespace_type)' "$$core/filesystem.rs"; \
	timeout 10s grep -Fq 'nix::libc::ioctl(namespace.as_raw_fd(), NS_GET_NSTYPE)' "$$core/filesystem.rs"; \
	timeout 10s test "$$( timeout 10s grep -Fc 'reject_kernel_pseudo_fixture' "$$core/filesystem.rs" )" -ge 8; \
	timeout 10s grep -Fq 'matches!(status.f_type, PROC_SUPER_MAGIC | SYSFS_MAGIC | NSFS_MAGIC)' "$$core/filesystem.rs"; \
	timeout 10s grep -Fq 'authenticate_mount_namespace_descriptor' "$$runtime"; \
	if timeout 10s rg -n 'fn require_nsfs|NS_GET_NSTYPE|NSFS_MAGIC' "$$runtime"; then exit 1; else status="$$?"; timeout 10s test "$$status" = 1; fi; \
	if timeout 10s rg -n 'read_dir|canonicalize|std::env|std::process|process::Command|Command::new|OpenOptions|create_dir|(?:std::fs::|fs::)?write\(|write_all|set_len|remove_(file|dir)|rename\(|nix::mount|nix::sched::setns|nix::sched::unshare|nix::unistd::chroot|libc::(?:setns|unshare|chroot|pivot_root|mount|umount2|open_tree|move_mount)' "$$root" "$$core"; then exit 1; else status="$$?"; timeout 10s test "$$status" = 1; fi; \
	tests="$(MOUNT_NAMESPACE_TOP_DIR)/crates/forge/src/linux_fs/tests/mount_namespace.rs"; \
	if timeout 10s rg -n 'PreparedMountNamespaceAnchor::prepare|/proc|(?:std::fs::|fs::)?File::open\("/(?:sys|dev)(?:/|"\))|authenticated_current_thread_procfs|descriptor_mount_id|RuntimeEpoch::capture|setns|unshare|chroot|pivot_root|open_tree|move_mount|nix::mount|libc::mount|libc::umount2?|read_dir|canonicalize|std::process|process::Command|Command::new|/dev/disk|blkid|lsblk|findmnt|udevadm|smartctl|hdparm' "$$tests" "$(MOUNT_NAMESPACE_TOP_DIR)/crates/forge/src/linux_fs/tests/mount_namespace"; then exit 1; else status="$$?"; timeout 10s test "$$status" = 1; fi; \
	for file in \
		"$$root" \
		"$$core"/*.rs \
		"$$tests" \
		"$(MOUNT_NAMESPACE_TOP_DIR)"/crates/forge/src/linux_fs/tests/mount_namespace/*.rs \
		"$(MOUNT_NAMESPACE_TOP_DIR)/misc/make/linux-mount-namespace-tests.mk"; do \
		timeout 10s test "$$( timeout 10s wc -l < "$$file" )" -le 1000; \
	done; \
	timeout 1200s $(CARGO) test --manifest-path "$(MOUNT_NAMESPACE_TOP_DIR)/Cargo.toml" -p forge --lib "$$prefix" -- --test-threads=1
