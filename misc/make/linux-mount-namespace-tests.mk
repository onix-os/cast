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
	timeout 10s test "$$( timeout 10s grep -Ec "^$$prefix.*: test$$" "$$listed" )" = 66; \
	for name in \
		attachment::bounds::zero_limits_and_expired_deadline_fail_before_attachment_hooks \
		attachment::bounds::attachment_hooks_are_finite_and_injected_failures_propagate \
		attachment::bounds::preparation_work_and_descriptor_budgets_have_exact_adjacent_boundaries \
		attachment::bounds::revalidation_budgets_cover_both_chain_passes_and_both_anchor_edges \
		attachment::boot_namespace::success_orders_stages_and_retains_only_exact_scalars_and_states \
		attachment::boot_namespace::opening_failure_skips_namespace_and_closing_without_a_result \
		attachment::boot_namespace::namespace_failure_skips_closing_and_returns_no_result \
		attachment::boot_namespace::closing_failure_discards_namespace_assessment \
		attachment::boot_namespace::opening_and_closing_filesystem_drift_discards_namespace_assessment \
		attachment::boot_namespace::identical_foreign_filesystem_identity_mismatches_discard_namespace_assessment \
		attachment::boot_namespace::every_observed_root_scalar_mismatch_discards_namespace_assessment \
		attachment::boot_namespace::missing_observed_root_discards_namespace_assessment \
		attachment::boot_namespace::empty_request_set_is_rejected_before_clock_or_any_stage \
		attachment::boot_namespace::expiry_after_opening_skips_namespace_and_closing \
		attachment::boot_namespace::expiry_after_namespace_discards_assessment_and_skips_closing \
		attachment::boot_namespace::terminal_deadline_expiry_discards_an_otherwise_complete_assessment \
		attachment::deadline::fixture_clock_rejects_attachment_prepare_and_revalidation_at_entry \
		attachment::deadline::fixture_clock_rejects_attachment_prepare_and_revalidation_at_final_checkpoint \
		attachment::device::non_dev_selector_is_rejected_before_injected_descriptor_observation \
		attachment::device::exact_dev_selector_retains_attachment_and_policy_scalars_in_closed_evidence \
		attachment::device::expired_deadline_rejects_exact_dev_before_injected_descriptor_observation \
		attachment::device::injected_descriptor_error_is_preserved_as_the_composition_source \
		attachment::device::injected_descriptor_identity_mismatch_fails_closed \
		attachment::gpt_device::exact_dev_authenticates_first_then_passes_unchanged_gpt_inputs_into_closed_result \
		attachment::gpt_device::non_dev_selector_rejects_before_either_authentication_layer \
		attachment::gpt_device::cross_wired_devtmpfs_identity_withholds_result_and_skips_gpt \
		attachment::gpt_device::injected_gpt_failure_is_preserved_and_withholds_closed_result \
		attachment::gpt_device::foreign_gpt_payload_claim_cannot_override_structural_mount_id \
		attachment::gpt_device::capture_rejects_a_foreign_gpt_root_mount_id_before_authentication \
		attachment::gpt_device::expiry_after_devtmpfs_withholds_result_before_gpt_authentication \
		attachment::gpt_device::expiry_after_gpt_discards_evidence_and_withholds_result \
		attachment::gpt_device::expiry_at_terminal_checkpoint_discards_matching_evidence_and_withholds_result \
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
	boot_composition="$$core/attachment/boot_namespace.rs"; \
	capture="$$core/attachment/capture.rs"; \
	runtime="$(MOUNT_NAMESPACE_TOP_DIR)/crates/forge/src/transition_journal/runtime_evidence.rs"; \
	timeout 10s grep -Fq 'pub(crate) fn prepare() -> io::Result<Self>' "$$root"; \
	timeout 10s grep -Fq 'pub(crate) fn prepare_until(deadline: Instant) -> io::Result<Self>' "$$root"; \
	timeout 10s grep -Fq 'pub(crate) fn revalidate(&self) -> io::Result<RevalidatedMountNamespaceAnchor<' "$$root"; \
	timeout 10s grep -Fq 'pub(crate) fn revalidate_until(&self, deadline: Instant)' "$$root"; \
	timeout 10s grep -Fq 'pub(crate) fn prepare_task_rooted_attachment_until(' "$$core/attachment.rs"; \
	timeout 10s grep -Fq 'pub(crate) fn revalidate_against_until(' "$$core/attachment.rs"; \
	timeout 10s grep -Fq 'pub(crate) fn authenticate_boot_filesystem_until(' "$$core/attachment.rs"; \
	timeout 10s grep -Fq 'self.current.authenticate_boot_filesystem_until(deadline)' "$$core/attachment.rs"; \
	timeout 10s grep -Fq 'mod boot_namespace;' "$$core/attachment.rs"; \
	timeout 10s grep -Fq 'pub(crate) struct ValidatedTaskRootBootNamespaceAssessment' "$$boot_composition"; \
	timeout 10s grep -Fq 'boot_filesystem: ValidatedBootFilesystemDescriptorEvidence,' "$$boot_composition"; \
	timeout 10s grep -Fq 'destination_mount_id: u64,' "$$boot_composition"; \
	timeout 10s grep -Fq 'namespace: ValidatedRetainedBootNamespaceAssessment,' "$$boot_composition"; \
	timeout 10s grep -Fq 'pub(crate) fn assess_retained_boot_namespace_until(' "$$boot_composition"; \
	timeout 10s grep -Fq 'expected: &[RetainedBootNamespaceExpectedSource<' "$$boot_composition"; \
	timeout 10s grep -Fq 'expected: &[RetainedBootNamespaceExpectedSource<' "$$capture"; \
	if timeout 10s rg -n '&\[&\[u8\]\]' "$$boot_composition" "$$capture"; then exit 1; else status="$$?"; timeout 10s test "$$status" = 1; fi; \
	timeout 10s rg --pcre2 -U -q '(?s)pub\(crate\) fn assess_retained_boot_namespace_until\([\s\S]{0,900}?if requests\.is_empty\(\)[\s\S]{0,500}?require_deadline\(deadline\)\?;[\s\S]{0,500}?let opening = self\s*\.current\s*\.authenticate_boot_filesystem_until\(deadline\)[\s\S]{0,500}?require_deadline\(deadline\)\?;[\s\S]{0,300}?let namespace = self\s*\.current\s*\.assess_retained_boot_namespace_until\([\s\S]{0,900}?require_deadline\(deadline\)\?;[\s\S]{0,300}?let closing = self\s*\.current\s*\.authenticate_boot_filesystem_until\(deadline\)[\s\S]{0,700}?close_assessment_until\(' "$$boot_composition"; \
	timeout 10s grep -Fq 'fn close_assessment_until(' "$$boot_composition"; \
	if timeout 10s rg -n 'pub(?:\([^)]*\))?[[:space:]]+fn close_assessment_until' "$$boot_composition"; then exit 1; else status="$$?"; timeout 10s test "$$status" = 1; fi; \
	timeout 10s grep -Fq 'if opening != closing' "$$boot_composition"; \
	timeout 10s grep -Fq 'opening.destination_device() != destination.device' "$$boot_composition"; \
	timeout 10s grep -Fq 'opening.destination_inode() != destination.inode' "$$boot_composition"; \
	timeout 10s grep -Fq 'let observed_root = namespace.observed_root_identity()' "$$boot_composition"; \
	timeout 10s grep -Fq 'root.device != destination.device' "$$boot_composition"; \
	timeout 10s grep -Fq 'root.inode != destination.inode' "$$boot_composition"; \
	timeout 10s grep -Fq 'root.mount_id != destination.mount_id' "$$boot_composition"; \
	timeout 10s rg --pcre2 -U -q '(?s)fn close_assessment_until\([\s\S]{0,1800}?validate_closed_evidence\([^;]+\)\?;[\s\S]{0,300}?require_deadline\(deadline\)\?;[\s\S]{0,300}?Ok\(ValidatedTaskRootBootNamespaceAssessment' "$$boot_composition"; \
	timeout 10s test "$$( timeout 10s rg -c '\bValidatedTaskRootBootNamespaceAssessment \{' "$$boot_composition" )" = 3; \
	timeout 10s test "$$( timeout 10s rg -c 'Ok\(ValidatedTaskRootBootNamespaceAssessment \{' "$$boot_composition" )" = 1; \
	timeout 10s grep -Fq 'pub(crate) struct FixtureRetainedBootNamespaceAssessment' "$$boot_composition"; \
	if timeout 10s awk '/pub\(crate\) struct FixtureRetainedBootNamespaceAssessment/ { exit } { print }' "$$boot_composition" | timeout 10s rg -n 'fn[[:space:]]+[[:alnum:]_]+[[:space:]]*<|impl[[:space:]]+Fn(?:Once|Mut)?|dyn[[:space:]]+Fn(?:Once|Mut)?|ClosedComposition|FixtureNamespacePayload|FixtureRetainedBootNamespaceAssessment<'; then exit 1; else status="$$?"; timeout 10s test "$$status" = 1; fi; \
	if timeout 10s awk '/pub\(crate\) struct ValidatedTaskRootBootNamespaceAssessment/ { emit=1 } emit { print } /^}/ && emit { exit }' "$$boot_composition" | timeout 10s rg -ni '\b(?:File|Path|OwnedFd|BorrowedFd|RawFd|fd|callback|reader|reopen)\b'; then exit 1; else status="$$?"; timeout 10s test "$$status" = 1; fi; \
	timeout 10s grep -Fq 'pub(crate) fn authenticate_devtmpfs_attachment_until(' "$$core/attachment.rs"; \
	timeout 10s grep -Fq 'self.current.authenticate_devtmpfs_same_mount_until(policy, deadline)' "$$core/attachment.rs"; \
	timeout 10s grep -Fq 'pub(in crate::linux_fs) fn authenticate_devtmpfs_gpt_partition_device_until(' "$$core/attachment/gpt_device.rs"; \
	timeout 10s grep -Fq '.authenticate_devtmpfs_attachment_until(policy, deadline)' "$$core/attachment/gpt_device.rs"; \
	timeout 10s grep -Fq '.authenticate_gpt_parent_until(devtmpfs_attachment.mount_id(), expected, expected_role, deadline)' "$$core/attachment/gpt_device.rs"; \
	timeout 10s grep -Fq 'bind_authenticated_evidence_until(devtmpfs_attachment, gpt_partition_device, deadline)' "$$core/attachment/gpt_device.rs"; \
	timeout 10s grep -Fq 'pub(super) fn authenticate_boot_filesystem_until(' "$$core/attachment/capture.rs"; \
	timeout 10s grep -Fq 'authenticate_boot_filesystem_directory_until(' "$$core/attachment/capture.rs"; \
	timeout 10s grep -Fq '&destination.file,' "$$core/attachment/capture.rs"; \
	timeout 10s grep -Fq 'pub(super) fn assess_retained_boot_namespace_until(' "$$capture"; \
	timeout 10s rg --pcre2 -U -q '(?s)pub\(super\) fn assess_retained_boot_namespace_until\([\s\S]{0,1200}?assess_retained_boot_namespace_until\(\s*&destination\.file,\s*requests,\s*expected,\s*namespace_limits,\s*live_limits,\s*deadline,\s*\)' "$$capture"; \
	timeout 10s grep -Fq 'self.destination_witness.device,' "$$core/attachment/capture.rs"; \
	timeout 10s grep -Fq 'self.destination_witness.inode,' "$$core/attachment/capture.rs"; \
	timeout 10s grep -Fq 'pub(super) fn authenticate_devtmpfs_same_mount_until(' "$$core/attachment/capture.rs"; \
	timeout 10s grep -Fq 'authenticate_devtmpfs_same_mount_directory_until(' "$$core/attachment/capture.rs"; \
	timeout 10s grep -Fq 'self.destination_witness.mount_id,' "$$core/attachment/capture.rs"; \
	timeout 10s grep -Fq 'pub(super) fn authenticate_gpt_parent_until(' "$$core/attachment/capture.rs"; \
	timeout 10s grep -Fq 'self.require_gpt_root_mount_id(authenticated_root_mount_id)?' "$$core/attachment/capture.rs"; \
	timeout 10s grep -Fq 'authenticate_retained_devtmpfs_gpt_partition_device_until(' "$$core/attachment/capture.rs"; \
	timeout 10s rg --pcre2 -U -q '(?s)pub\(super\) fn authenticate_gpt_parent_until\([\s\S]{0,1800}?authenticate_retained_devtmpfs_gpt_partition_device_until\(\s*&destination\.file,\s*authenticated_root_mount_id,\s*expected,\s*expected_role,\s*deadline,\s*\)' "$$core/attachment/capture.rs"; \
	timeout 10s grep -Fq 'fn bind_authenticated_evidence_until(' "$$core/attachment/gpt_device.rs"; \
	if timeout 10s rg -n 'pub[^[:space:]]*[[:space:]]+fn bind_authenticated_evidence_until|authenticate_task_root_devtmpfs_gpt_partition_device_until|ClosedComposition|io::Result<\(u64, GptEvidence\)>' "$$core/attachment.rs" "$$core/attachment"; then exit 1; else status="$$?"; timeout 10s test "$$status" = 1; fi; \
	timeout 10s grep -Fq 'pub(crate) struct FixtureGptPartitionDeviceEvidence' "$$core/attachment/gpt_device.rs"; \
	if timeout 10s awk '/pub\(crate\) struct FixtureGptPartitionDeviceEvidence/ { exit } { print }' "$$core/attachment/gpt_device.rs" | timeout 10s rg -n 'fn[[:space:]]+[[:alnum:]_]+[[:space:]]*<|impl[[:space:]]+Fn(?:Once|Mut)?|dyn[[:space:]]+Fn(?:Once|Mut)?|ClosedComposition|io::Result<[[:space:]]*\('; then exit 1; else status="$$?"; timeout 10s test "$$status" = 1; fi; \
	timeout 10s grep -Fq 'let gpt_mount_id = gpt_partition_device.mount_id();' "$$core/attachment/gpt_device.rs"; \
	timeout 10s grep -Fq 'if gpt_mount_id != devtmpfs_mount_id' "$$core/attachment/gpt_device.rs"; \
	timeout 10s grep -Fq 'does not prevent `setns(2)` on the same thread' "$$core/attachment/gpt_device.rs"; \
	timeout 10s grep -Fq 'const EXACT_TASK_ROOT_DEVICE_SELECTOR: &str = "/dev";' "$$core/attachment/device.rs"; \
	timeout 10s grep -Fq 'if selector != EXACT_TASK_ROOT_DEVICE_SELECTOR' "$$core/attachment/device.rs"; \
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
	if timeout 10s rg --pcre2 -U -n '(?s)pub(?:\(crate\)|\(in crate::linux_fs\))[^;{]{0,512}\b(?:File|OwnedFd|BorrowedFd|RawFd|AsRawFd|raw_fd|as_raw_fd)\b[^;{]*[;{]' "$$core/attachment.rs" "$$core/attachment"; then exit 1; else status="$$?"; timeout 10s test "$$status" = 1; fi; \
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
