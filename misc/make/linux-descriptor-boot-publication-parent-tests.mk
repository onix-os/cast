DESCRIPTOR_BOOT_PUBLICATION_PARENT_TOP_DIR := $(abspath $(dir $(lastword $(MAKEFILE_LIST)))/../..)
CARGO ?= cargo

.PHONY: forge-linux-descriptor-boot-publication-parent-test \
	forge-linux-descriptor-boot-publication-parent-unit-test

forge-linux-descriptor-boot-publication-parent-unit-test:
	@$(CARGO) test --manifest-path "$(DESCRIPTOR_BOOT_PUBLICATION_PARENT_TOP_DIR)/Cargo.toml" -p forge --lib 'linux_fs::tests::descriptor_boot_publication_parent::' -- --test-threads=1

forge-linux-descriptor-boot-publication-parent-test: host-storage-safety-test forge-linux-descriptor-boot-file-publication-test
	@set -euo pipefail; \
	mkdir -p "$(DESCRIPTOR_BOOT_PUBLICATION_PARENT_TOP_DIR)/target"; \
	listed="$$( mktemp "$(DESCRIPTOR_BOOT_PUBLICATION_PARENT_TOP_DIR)/target/linux-descriptor-boot-publication-parent-test-list.XXXXXXXXXXXX" )"; \
	trap 'rm -f "$$listed"' EXIT; \
	$(CARGO) test --manifest-path "$(DESCRIPTOR_BOOT_PUBLICATION_PARENT_TOP_DIR)/Cargo.toml" -p forge --lib -- --list | tee "$$listed" >/dev/null; \
	grep -q . "$$listed"; \
	prefix='linux_fs::tests::descriptor_boot_publication_parent::'; \
	test "$$( grep -Ec "^$$prefix.*: test$$" "$$listed" )" = 15; \
	for name in \
		existing_parent_chain_is_reused_without_inode_replacement \
		existing_only_parent_chain_is_retained_without_effect_emission \
		existing_only_parent_retention_never_creates_a_missing_component \
		existing_only_parent_retention_never_recreates_a_post_assessment_race \
		multi_component_chain_is_created_retained_and_same_root_bound \
		mkdir_error_report_after_applied_is_reconciled_without_second_attempt \
		interrupted_creation_residue_is_re_admitted_with_the_same_inode \
		directory_durability_runs_deepest_child_to_root_before_filesystem_and_terminal_checks \
		terminal_name_substitution_is_preserved_but_refused \
		intermediate_parent_substitution_is_refused_before_deeper_creation \
		regular_symlink_and_writable_directory_components_are_refused_without_replacement \
		foreign_device_and_mount_id_are_rejected_by_the_closed_identity_policy \
		root_credentials_and_child_owner_group_mode_drift_are_rejected \
		only_nonempty_bounded_raw_parent_components_reach_the_syscall_boundary \
		nested_parent_consumes_the_leaf_engine_and_binds_reusable_source_evidence; do \
		grep -Fqx "$$prefix$$name: test" "$$listed"; \
	done; \
	module="$(DESCRIPTOR_BOOT_PUBLICATION_PARENT_TOP_DIR)/crates/forge/src/linux_fs/mount_namespace/attachment/boot_publication_parent.rs"; \
	existing="$(DESCRIPTOR_BOOT_PUBLICATION_PARENT_TOP_DIR)/crates/forge/src/linux_fs/mount_namespace/attachment/boot_publication_parent/existing.rs"; \
	effect="$(DESCRIPTOR_BOOT_PUBLICATION_PARENT_TOP_DIR)/crates/forge/src/linux_fs/mount_namespace/attachment/boot_publication_parent/effect.rs"; \
	leaf="$(DESCRIPTOR_BOOT_PUBLICATION_PARENT_TOP_DIR)/crates/forge/src/linux_fs/mount_namespace/attachment/boot_file_publication.rs"; \
	access="$(DESCRIPTOR_BOOT_PUBLICATION_PARENT_TOP_DIR)/crates/forge/src/linux_fs/descriptor_access.rs"; \
	tests="$(DESCRIPTOR_BOOT_PUBLICATION_PARENT_TOP_DIR)/crates/forge/src/linux_fs/tests/descriptor_boot_publication_parent.rs"; \
	grep -Fq 'pub(crate) struct RetainedBootPublicationParent' "$$module"; \
	grep -Fq "root: &'view RevalidatedTaskRootedAttachment" "$$module"; \
	grep -Fq 'chain: Vec<RetainedPublicationDirectory>' "$$module"; \
	grep -Fq 'pub(crate) fn retain_boot_publication_parent_until' "$$module"; \
	grep -Fq 'pub(crate) fn retain_existing_boot_publication_parent_until' "$$existing"; \
	test "$$( grep -Fc 'mkdirat_once(parent, name, CREATED_DIRECTORY_MODE)' "$$module" )" = 1; \
	grep -Fq 'ParentRetentionMode::ExistingOnly' "$$existing"; \
	grep -Fq 'ParentRetentionMode::ExistingOnly => (' "$$module"; \
	grep -Fq 'open_existing_component(parent, &name, index, deadline)?' "$$module"; \
	grep -Fq 'ExistingComponentMissing { index }' "$$module"; \
	test "$$( grep -Fc 'if mode == ParentRetentionMode::CreateMissing {' "$$module" )" = 2; \
	if rg -n 'mkdirat_once|sync_filesystem_until|\.sync_all\(|effect::|ParentCheckpoint' "$$existing"; then exit 1; else status="$$?"; test "$$status" = 1; fi; \
	grep -Fq 'controlled_resolution()' "$$module"; \
	grep -Fq 'nix::libc::RESOLVE_NO_MAGICLINKS' "$$access"; \
	grep -Fq 'nix::libc::RESOLVE_NO_SYMLINKS' "$$access"; \
	grep -Fq 'nix::libc::RESOLVE_NO_XDEV' "$$access"; \
	grep -Fq 'found.device != root.device' "$$module"; \
	grep -Fq 'found.mount_id != root.mount_id' "$$module"; \
	grep -Fq 'root.uid != effective_uid' "$$module"; \
	grep -Fq 'root.gid != effective_gid' "$$module"; \
	grep -Fq 'found.uid != root.uid' "$$module"; \
	grep -Fq 'found.gid != root.gid' "$$module"; \
	grep -Fq 'found.mode & 0o022 != 0' "$$module"; \
	grep -Fq 'found.mode & 0o7000 != 0' "$$module"; \
	grep -Fq '.iter().enumerate().rev()' "$$module"; \
	grep -Fq 'sync_filesystem_until(&readable_root, deadline)' "$$module"; \
	grep -Fq 'require_named_chain(self.root' "$$module"; \
	grep -Fq 'matches_leaf_evidence' "$$module"; \
	grep -Fq 'expected_source: &RetainedBootNamespaceExpectedSource' "$$module"; \
	grep -Fq 'expected_source: &RetainedBootNamespaceExpectedSource' "$$leaf"; \
	grep -Fq 'std::slice::from_ref(expected_source)' "$$leaf"; \
	if rg -n '#\[derive\([^]]*Clone[^]]*\)\][[:space:]]*pub\(crate\) struct RetainedBootPublicationParent' "$$module"; then exit 1; else status="$$?"; test "$$status" = 1; fi; \
	if rg -n 'pub\(crate\) fn (descriptor|file|fd)\(|pub\(crate\) fn [^(]*\([^)]*\b(File|RawFd|BorrowedFd|OwnedFd)\b' "$$module"; then exit 1; else status="$$?"; test "$$status" = 1; fi; \
	if rg -n '\x1b' "$$module" "$$existing" "$$effect" "$$leaf" "$$tests" "$(DESCRIPTOR_BOOT_PUBLICATION_PARENT_TOP_DIR)/misc/make/linux-descriptor-boot-publication-parent-tests.mk"; then exit 1; else status="$$?"; test "$$status" = 1; fi; \
	if rg -n 'create_dir|create_dir_all|canonicalize\(|std::process|process::Command|Command::new|nix::mount|libc::mount|umount|setns|unshare|chroot|pivot_root|/dev/|/(boot|efi|esp)(/|`)' "$$module" "$$effect"; then exit 1; else status="$$?"; test "$$status" = 1; fi; \
	for file in "$$module" "$$existing" "$$effect" "$$leaf" "$$tests" "$(DESCRIPTOR_BOOT_PUBLICATION_PARENT_TOP_DIR)/misc/make/linux-descriptor-boot-publication-parent-tests.mk"; do \
		test "$$( wc -l < "$$file" )" -le 1000; \
	done; \
	$(CARGO) test --manifest-path "$(DESCRIPTOR_BOOT_PUBLICATION_PARENT_TOP_DIR)/Cargo.toml" -p forge --lib "$$prefix" -- --test-threads=1
