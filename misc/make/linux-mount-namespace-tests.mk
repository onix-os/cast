SHELL := /bin/bash

MOUNT_NAMESPACE_TOP_DIR := $(abspath $(dir $(lastword $(MAKEFILE_LIST)))/../..)
CARGO ?= cargo

.PHONY: forge-linux-mount-namespace-test

forge-linux-mount-namespace-test: host-storage-safety-test
	@set -euo pipefail; \
	timeout 10s mkdir -p "$(MOUNT_NAMESPACE_TOP_DIR)/target"; \
	listed="$$( timeout 10s mktemp "$(MOUNT_NAMESPACE_TOP_DIR)/target/linux-mount-namespace-test-list.XXXXXXXXXXXX" )"; \
	trap 'timeout 10s rm -f "$$listed"' EXIT; \
	timeout 300s $(CARGO) test --manifest-path "$(MOUNT_NAMESPACE_TOP_DIR)/Cargo.toml" -p forge --lib -- --list | timeout 300s tee "$$listed" >/dev/null; \
	timeout 10s grep -q . "$$listed"; \
	prefix='linux_fs::tests::mount_namespace::'; \
	timeout 10s test "$$( timeout 10s grep -Ec "^$$prefix.*: test$$" "$$listed" )" = 40; \
	for name in \
		attachment::bounds::zero_limits_and_expired_deadline_fail_before_attachment_hooks \
		attachment::bounds::attachment_hooks_are_finite_and_injected_failures_propagate \
		attachment::bounds::preparation_work_and_descriptor_budgets_have_exact_adjacent_boundaries \
		attachment::bounds::revalidation_budgets_cover_both_chain_passes_and_both_anchor_edges \
		attachment::deadline::fixture_clock_rejects_attachment_prepare_and_revalidation_at_entry \
		attachment::deadline::fixture_clock_rejects_attachment_prepare_and_revalidation_at_final_checkpoint \
		attachment::malformed::malformed_absolute_lexical_selectors_fail_before_resolution \
		attachment::malformed::selector_byte_component_and_depth_ceilings_are_exact \
		attachment::malformed::missing_symlink_fifo_and_non_directory_components_are_rejected \
		attachment::malformed::st_dev_classifier_accepts_canonical_boundaries_and_rejects_unrepresentable_values \
		attachment::races::every_component_replacement_in_either_complete_pass_fails_closed \
		attachment::races::public_task_root_replacement_is_rejected_by_both_anchor_sandwich_edges \
		attachment::races::mount_namespace_replacement_is_rejected_by_both_anchor_sandwich_edges \
		attachment::races::terminal_parent_final_name_and_late_full_chain_replacements_fail_closed \
		attachment::races::prepared_attachment_rejects_across_call_component_and_root_replacements \
		attachment::races::revalidation_repeats_second_pass_and_closing_anchor_checks \
		attachment::stable::stable_nested_selector_retains_exact_raw_chain_and_destination_identity \
		attachment::stable::one_component_selector_uses_task_root_as_its_final_parent \
		attachment::stable::attachment_revalidation_rejects_a_different_mount_context_anchor \
		attachment::stable::independently_prepared_anchor_with_the_same_authenticated_snapshot_is_accepted \
		attachment::stable::destination_st_dev_converts_to_exact_sysfs_major_and_minor \
		bounds::zero_limits_and_expired_deadline_fail_before_fixture_hooks \
		bounds::checkpoint_hooks_are_finite_and_injected_failure_is_propagated \
		bounds::preparation_work_and_descriptor_budgets_have_exact_boundaries \
		bounds::revalidation_budgets_are_global_across_both_passes_and_terminal_checks \
		deadline::fixture_clock_rejects_prepare_and_revalidate_at_entry \
		deadline::fixture_clock_rejects_prepare_and_revalidate_at_explicit_final_checkpoint \
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
	timeout 10s grep -Fq 'pub(crate) fn prepare_until(deadline: Instant) -> io::Result<Self>' "$$root"; \
	timeout 10s grep -Fq 'pub(crate) fn revalidate(&self) -> io::Result<RevalidatedMountNamespaceAnchor<' "$$root"; \
	timeout 10s grep -Fq 'pub(crate) fn revalidate_until(&self, deadline: Instant)' "$$root"; \
	timeout 10s grep -Fq 'pub(crate) fn prepare_task_rooted_attachment_until(' "$$core/attachment.rs"; \
	timeout 10s grep -Fq 'pub(crate) fn revalidate_against_until(' "$$core/attachment.rs"; \
	timeout 10s grep -Fq 'pub(crate) fn destination_sysfs_device_number(&self)' "$$core/attachment.rs"; \
	timeout 10s grep -Fq 'nix::libc::major(raw_device)' "$$core/attachment.rs"; \
	timeout 10s grep -Fq 'nix::libc::minor(raw_device)' "$$core/attachment.rs"; \
	timeout 10s grep -Fq 'nix::libc::makedev(major, minor)' "$$core/attachment.rs"; \
	timeout 10s grep -Fq 'rebuilt != raw_device || rebuilt_u128 != device' "$$core/attachment.rs"; \
	timeout 10s grep -Fq 'operation.emit(CaptureCheckpoint::OperationComplete)?' "$$root"; \
	timeout 10s grep -Fq 'operation.emit_attachment(AttachmentCheckpoint::Complete)?' "$$core/attachment.rs"; \
	timeout 10s grep -Fq 'pub(super) fn fixture_with_clock(' "$$core/filesystem.rs"; \
	timeout 10s grep -Fq 'NS_GET_NSTYPE' "$$root"; \
	timeout 10s grep -Fq 'CLONE_NEWNS' "$$root"; \
	timeout 10s grep -Fq 'NSFS_MAGIC' "$$root"; \
	timeout 10s grep -Fq 'validate_fixture_namespace_authentication' "$$root"; \
	timeout 10s test "$$( timeout 10s grep -Fc 'PhantomData<Rc<()>>' "$$root" )" -ge 2; \
	timeout 10s grep -Fq 'PhantomData<Rc<()>>' "$$core/attachment.rs"; \
	if timeout 10s rg -n 'mount_namespace_mount_id' "$$root" "$(MOUNT_NAMESPACE_TOP_DIR)/crates/forge/src/linux_fs/tests/mount_namespace"; then exit 1; else status="$$?"; timeout 10s test "$$status" = 1; fi; \
	if timeout 10s rg -n 'unsafe[[:space:]]+impl[[:space:]]+(Send|Sync)' "$$root"; then exit 1; else status="$$?"; timeout 10s test "$$status" = 1; fi; \
	if timeout 10s rg -n 'unsafe[[:space:]]+impl[[:space:]]+(Send|Sync)' "$$core/attachment.rs" "$$core/attachment"; then exit 1; else status="$$?"; timeout 10s test "$$status" = 1; fi; \
	if timeout 10s rg -n 'pub\(crate\).*(File|OwnedFd|RawFd|AsRawFd|raw_fd|as_raw_fd)' "$$core/attachment.rs" "$$core/attachment"; then exit 1; else status="$$?"; timeout 10s test "$$status" = 1; fi; \
	timeout 10s grep -Fq 'RESOLVE_BENEATH' "$$core/attachment/filesystem.rs"; \
	timeout 10s grep -Fq 'RESOLVE_NO_MAGICLINKS' "$$core/attachment/filesystem.rs"; \
	timeout 10s grep -Fq 'RESOLVE_NO_SYMLINKS' "$$core/attachment/filesystem.rs"; \
	timeout 10s grep -Fq 'ATTACHMENT_RESOLUTION & nix::libc::RESOLVE_NO_XDEV' "$$core/attachment/filesystem.rs"; \
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
		"$$core"/attachment/*.rs \
		"$$tests" \
		"$(MOUNT_NAMESPACE_TOP_DIR)"/crates/forge/src/linux_fs/tests/mount_namespace/*.rs \
		"$(MOUNT_NAMESPACE_TOP_DIR)"/crates/forge/src/linux_fs/tests/mount_namespace/attachment/*.rs \
		"$(MOUNT_NAMESPACE_TOP_DIR)/misc/make/linux-mount-namespace-tests.mk"; do \
		timeout 10s test "$$( timeout 10s wc -l < "$$file" )" -le 1000; \
	done; \
	timeout 1200s $(CARGO) test --manifest-path "$(MOUNT_NAMESPACE_TOP_DIR)/Cargo.toml" -p forge --lib "$$prefix" -- --test-threads=1
