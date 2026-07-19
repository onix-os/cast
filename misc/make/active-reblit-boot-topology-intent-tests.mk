.PHONY: forge-active-reblit-boot-topology-intent-test

forge-active-reblit-boot-topology-intent-test: host-storage-safety-test
	@set -euo pipefail; \
	timeout 10s mkdir -p "$(TOP_DIR)/target"; \
	listed="$$( timeout 10s mktemp "$(TOP_DIR)/target/active-reblit-boot-topology-intent-list.XXXXXXXXXXXX" )"; \
	trap 'timeout 10s rm -f "$$listed"' EXIT; \
	timeout 300s $(CARGO) test -p forge --lib -- --list | timeout 30s tee "$$listed" >/dev/null; \
	timeout 10s grep -q . "$$listed"; \
	prefix='client::active_reblit_boot_topology_intent::tests::'; \
	timeout 10s test "$$( timeout 10s grep -Ec "^$$prefix.*: test$$" "$$listed" )" = 25; \
	for name in \
		evaluation::alias_intent_exposes_only_revalidated_typed_identity_and_exact_provenance \
		evaluation::distinct_xbootldr_is_typed_declarative_intent_not_runtime_role_proof \
		evaluation::canonical_partuuid_policy_rejects_uppercase_malformed_and_nil_values \
		evaluation::invalid_partuuid_diagnostics_cap_the_preview_at_an_exact_byte_boundary \
		evaluation::lexical_mount_selector_grammar_accepts_valid_and_rejects_unsafe_forms \
		evaluation::distinct_form_rejects_duplicate_partition_identities \
		evaluation::distinct_form_rejects_duplicate_mount_selectors_independently_of_partuuid \
		evaluation::relative_host_and_unknown_embedded_imports_are_all_rejected \
		evaluation::v1_module_is_rejected_without_a_compatibility_fallback \
		evaluation::api_import_is_mandatory_and_unknown_output_fields_are_rejected \
		evaluation::exact_source_and_embedded_abi_participate_in_deterministic_fingerprint \
		evaluation::checked_documentation_examples_use_the_exact_restricted_topology_loader \
		filesystem_security::missing_machine_local_intent_is_a_hard_error_without_fallback \
		filesystem_security::symlink_fifo_socket_and_hardlink_sources_are_rejected_without_blocking \
		filesystem_security::unsafe_source_and_ancestor_modes_fail_closed \
		filesystem_security::source_and_ancestor_acls_or_xattrs_are_rejected_when_supported \
		filesystem_security::invalid_intent_and_all_structural_failures_have_zero_mutation \
		filesystem_security::descriptor_reads_preserve_source_and_directory_atime \
		races_and_bounds::same_byte_source_replacement_before_final_preparation_revalidation_is_rejected \
		races_and_bounds::terminal_rebind_rejects_replacement_after_a_successful_evaluation \
		races_and_bounds::second_complete_pass_rejects_same_inode_content_change_between_passes \
		races_and_bounds::directory_chain_and_installation_root_substitution_are_rejected \
		races_and_bounds::source_byte_bound_is_inclusive_and_production_ceiling_fails_before_evaluation \
		races_and_bounds::mount_selector_byte_component_byte_and_component_count_bounds_are_exact \
		races_and_bounds::work_and_elapsed_time_bounds_are_exact_and_fail_closed; do \
		timeout 10s grep -Fqx "$$prefix$$name: test" "$$listed"; \
	done; \
	for shared in \
		linux_fs::tests::interrupted_retry_limit_accepts_n_and_rejects_n_plus_one \
		linux_fs::tests::xattrs::no_xattr_probe_bounds_interrupted_retries_and_obeys_its_deadline; do \
		timeout 10s grep -Fqx "$$shared: test" "$$listed"; \
	done; \
	module=crates/forge/src/client/boot/active_reblit_boot_topology_intent.rs; \
	filesystem=crates/forge/src/client/boot/active_reblit_boot_topology_intent/filesystem.rs; \
	gluon=crates/forge/src/client/boot/active_reblit_boot_topology_intent/gluon.rs; \
	abi=crates/forge/gluon/boot_topology.glu; \
	timeout 10s grep -Fq 'PreparedActiveReblitBootTopologyIntent' "$$module"; \
	timeout 10s grep -Fq 'RevalidatedActiveReblitBootTopologyIntent' "$$module"; \
	timeout 10s grep -Fq 'revalidate_root_directory_until(budget.deadline)' "$$module"; \
	timeout 10s grep -Fq 'BOOT_TOPOLOGY_ABI_NAME: &str = "cast.boot_topology.v2"' "$$gluon"; \
	timeout 10s grep -Fq 'BOOT_TOPOLOGY_ABI_VERSION: u32 = 2' "$$gluon"; \
	timeout 10s grep -Fq 'abi_version = 2' "$$abi"; \
	timeout 10s grep -Fq 'type PartitionSelector = {' "$$abi"; \
	timeout 10s grep -Fq 'mount_point: String' "$$abi"; \
	timeout 10s grep -Fq 'MAX_MOUNT_POINT_BYTES: usize = 4_095' "$$gluon"; \
	timeout 10s grep -Fq 'MAX_MOUNT_POINT_COMPONENTS: usize = 128' "$$gluon"; \
	timeout 10s grep -Fq 'MAX_MOUNT_POINT_COMPONENT_BYTES: usize = 255' "$$gluon"; \
	timeout 10s grep -Fq 'BoundActiveReblitBootPartitionSelector' "$$module"; \
	timeout 10s grep -Fq 'mount_point_hint' "$$module"; \
	timeout 10s grep -Fq 'limits.max_imports = 1' "$$gluon"; \
	timeout 10s grep -Fq 'limits.max_explicit_input_bytes = 0' "$$gluon"; \
	timeout 10s grep -Fq 'controlled_resolution()' "$$filesystem"; \
	timeout 10s grep -Fq 'descriptor_mount_id' "$$filesystem"; \
	if timeout 10s rg -n 'cast\.boot_topology\.v1|BOOT_TOPOLOGY_ABI_VERSION: u32 = 1|esp_partuuid|xbootldr_partuuid' "$$module" "$$gluon" "$$abi"; then exit 1; else status="$$?"; timeout 10s test "$$status" = 1; fi; \
	if timeout 10s rg -q 'blsforme|SourceRoot|(^|[^[:alnum:]_])config::|system_model|read_dir\(|\.exists\(|canonicalize\(|create_dir|create_dir_all|mount\(|umount\(|std::fs::write|fs::write' "$$module" "$$filesystem" "$$gluon" "$$abi"; then exit 1; else status="$$?"; timeout 10s test "$$status" = 1; fi; \
	tests=crates/forge/src/client/boot/active_reblit_boot_topology_intent_tests; \
	if timeout 10s rg -n 'File::open\("/(?:proc|sys|dev)|read_to_string\("/(?:proc|sys|dev)|std::process|process::Command|Command::new|nix::mount|libc::mount|/dev/disk|/dev/(?:sd|hd|vd|xvd|nvme|mmcblk|loop|md|dm-|nbd|zram)|"/(?:boot|efi|esp)(?:/|")' "$$tests.rs" "$$tests"; then exit 1; else status="$$?"; timeout 10s test "$$status" = 1; fi; \
	timeout 10s grep -Fq 'pub(super) const ESP_MOUNT_POINT: &str = "/synthetic/esp-root";' "$$tests/support.rs"; \
	timeout 10s grep -Fq 'pub(super) const XBOOTLDR_MOUNT_POINT: &str = "/synthetic/boot-root";' "$$tests/support.rs"; \
	for file in \
		"$$module" "$$filesystem" "$$gluon" "$$abi" \
		crates/forge/src/client/boot/active_reblit_boot_topology_intent_tests.rs \
		crates/forge/src/client/boot/active_reblit_boot_topology_intent_tests/*.rs \
		docs/examples/gluon/boot-topology-aliases-esp.glu \
		docs/examples/gluon/boot-topology-distinct-xbootldr.glu \
		misc/make/active-reblit-boot-topology-intent-tests.mk; do \
		timeout 10s test "$$( timeout 10s wc -l < "$$file" )" -le 1000; \
	done; \
	timeout 1200s $(CARGO) test -p forge --lib "$$prefix" -- --test-threads=1; \
	timeout 300s $(CARGO) test -p forge --lib linux_fs::tests::interrupted_retry_limit_accepts_n_and_rejects_n_plus_one -- --exact --test-threads=1; \
	timeout 300s $(CARGO) test -p forge --lib linux_fs::tests::xattrs::no_xattr_probe_bounds_interrupted_retries_and_obeys_its_deadline -- --exact --test-threads=1
