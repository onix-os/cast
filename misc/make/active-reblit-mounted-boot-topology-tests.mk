MOUNTED_BOOT_TOPOLOGY_TOP_DIR := $(abspath $(dir $(lastword $(MAKEFILE_LIST)))/../..)
CARGO ?= cargo

.PHONY: forge-active-reblit-mounted-boot-topology-test

forge-active-reblit-mounted-boot-topology-test: host-storage-safety-test
	@set -euo pipefail; \
	timeout 10s mkdir -p "$(MOUNTED_BOOT_TOPOLOGY_TOP_DIR)/target"; \
	listed="$$( timeout 10s mktemp "$(MOUNTED_BOOT_TOPOLOGY_TOP_DIR)/target/active-reblit-mounted-boot-topology-list.XXXXXXXXXXXX" )"; \
	trap 'timeout 10s rm -f "$$listed"' EXIT; \
	timeout 300s $(CARGO) test --manifest-path "$(MOUNTED_BOOT_TOPOLOGY_TOP_DIR)/Cargo.toml" -p forge --lib -- --list | timeout 300s tee "$$listed" >/dev/null; \
	timeout 10s grep -q . "$$listed"; \
	prefix='client::active_reblit_mounted_boot_topology::tests::'; \
	timeout 10s test "$$( timeout 10s grep -Ec "^$$prefix.*: test$$" "$$listed" )" = 18; \
	for name in \
		alias_is_structurally_one_target_and_exposes_only_closed_scalar_facts \
		distinct_targets_accept_every_required_inequality_and_same_parent_evidence \
		target_requires_nonzero_mount_id \
		target_requires_nonzero_destination_device_and_inode_identity \
		target_requires_destination_stat_device_to_match_typed_device \
		target_requires_descriptor_filesystem_evidence_for_the_exact_destination \
		target_requires_declarative_and_authenticated_partuuid_equality \
		distinct_rejects_equal_selectors \
		distinct_rejects_equal_destination_inode_identities \
		distinct_rejects_equal_mount_ids \
		distinct_rejects_equal_typed_devices_even_for_different_inodes \
		distinct_rejects_equal_partuuids_independently_of_other_facts \
		distinct_requires_explicit_same_revalidated_parent_evidence \
		distinct_same_parent_evidence_rejects_inconsistent_disk_sequences \
		every_later_phase_accepts_only_exact_alias_facts \
		exact_pass_comparison_covers_every_retained_target_fact \
		exact_pass_comparison_rejects_structural_alias_to_distinct_change \
		exact_pass_comparison_accepts_the_complete_distinct_fact_set; do \
		timeout 10s grep -Fqx "$$prefix$$name: test" "$$listed"; \
	done; \
	root="$(MOUNTED_BOOT_TOPOLOGY_TOP_DIR)/crates/forge/src/client/boot/active_reblit_mounted_boot_topology.rs"; \
	core="$(MOUNTED_BOOT_TOPOLOGY_TOP_DIR)/crates/forge/src/client/boot/active_reblit_mounted_boot_topology"; \
	tests="$(MOUNTED_BOOT_TOPOLOGY_TOP_DIR)/crates/forge/src/client/boot/active_reblit_mounted_boot_topology_tests.rs"; \
	timeout 10s grep -Fq 'pub(in crate::client) enum BoundActiveReblitMountedBootTopology' "$$core/model.rs"; \
	timeout 10s grep -Fq 'BootAliasesEsp {' "$$core/model.rs"; \
	timeout 10s grep -Fq 'same_revalidated_block_parent_snapshot: bool' "$$core/model.rs"; \
	timeout 10s grep -Fq 'mount_policy: ValidatedBootMountInfoPolicy' "$$core/model.rs"; \
	timeout 10s grep -Fq 'boot_filesystem: ValidatedBootFilesystemDescriptorEvidence' "$$core/model.rs"; \
	timeout 10s grep -Fq 'require_boot_filesystem_identity(phase, role, observation)?' "$$core/validation.rs"; \
	for error in DistinctSelectorAlias DistinctAttachmentAlias DistinctMountIdAlias DistinctDeviceAlias DistinctPartuuidAlias BlockParentMismatch PassFactsChanged; do \
		timeout 10s grep -Fq "$$error" "$$core/error.rs"; \
	done; \
	timeout 10s grep -Fq 'esp.destination == xbootldr.destination' "$$core/validation.rs"; \
	timeout 10s grep -Fq 'esp.device == xbootldr.device' "$$core/validation.rs"; \
	timeout 10s grep -Fq 'esp.disk_sequence != xbootldr.disk_sequence' "$$core/validation.rs"; \
	timeout 10s grep -Fq 'BootFilesystemIdentityMismatch' "$$core/error.rs"; \
	if timeout 10s rg -n 'std::fs|\bFile\b|OwnedFd|BorrowedFd|RawFd|PathBuf|Path[<>,)]|mountinfo::MountInfo|SelectedMountInfoAttachment|AuthenticatedMountInfoSnapshot|PreparedMountNamespaceAnchor|PreparedTaskRootedAttachment|PreparedSysfsPartitionIdentity|std::process|process::Command|Command::new|nix::mount|libc::mount|setns|unshare|chroot|pivot_root|open_tree|move_mount|/dev/disk|/dev/(sd|hd|vd|xvd|nvme|mmcblk|loop|md|dm-|nbd|zram)' "$$root" "$$core/error.rs" "$$core/model.rs" "$$core/validation.rs" "$$tests"; then exit 1; else status="$$?"; timeout 10s test "$$status" = 1; fi; \
	for file in "$$root" "$$core"/*.rs "$$tests" "$(MOUNTED_BOOT_TOPOLOGY_TOP_DIR)/misc/make/active-reblit-mounted-boot-topology-tests.mk"; do \
		timeout 10s test "$$( timeout 10s wc -l < "$$file" )" -le 1000; \
	done; \
	timeout 1200s $(CARGO) test --manifest-path "$(MOUNTED_BOOT_TOPOLOGY_TOP_DIR)/Cargo.toml" -p forge --lib "$$prefix" -- --test-threads=1
