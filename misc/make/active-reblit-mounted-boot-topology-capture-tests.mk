SHELL := /bin/bash

MOUNTED_BOOT_CAPTURE_TOP_DIR := $(abspath $(dir $(lastword $(MAKEFILE_LIST)))/../..)
CARGO ?= cargo

.PHONY: forge-active-reblit-mounted-boot-topology-capture-test

forge-active-reblit-mounted-boot-topology-capture-test: host-storage-safety-test
	@set -euo pipefail; \
	timeout 10s mkdir -p "$(MOUNTED_BOOT_CAPTURE_TOP_DIR)/target"; \
	listed="$$( timeout 10s mktemp "$(MOUNTED_BOOT_CAPTURE_TOP_DIR)/target/active-reblit-mounted-boot-capture-list.XXXXXXXXXXXX" )"; \
	trap 'timeout 10s rm -f "$$listed"' EXIT; \
	timeout 300s $(CARGO) test --manifest-path "$(MOUNTED_BOOT_CAPTURE_TOP_DIR)/Cargo.toml" -p forge --lib -- --list | timeout 300s tee "$$listed" >/dev/null; \
	timeout 10s grep -q . "$$listed"; \
	prefix='client::active_reblit_mounted_boot_topology::capture_tests::'; \
	timeout 10s test "$$( timeout 10s grep -Ec "^$$prefix.*: test$$" "$$listed" )" = 10; \
	for name in \
		stable::alias_fixture_retains_exact_descriptor_backed_scalar_facts \
		stable::repeated_revalidation_keeps_the_bootstrap_topology_exact \
		races::changed_declarative_intent_fails_at_the_opening_boundary \
		races::changed_mount_namespace_identity_fails_before_attachment_use \
		races::changed_attachment_identity_fails_before_mountinfo_selection \
		races::changed_mountinfo_identity_is_a_role_typed_selection_failure \
		races::changed_sysfs_identity_fails_after_exact_mountinfo_selection \
		races::attachment_selector_mismatch_is_role_typed_before_mountinfo_use \
		deadlines::expired_caller_deadline_is_rejected_at_coordinator_entry \
		deadlines::expiry_at_the_final_terminal_checkpoint_cannot_return_a_view; do \
		timeout 10s grep -Fqx "$$prefix$$name: test" "$$listed"; \
	done; \
	root="$(MOUNTED_BOOT_CAPTURE_TOP_DIR)/crates/forge/src/client/boot/active_reblit_mounted_boot_topology.rs"; \
	capture="$(MOUNTED_BOOT_CAPTURE_TOP_DIR)/crates/forge/src/client/boot/active_reblit_mounted_boot_topology/capture.rs"; \
	core="$(MOUNTED_BOOT_CAPTURE_TOP_DIR)/crates/forge/src/client/boot/active_reblit_mounted_boot_topology/capture"; \
	tests="$(MOUNTED_BOOT_CAPTURE_TOP_DIR)/crates/forge/src/client/boot/active_reblit_mounted_boot_topology_capture_tests.rs"; \
	test_core="$(MOUNTED_BOOT_CAPTURE_TOP_DIR)/crates/forge/src/client/boot/active_reblit_mounted_boot_topology_capture_tests"; \
	timeout 10s grep -Fq 'pub(in crate::client) struct PreparedActiveReblitMountedBootTopology' "$$core/model.rs"; \
	timeout 10s grep -Fq 'intent: PreparedActiveReblitBootTopologyIntent' "$$core/model.rs"; \
	timeout 10s grep -Fq 'anchor: PreparedMountNamespaceAnchor' "$$core/model.rs"; \
	timeout 10s grep -Fq 'BootAliasesEsp {' "$$core/model.rs"; \
	timeout 10s grep -Fq 'DistinctXbootldr {' "$$core/model.rs"; \
	timeout 10s grep -Fq 'attachment: PreparedTaskRootedAttachment' "$$core/model.rs"; \
	timeout 10s grep -Fq 'sysfs: PreparedSysfsPartitionIdentity' "$$core/model.rs"; \
	for phase in Pass1 Pass2 Terminal; do timeout 10s grep -Fq "ObservationPhase::$$phase" "$$core/preparation.rs"; done; \
	timeout 10s grep -Fq 'same_revalidated_block_parent_snapshot(&xbootldr_sysfs)' "$$core/observation.rs"; \
	timeout 10s grep -Fq 'intent_selector == attachment_selector' "$$core/observation.rs"; \
	timeout 10s grep -Fq 'read_count(), 4' "$$test_core/stable.rs"; \
	timeout 10s grep -Fq 'read_count(), 7' "$$test_core/stable.rs"; \
	if timeout 10s rg -n 'impl Clone for PreparedActiveReblitMountedBootTopology|std::process|process::Command|Command::new|nix::mount|libc::mount|mount_partitions|setns|unshare|chroot|pivot_root|open_tree|move_mount|canonicalize\(|(?:fs::|std::fs::|File::)?(?:create_dir(?:_all)?|rename|remove_file|write)\(|BLK[A-Z_]+|/dev/(disk|sd|hd|vd|xvd|nvme|mmcblk|loop|md|dm-|nbd|zram)' "$$capture" "$$core"; then exit 1; else status="$$?"; timeout 10s test "$$status" = 1; fi; \
	host_root_pattern='/''(boot|efi|esp)(/|["[:space:]]|$$)'; \
	if timeout 10s rg -n "$$host_root_pattern" "$$capture" "$$core"; then exit 1; else status="$$?"; timeout 10s test "$$status" = 1; fi; \
	if timeout 10s rg --pcre2 -U -n '#\[derive\([^\]]*Clone[^\]]*\)\]\s*pub\(in crate::client\) struct PreparedActiveReblitMountedBootTopology' "$$core/model.rs"; then exit 1; else status="$$?"; timeout 10s test "$$status" = 1; fi; \
	for file in "$$root" "$$capture" "$$core"/*.rs "$$tests" "$$test_core"/*.rs "$(MOUNTED_BOOT_CAPTURE_TOP_DIR)/misc/make/active-reblit-mounted-boot-topology-capture-tests.mk"; do \
		timeout 10s test "$$( timeout 10s wc -l < "$$file" )" -le 1000; \
	done; \
	timeout 1200s $(CARGO) test --manifest-path "$(MOUNTED_BOOT_CAPTURE_TOP_DIR)/Cargo.toml" -p forge --lib "$$prefix" -- --test-threads=1
