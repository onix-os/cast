DESCRIPTOR_BOOT_FILE_PUBLICATION_TOP_DIR := $(abspath $(dir $(lastword $(MAKEFILE_LIST)))/../..)
CARGO ?= cargo

.PHONY: forge-linux-descriptor-boot-file-publication-test

forge-linux-descriptor-boot-file-publication-test: host-storage-safety-test forge-linux-descriptor-boot-namespace-production-test forge-active-reblit-boot-publication-plan-test
	@set -euo pipefail; \
	mkdir -p "$(DESCRIPTOR_BOOT_FILE_PUBLICATION_TOP_DIR)/target"; \
	listed="$$( mktemp "$(DESCRIPTOR_BOOT_FILE_PUBLICATION_TOP_DIR)/target/linux-descriptor-boot-file-publication-test-list.XXXXXXXXXXXX" )"; \
	trap 'rm -f "$$listed"' EXIT; \
	$(CARGO) test --manifest-path "$(DESCRIPTOR_BOOT_FILE_PUBLICATION_TOP_DIR)/Cargo.toml" -p forge --lib -- --list | tee "$$listed" >/dev/null; \
	grep -q . "$$listed"; \
	prefix='linux_fs::tests::descriptor_boot_file_publication::'; \
	test "$$( grep -Ec "^$$prefix.*: test$$" "$$listed" )" = 15; \
	for name in \
		canonical_request_cannot_alias_private_stage_namespace_case_insensitively \
		stop_after_exclusive_creation_preserves_empty_mode_0644_residue_and_retry_refuses \
		stop_mid_multichunk_write_preserves_partial_mode_0644_residue_and_retry_refuses \
		stop_after_final_write_preserves_exact_mode_0644_residue_then_same_inode_resume \
		generated_source_publishes_once_and_exact_destination_is_idempotent \
		sealed_source_streams_multiple_chunks_without_exposing_its_descriptor \
		different_canonical_destination_is_preserved_and_refused \
		exact_private_residue_is_resumed_without_replacement_or_deletion \
		different_and_foreign_private_residue_are_preserved_and_refused \
		exact_bytes_with_wrong_effective_mode_are_not_adopted \
		source_sha256_mismatch_fails_before_private_publication \
		error_reported_after_single_move_is_reconciled_as_published \
		durability_suffix_failures_leave_an_exact_idempotent_canonical_leaf \
		retained_attachment_replacement_fails_before_mutating_either_directory \
		same_credential_private_name_substitution_fails_without_validated_evidence; do \
		grep -Fqx "$$prefix$$name: test" "$$listed"; \
	done; \
	module="$(DESCRIPTOR_BOOT_FILE_PUBLICATION_TOP_DIR)/crates/forge/src/linux_fs/mount_namespace/attachment/boot_file_publication.rs"; \
	module_dir="$(DESCRIPTOR_BOOT_FILE_PUBLICATION_TOP_DIR)/crates/forge/src/linux_fs/mount_namespace/attachment/boot_file_publication"; \
	bridge="$(DESCRIPTOR_BOOT_FILE_PUBLICATION_TOP_DIR)/crates/forge/src/linux_fs/descriptor_boot_namespace/production/retained/publication_source.rs"; \
	reserved_names="$(DESCRIPTOR_BOOT_FILE_PUBLICATION_TOP_DIR)/crates/forge/src/linux_fs/boot_file_publication_name.rs"; \
	plan_policy="$(DESCRIPTOR_BOOT_FILE_PUBLICATION_TOP_DIR)/crates/forge/src/client/boot/active_reblit_publication_plan/path_policy.rs"; \
	tests="$(DESCRIPTOR_BOOT_FILE_PUBLICATION_TOP_DIR)/crates/forge/src/linux_fs/tests/descriptor_boot_file_publication.rs"; \
	grep -Fq 'impl RevalidatedTaskRootedAttachment' "$$module"; \
	grep -Fq 'pub(crate) fn publish_immutable_boot_file_until' "$$module"; \
	test "$$( grep -Fc 'renameat2_noreplace_once(parent, private_name, parent, canonical_name)' "$$module" )" = 1; \
	grep -Fq 'BoundRetainedBootFileSource::bind_until' "$$module"; \
	grep -Fq 'BootNamespaceDestinationState::Absent' "$$module"; \
	grep -Fq 'BootNamespaceDestinationState::Exact' "$$module"; \
	grep -Fq 'BootNamespaceDestinationState::Different' "$$module"; \
	grep -Fq 'synchronizing the exact private boot-file leaf' "$$module"; \
	grep -Fq 'canonical' "$$module"; \
	grep -Fq 'parent' "$$module"; \
	grep -Fq 'sync_filesystem_until(parent, deadline)' "$$module"; \
	grep -Fq 'metadata.permissions().mode() & 0o7777 != 0o644' "$$module_dir/destination.rs"; \
	grep -Fq 'sha256.update(&buffer[..found]);' "$$module_dir/effect.rs"; \
	grep -Fq 'xxh3.update(&buffer[..found]);' "$$module_dir/effect.rs"; \
	grep -Fq 'terminally_revalidate_expected_streams' "$$bridge"; \
	grep -Fq 'non-crash-recoverable foundation' "$$module"; \
	grep -Fq 'mode-0644 deterministic residue' "$$module"; \
	grep -Fq 'no standalone reboot' "$$module"; \
	grep -Fq 'later current-plan' "$$module"; \
	grep -Fq 'VFAT mounted with `fmask=0133` already expose 0644' "$$module"; \
	grep -Fq 'Streaming never changes' "$$module"; \
	grep -Fq 'same credentials' "$$module"; \
	grep -Fq 'poisoning it' "$$module"; \
	grep -Fq 'AfterExclusiveCreation' "$$module_dir/effect.rs"; \
	grep -Fq 'MidMultiChunkWrite' "$$module_dir/effect.rs"; \
	grep -Fq 'AfterFinalWriteBeforeSourceValidation' "$$module_dir/effect.rs"; \
	grep -Fq 'before_private_name_rename();' "$$module"; \
	grep -Fq 'arm_retained_boot_file_private_name_substitution' "$$module_dir/effect.rs"; \
	grep -Fq 'ReservedPrivatePublicationLeaf' "$$module_dir/error.rs"; \
	grep -Fq 'is_retained_boot_file_private_component(request.canonical_leaf())' "$$module"; \
	test "$$( grep -Fc 'nix::libc::fchmod(file.as_raw_fd(), 0o644)' "$$module_dir/destination.rs" )" = 1; \
	if rg -n 'fchmod' "$$module_dir/effect.rs"; then exit 1; else status="$$?"; test "$$status" = 1; fi; \
	if rg -n '0o600|0600' "$$module_dir" "$$module"; then exit 1; else status="$$?"; test "$$status" = 1; fi; \
	grep -Fq 'RETAINED_BOOT_FILE_PRIVATE_PREFIX: &str = ".cast-payload-"' "$$reserved_names"; \
	grep -Fq 'eq_ignore_ascii_case(RETAINED_BOOT_FILE_PRIVATE_PREFIX.as_bytes())' "$$reserved_names"; \
	grep -Fq 'ReservedPrivatePublicationComponent' "$$plan_policy"; \
	if rg -n 'remove_(file|dir)\(|create_dir\(|mkdirat\(|RENAME_EXCHANGE|mount_partitions\(' "$$module" "$$module_dir" "$$bridge"; then exit 1; else status="$$?"; test "$$status" = 1; fi; \
	if rg -n 'pub\(crate\) fn (?:publish|create|rename)[[:alnum:]_]*\(' "$$module_dir" "$$bridge"; then exit 1; else status="$$?"; test "$$status" = 1; fi; \
	for file in "$$module" "$$module_dir"/*.rs "$$bridge" "$$reserved_names" "$$plan_policy" "$$tests" "$(DESCRIPTOR_BOOT_FILE_PUBLICATION_TOP_DIR)/misc/make/linux-descriptor-boot-file-publication-tests.mk"; do \
		test "$$( wc -l < "$$file" )" -le 1000; \
	done; \
	timeout 1200s $(CARGO) test --manifest-path "$(DESCRIPTOR_BOOT_FILE_PUBLICATION_TOP_DIR)/Cargo.toml" -p forge --lib "$$prefix" -- --test-threads=1
