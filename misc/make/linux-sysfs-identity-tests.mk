SHELL := /bin/bash

SYSFS_IDENTITY_TOP_DIR := $(abspath $(dir $(lastword $(MAKEFILE_LIST)))/../..)
CARGO ?= cargo

.PHONY: forge-linux-sysfs-identity-test

forge-linux-sysfs-identity-test: host-storage-safety-test
	@set -euo pipefail; \
	timeout 10s mkdir -p "$(SYSFS_IDENTITY_TOP_DIR)/target"; \
	listed="$$( timeout 10s mktemp "$(SYSFS_IDENTITY_TOP_DIR)/target/linux-sysfs-identity-test-list.XXXXXXXXXXXX" )"; \
	trap 'timeout 10s rm -f "$$listed"' EXIT; \
	timeout 300s $(CARGO) test --manifest-path "$(SYSFS_IDENTITY_TOP_DIR)/Cargo.toml" -p forge --lib -- --list | timeout 300s tee "$$listed" >/dev/null; \
	timeout 10s grep -q . "$$listed"; \
	prefix='linux_fs::tests::sysfs_identity::'; \
	timeout 10s test "$$( timeout 10s grep -Ec "^$$prefix.*: test$$" "$$listed" )" = 25; \
	for name in \
		bounds::zero_fixture_limits_and_expired_deadlines_fail_before_hooks_or_syscalls \
		bounds::injected_checkpoint_failure_is_propagated_without_external_effects \
		bounds::preparation_work_ancestor_and_descriptor_limits_have_exact_boundaries \
		bounds::revalidation_budget_is_global_across_both_recaptures_and_terminal_checks \
		bounds::attribute_read_ceilings_accept_exact_bytes_and_reject_one_more \
		bounds::uevent_and_link_bounds_are_enforced_before_resolution_or_parsing \
		malformed::fixture_admission_accepts_only_one_retained_directory_component \
		malformed::every_required_lookup_target_and_attribute_must_exist \
		malformed::dev_block_lookup_target_cannot_escape_or_name_a_non_device_path \
		malformed::target_and_attribute_entry_kinds_fail_closed_without_opening_special_files \
		malformed::partition_attributes_reject_non_utf8_and_cross_file_disagreement \
		malformed::parent_attributes_reject_non_disk_or_internally_inconsistent_evidence \
		malformed::caller_device_number_is_exact_and_never_triggers_discovery \
		races::retained_root_and_lookup_name_races_fail_closed \
		races::lookup_link_and_normalized_target_races_fail_closed_between_passes \
		races::partition_attribute_inode_and_content_races_fail_closed \
		races::subsystem_ancestor_and_selected_parent_races_fail_closed \
		races::parent_attribute_and_terminal_rebind_races_fail_closed \
		races::revalidation_repeats_the_complete_race_resistant_capture \
		stable::stable_fixture_captures_and_revalidates_exact_partition_identity \
		stable::revalidated_views_compare_only_retained_block_parent_snapshots \
		stable::non_block_intermediate_ancestors_are_skipped_without_lexical_parent_assumptions \
		stable::nearest_block_ancestor_must_itself_be_an_exact_disk \
		stable::selected_parent_must_not_itself_have_a_partition_attribute \
		stable::disk_sequence_is_either_absent_on_both_nodes_or_equal_on_both; do \
		timeout 10s grep -Fqx "$$prefix$$name: test" "$$listed"; \
	done; \
	root="$(SYSFS_IDENTITY_TOP_DIR)/crates/forge/src/linux_fs/sysfs_identity.rs"; \
	capture="$(SYSFS_IDENTITY_TOP_DIR)/crates/forge/src/linux_fs/sysfs_identity/capture.rs"; \
	filesystem="$(SYSFS_IDENTITY_TOP_DIR)/crates/forge/src/linux_fs/sysfs_identity/filesystem.rs"; \
	timeout 10s grep -Fq 'pub(crate) fn prepare(device: SysfsDeviceNumber) -> io::Result<Self>' "$$root"; \
	timeout 10s grep -Fq 'pub(crate) fn revalidate(&self) -> io::Result<RevalidatedSysfsPartitionIdentity<' "$$root"; \
	timeout 10s grep -Fq 'pub(crate) fn has_same_revalidated_block_parent_snapshot(&self, other: &Self) -> bool' "$$root"; \
	timeout 10s grep -Fq 'pub(crate) fn normalized_devpath(&self) -> &[u8]' "$$root"; \
	timeout 10s test "$$( timeout 10s grep -Fc 'PhantomData<Rc<()>>' "$$root" )" -ge 2; \
	timeout 10s grep -Fq 'let first = capture_once(root, device, operation)?;' "$$capture"; \
	timeout 10s grep -Fq 'let second = capture_once(root, device, operation)?;' "$$capture"; \
	timeout 10s grep -Fq 'operation.emit(CaptureCheckpoint::TerminalRebind)?;' "$$capture"; \
	timeout 10s grep -Fq 'attribute_absent(&directory.file, c"partition", operation)?;' "$$capture"; \
	timeout 10s grep -Fq 'parse_sysfs_disk_identity_until(' "$$capture"; \
	timeout 10s grep -Fq 'require_matching_disk_sequence_until(' "$$capture"; \
	timeout 10s grep -Fq 'operation.emit(CaptureCheckpoint::FinalNameRebind)?;' "$$capture"; \
	timeout 10s grep -Fq 'c"/sys"' "$$filesystem"; \
	timeout 10s grep -Fq 'require_sysfs_until(&file, std::path::Path::new("/sys"), operation.deadline())?;' "$$filesystem"; \
	timeout 10s grep -Fq 'nix::libc::O_PATH | nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW' "$$filesystem"; \
	timeout 10s grep -Fq 'nix::libc::readlinkat(' "$$filesystem"; \
	timeout 10s grep -Fq 'let buffer_len = max_bytes' "$$filesystem"; \
	timeout 10s grep -Fq 'let sentinel = max_bytes' "$$filesystem"; \
	timeout 10s test "$$( timeout 10s grep -Fc '.checked_add(1)' "$$filesystem" )" -ge 2; \
	timeout 10s grep -Fq 'fn descriptor_mount_id(' "$$filesystem"; \
	timeout 10s grep -Fq 'authenticated_current_thread_procfs_with_deadline(Some(operation.deadline()))?' "$$filesystem"; \
	timeout 10s grep -Fq 'parse_descriptor_mount_id(&bytes)?' "$$filesystem"; \
	timeout 10s grep -Fq 'reject_kernel_pseudo_fixture(&parent, operation, "fixture sysfs parent")?;' "$$filesystem"; \
	timeout 10s grep -Fq 'if status.f_type == SYSFS_MAGIC || status.f_type == PROC_SUPER_MAGIC {' "$$filesystem"; \
	if timeout 10s rg -n 'read_dir|canonicalize|std::process|process::Command|Command::new|std::env|/dev/disk|/dev/(sd|hd|vd|xvd|nvme|mmcblk|loop|md|dm-|nbd|zram)|blkid|lsblk|findmnt|udevadm|smartctl|hdparm|OpenOptions|create_new|set_len|write_all|remove_(file|dir)|rename\(' "$$root" "$(SYSFS_IDENTITY_TOP_DIR)/crates/forge/src/linux_fs/sysfs_identity"; then exit 1; else status="$$?"; timeout 10s test "$$status" = 1; fi; \
	if timeout 10s rg -n 'PreparedSysfsPartitionIdentity::prepare|(?:std::fs::|fs::)?File::open\("/sys(?:/|"\))|read_dir|canonicalize|std::process|process::Command|Command::new|/dev/disk|/dev/(sd|hd|vd|xvd|nvme|mmcblk|loop|md|dm-|nbd|zram)|blkid|lsblk|findmnt|udevadm|smartctl|hdparm' "$(SYSFS_IDENTITY_TOP_DIR)/crates/forge/src/linux_fs/tests/sysfs_identity.rs" "$(SYSFS_IDENTITY_TOP_DIR)/crates/forge/src/linux_fs/tests/sysfs_identity"; then exit 1; else status="$$?"; timeout 10s test "$$status" = 1; fi; \
	timeout 10s grep -Fq 'pub(super) const PARTITION_MAJOR: u32 = u32::MAX;' "$(SYSFS_IDENTITY_TOP_DIR)/crates/forge/src/linux_fs/tests/sysfs_identity/support.rs"; \
	for file in \
		"$$root" \
		"$(SYSFS_IDENTITY_TOP_DIR)"/crates/forge/src/linux_fs/sysfs_identity/*.rs \
		"$(SYSFS_IDENTITY_TOP_DIR)/crates/forge/src/linux_fs/tests/sysfs_identity.rs" \
		"$(SYSFS_IDENTITY_TOP_DIR)"/crates/forge/src/linux_fs/tests/sysfs_identity/*.rs \
		"$(SYSFS_IDENTITY_TOP_DIR)/misc/make/linux-sysfs-identity-tests.mk"; do \
		timeout 10s test "$$( timeout 10s wc -l < "$$file" )" -le 1000; \
	done; \
	timeout 1200s $(CARGO) test --manifest-path "$(SYSFS_IDENTITY_TOP_DIR)/Cargo.toml" -p forge --lib "$$prefix" -- --test-threads=1
