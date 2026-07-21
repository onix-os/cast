MOUNTED_BOOT_CAPTURE_TOP_DIR := $(abspath $(dir $(lastword $(MAKEFILE_LIST)))/../..)
CARGO ?= cargo

.PHONY: forge-active-reblit-mounted-boot-topology-capture-test

forge-active-reblit-mounted-boot-topology-capture-test: host-storage-safety-test forge-linux-mount-boot-policy-test forge-linux-descriptor-boot-filesystem-test
	@set -euo pipefail; \
	timeout 10s mkdir -p "$(MOUNTED_BOOT_CAPTURE_TOP_DIR)/target"; \
	listed="$$( timeout 10s mktemp "$(MOUNTED_BOOT_CAPTURE_TOP_DIR)/target/active-reblit-mounted-boot-capture-list.XXXXXXXXXXXX" )"; \
	trap 'timeout 10s rm -f "$$listed"' EXIT; \
	timeout 300s $(CARGO) test --manifest-path "$(MOUNTED_BOOT_CAPTURE_TOP_DIR)/Cargo.toml" -p forge --lib -- --list | timeout 300s tee "$$listed" >/dev/null; \
	timeout 10s grep -q . "$$listed"; \
	prefix='client::active_reblit_mounted_boot_topology::capture_tests::'; \
	timeout 10s test "$$( timeout 10s grep -Ec "^$$prefix.*: test$$" "$$listed" )" = 28; \
	for name in \
		stable::alias_fixture_retains_exact_descriptor_backed_scalar_facts \
		stable::repeated_revalidation_keeps_the_bootstrap_topology_exact \
		filesystem::stable_msdos_family_evidence_is_retained_in_every_observation \
		filesystem::wrong_boot_filesystem_magic_is_a_bootstrap_role_typed_failure \
		filesystem::boot_filesystem_identity_mismatch_is_a_bootstrap_role_typed_failure \
		filesystem::distinct_bootstrap_consumes_both_feeds_and_types_xbootldr_wrong_magic \
		filesystem::distinct_bootstrap_types_xbootldr_identity_mismatch_after_both_feeds \
		filesystem::evidence_drift_is_rejected_in_pass2_and_terminal_observations \
		publication_targets::alias_bridge_brackets_exact_attachment_and_retains_original_deadline \
		publication_targets::bridge_rejects_intent_drift_in_its_opening_complete_topology_pass \
		publication_targets::bridge_rejects_attachment_drift_in_its_closing_complete_topology_pass \
		publication_targets::bridge_terminal_checkpoint_cannot_outlive_original_deadline \
		publication_targets::scalar_binding_rejects_each_role_typed_identity_component_drift \
		races::bootstrap_rejects_an_unsupported_boot_filesystem_policy \
		races::changed_declarative_intent_fails_at_the_opening_boundary \
		races::changed_mount_namespace_identity_fails_before_attachment_use \
		races::changed_attachment_identity_fails_before_mountinfo_selection \
		races::changed_mountinfo_identity_is_a_role_typed_selection_failure \
		races::changed_mountinfo_filesystem_policy_is_role_typed_before_sysfs_use \
		races::changed_mount_read_write_policy_is_role_typed \
		races::changed_superblock_read_write_policy_is_role_typed \
		races::each_required_security_flag_drift_is_role_typed \
		races::irrelevant_mountinfo_policy_churn_keeps_the_closed_facts_exact \
		races::changed_sysfs_identity_fails_after_exact_mountinfo_selection \
		races::attachment_selector_mismatch_is_role_typed_before_mountinfo_use \
		deadlines::expired_caller_deadline_is_rejected_at_coordinator_entry \
		deadlines::successful_fixture_revalidation_retains_exact_caller_deadline \
		deadlines::expiry_at_the_final_terminal_checkpoint_cannot_return_a_view; do \
		timeout 10s grep -Fqx "$$prefix$$name: test" "$$listed"; \
	done; \
	root="$(MOUNTED_BOOT_CAPTURE_TOP_DIR)/crates/forge/src/client/boot/active_reblit_mounted_boot_topology.rs"; \
	capture="$(MOUNTED_BOOT_CAPTURE_TOP_DIR)/crates/forge/src/client/boot/active_reblit_mounted_boot_topology/capture.rs"; \
	core="$(MOUNTED_BOOT_CAPTURE_TOP_DIR)/crates/forge/src/client/boot/active_reblit_mounted_boot_topology/capture"; \
	publication_targets="$$core/publication_targets.rs"; \
	renderer="$(MOUNTED_BOOT_CAPTURE_TOP_DIR)/crates/forge/src/client/boot/active_reblit_bls_renderer.rs"; \
	tests="$(MOUNTED_BOOT_CAPTURE_TOP_DIR)/crates/forge/src/client/boot/active_reblit_mounted_boot_topology_capture_tests.rs"; \
	test_core="$(MOUNTED_BOOT_CAPTURE_TOP_DIR)/crates/forge/src/client/boot/active_reblit_mounted_boot_topology_capture_tests"; \
	timeout 10s grep -Fq 'pub(in crate::client) struct PreparedActiveReblitMountedBootTopology' "$$core/model.rs"; \
	timeout 10s grep -Fq 'intent: PreparedActiveReblitBootTopologyIntent' "$$core/model.rs"; \
	timeout 10s grep -Fq 'anchor: PreparedMountNamespaceAnchor' "$$core/model.rs"; \
	timeout 10s grep -Fq 'BootAliasesEsp {' "$$core/model.rs"; \
	timeout 10s grep -Fq 'DistinctXbootldr {' "$$core/model.rs"; \
	timeout 10s grep -Fq 'attachment: PreparedTaskRootedAttachment' "$$core/model.rs"; \
	timeout 10s grep -Fq 'sysfs: PreparedSysfsPartitionIdentity' "$$core/model.rs"; \
	timeout 10s grep -Fq 'boot_filesystem_source: BootFilesystemEvidenceSource' "$$core/model.rs"; \
	timeout 10s grep -Fq 'deadline: Instant' "$$core/model.rs"; \
	timeout 10s grep -Fq 'pub(in crate::client) fn deadline(&self) -> Instant' "$$core/model.rs"; \
	timeout 10s grep -Fq 'deadline,' "$$core/preparation.rs"; \
	timeout 10s grep -Fq 'assert_eq!(view.deadline(), operation_deadline);' "$$test_core/deadlines.rs"; \
	timeout 10s grep -Fq 'pub(in crate::client) enum RevalidatedActiveReblitBootPublicationTargets' "$$publication_targets"; \
	timeout 10s grep -Fq 'attachment: RevalidatedTaskRootedAttachment' "$$publication_targets"; \
	timeout 10s test "$$( timeout 10s grep -Fc '.revalidate_until(self._installation, deadline)' "$$publication_targets" )" = 2; \
	timeout 10s grep -Fq '.revalidate_against_until(anchor, deadline)' "$$publication_targets"; \
	for scalar in destination_device destination_inode destination_mount_id; do timeout 10s grep -Fq "attachment.$$scalar()" "$$publication_targets"; done; \
	timeout 10s grep -Fq 'self.topology.revalidate_publication_targets()' "$$renderer"; \
	if timeout 10s rg --pcre2 -U -n '#\[derive\([^]]*(?:Clone|Copy)[^]]*\)\]\s*pub\(in crate::client\) (?:struct RevalidatedActiveReblitBootPublicationTarget|enum RevalidatedActiveReblitBootPublicationTargets)|impl(?:<[^>]+>)?\s+(?:Clone|Copy)\s+for\s+(?:RevalidatedActiveReblitBootPublicationTarget|RevalidatedActiveReblitBootPublicationTargets)' "$$publication_targets"; then exit 1; else status="$$?"; timeout 10s test "$$status" = 1; fi; \
	if timeout 10s rg --pcre2 -U -n 'pub\(in crate::client\)\s+fn\s+[[:alnum:]_]+[^\{;]{0,300}(?:RevalidatedTaskRootedAttachment|PreparedTaskRootedAttachment|std::fs::File|OwnedFd|BorrowedFd|RawFd|PathBuf|&Path)|pub\(in crate::client\).*attachment\s*:' "$$publication_targets"; then exit 1; else status="$$?"; timeout 10s test "$$status" = 1; fi; \
	if timeout 10s rg -n 'retain_boot_publication_parent|publish_immutable_boot_file|assess_retained_boot_namespace|create_dir|mkdir|rename|unlink|remove_file|syncfs|sync_all' "$$publication_targets"; then exit 1; else status="$$?"; timeout 10s test "$$status" = 1; fi; \
	for phase in Pass1 Pass2 Terminal; do timeout 10s grep -Fq "ObservationPhase::$$phase" "$$core/preparation.rs"; done; \
	timeout 10s grep -Fq 'same_revalidated_block_parent_snapshot(&xbootldr_sysfs)' "$$core/observation.rs"; \
	timeout 10s grep -Fq 'intent_selector == attachment_selector' "$$core/observation.rs"; \
	timeout 10s grep -Fq 'validate_selected_boot_mount_policy_until(selected, deadline)' "$$core/observation.rs"; \
	timeout 10s grep -Fq 'attachment.authenticate_boot_filesystem_until(deadline)' "$$core/observation.rs"; \
	timeout 10s grep -Fq 'BootFilesystem {' "$$core/error.rs"; \
	timeout 10s grep -Fq 'MountInfoPolicy {' "$$core/error.rs"; \
	timeout 10s grep -Fq 'rw,nosuid,nodev,noexec,nosymfollow' "$$test_core/support.rs"; \
	timeout 10s grep -Fq 'read_count(), 4' "$$test_core/filesystem.rs"; \
	timeout 10s grep -Fq 'read_count(), 7' "$$test_core/filesystem.rs"; \
	if timeout 10s rg -n 'impl Clone for PreparedActiveReblitMountedBootTopology|std::process|process::Command|Command::new|nix::mount|libc::mount|mount_partitions|setns|unshare|chroot|pivot_root|open_tree|move_mount|canonicalize\(|(?:fs::|std::fs::|File::)?(?:create_dir(?:_all)?|rename|remove_file|write)\(|BLK[A-Z_]+|/dev/(disk|sd|hd|vd|xvd|nvme|mmcblk|loop|md|dm-|nbd|zram)' "$$capture" "$$core"; then exit 1; else status="$$?"; timeout 10s test "$$status" = 1; fi; \
	host_root_pattern='/''(boot|efi|esp)(/|["[:space:]]|$$)'; \
	if timeout 10s rg -n "$$host_root_pattern" "$$capture" "$$core"; then exit 1; else status="$$?"; timeout 10s test "$$status" = 1; fi; \
	if timeout 10s rg --pcre2 -U -n '#\[derive\([^\]]*Clone[^\]]*\)\]\s*pub\(in crate::client\) struct PreparedActiveReblitMountedBootTopology' "$$core/model.rs"; then exit 1; else status="$$?"; timeout 10s test "$$status" = 1; fi; \
	for file in "$$root" "$$capture" "$$core"/*.rs "$$tests" "$$test_core"/*.rs "$(MOUNTED_BOOT_CAPTURE_TOP_DIR)/misc/make/active-reblit-mounted-boot-topology-capture-tests.mk"; do \
		timeout 10s test "$$( timeout 10s wc -l < "$$file" )" -le 1000; \
	done; \
	timeout 1200s $(CARGO) test --manifest-path "$(MOUNTED_BOOT_CAPTURE_TOP_DIR)/Cargo.toml" -p forge --lib "$$prefix" -- --test-threads=1
