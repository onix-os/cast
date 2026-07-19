.PHONY: forge-active-reblit-root-filesystem-intent-test

forge-active-reblit-root-filesystem-intent-test: host-storage-safety-test
	@set -euo pipefail; \
	timeout 10s mkdir -p "$(TOP_DIR)/target"; \
	listed="$$( timeout 10s mktemp "$(TOP_DIR)/target/active-reblit-root-filesystem-intent-list.XXXXXXXXXXXX" )"; \
	trap 'timeout 10s rm -f "$$listed"' EXIT; \
	timeout 300s $(CARGO) test -p forge --lib -- --list | timeout 300s tee "$$listed" >/dev/null; \
	timeout 10s grep -q . "$$listed"; \
	prefix='client::active_reblit_root_filesystem_intent::tests::'; \
	timeout 10s test "$$( timeout 10s grep -Ec "^$$prefix.*: test$$" "$$listed" )" = 23; \
	for name in \
		evaluation::authored_intent_exposes_one_revalidated_root_token_and_exact_provenance \
		evaluation::root_locator_is_an_opaque_authored_scalar_not_device_or_filesystem_proof \
		evaluation::empty_whitespace_non_ascii_quoted_escaped_and_prefixed_values_are_rejected \
		evaluation::root_locator_byte_bound_is_inclusive_and_diagnostic_preview_is_bounded \
		evaluation::relative_host_unknown_and_old_abi_imports_are_rejected \
		evaluation::v1_api_import_is_mandatory_and_the_output_record_is_closed \
		evaluation::exact_source_and_embedded_abi_participate_in_a_deterministic_fingerprint \
		evaluation::checked_documentation_example_uses_the_exact_restricted_loader \
		filesystem_security::missing_machine_local_intent_is_a_hard_error_without_fallback \
		filesystem_security::symlink_fifo_socket_and_hardlink_sources_are_rejected_without_blocking \
		filesystem_security::unsafe_source_and_ancestor_modes_fail_closed \
		filesystem_security::source_and_ancestor_acls_or_xattrs_are_rejected_when_supported \
		filesystem_security::invalid_intent_and_structural_failures_have_zero_mutation \
		filesystem_security::descriptor_reads_preserve_source_and_directory_atime \
		races_and_bounds::same_byte_source_replacement_before_final_preparation_revalidation_is_rejected \
		races_and_bounds::terminal_rebind_rejects_replacement_after_a_successful_evaluation \
		races_and_bounds::second_complete_pass_rejects_same_inode_content_change_between_passes \
		races_and_bounds::directory_chain_and_installation_root_substitution_are_rejected \
		races_and_bounds::source_byte_bound_is_inclusive_and_production_ceiling_fails_before_evaluation \
		races_and_bounds::work_and_default_elapsed_time_bounds_are_exact_and_fail_closed \
		races_and_bounds::caller_owned_deadline_is_rejected_at_prepare_and_revalidate_entry \
		races_and_bounds::caller_owned_deadline_is_rechecked_at_prepare_and_revalidate_completion \
		races_and_bounds::normalized_materialization_rechecks_deadline_after_owned_tokens_exist; do \
		timeout 10s grep -Fqx "$$prefix$$name: test" "$$listed"; \
	done; \
	module=crates/forge/src/client/boot/active_reblit_root_filesystem_intent.rs; \
	core=crates/forge/src/client/boot/active_reblit_root_filesystem_intent; \
	filesystem="$$core/filesystem.rs"; \
	gluon="$$core/gluon.rs"; \
	normalization="$$core/normalization.rs"; \
	abi=crates/forge/gluon/root_filesystem.glu; \
	timeout 10s grep -Fq 'etc/cast/root-filesystem.glu' "$$module"; \
	timeout 10s grep -Fq 'PreparedActiveReblitRootFilesystemIntent' "$$module"; \
	timeout 10s grep -Fq 'RevalidatedActiveReblitRootFilesystemIntent' "$$module"; \
	timeout 10s grep -Fq 'PhantomData<Rc<()>>' "$$module"; \
	timeout 10s grep -Fq 'Self::prepare_until(installation, deadline)' "$$module"; \
	timeout 10s grep -Fq 'self.revalidate_until(installation, deadline)' "$$module"; \
	timeout 10s test "$$( timeout 10s grep -Fc '#[cfg(test)]' "$$module" )" -ge 5; \
	timeout 10s grep -Fq 'remaining_at_admission: Duration' "$$module"; \
	timeout 10s grep -Fq 'revalidate_root_directory_until(budget.deadline)' "$$module"; \
	timeout 10s grep -Fq 'ROOT_FILESYSTEM_ABI_NAME: &str = "cast.root_filesystem.v1"' "$$gluon"; \
	timeout 10s grep -Fq 'ROOT_FILESYSTEM_ABI_VERSION: u32 = 1' "$$gluon"; \
	timeout 10s grep -Fq 'abi_version = 1' "$$abi"; \
	timeout 10s grep -Fq 'type RootFilesystemIntent = {' "$$abi"; \
	timeout 10s grep -Fq 'root: String' "$$abi"; \
	timeout 10s test "$$( timeout 10s grep -Ec '^[[:space:]]+[a-z_]+: String,' "$$abi" )" = 1; \
	timeout 10s grep -Fq 'limits.max_imports = 1' "$$gluon"; \
	timeout 10s grep -Fq 'limits.max_explicit_input_bytes = 0' "$$gluon"; \
	timeout 10s grep -Fq 'argument.push_str("root=")' "$$normalization"; \
	timeout 10s grep -Fq 'controlled_resolution()' "$$filesystem"; \
	timeout 10s grep -Fq 'descriptor_mount_id_until(descriptor, budget.deadline)' "$$filesystem"; \
	timeout 10s grep -Fq 'read_to_end_bounded_until(' "$$filesystem"; \
	timeout 10s grep -Fq 'O_NOATIME' "$$filesystem"; \
	if timeout 10s rg -q 'impl Clone for PreparedActiveReblitRootFilesystemIntent|impl Copy for PreparedActiveReblitRootFilesystemIntent' "$$module"; then exit 1; else status="$$?"; timeout 10s test "$$status" = 1; fi; \
	if timeout 10s rg -q 'active_reblit_(publication_plan|boot_topology_intent|mounted_boot_topology|local_boot_policy|package_cmdline_inputs)|system_model|/proc/cmdline|/etc/fstab|blkid|udev|blsforme|SourceRoot|read_dir\(|\.exists\(|canonicalize\(|create_dir|create_dir_all|mount\(|umount\(|std::fs::write|fs::write|std::process|process::Command|Command::new' "$$module" "$$core" "$$abi"; then exit 1; else status="$$?"; timeout 10s test "$$status" = 1; fi; \
	tests=crates/forge/src/client/boot/active_reblit_root_filesystem_intent_tests; \
	if timeout 10s rg -n 'File::open\("/(?:proc|sys|dev)|read_to_string\("/(?:proc|sys|dev)|std::process|process::Command|Command::new|nix::mount|libc::mount|/dev/disk|/dev/(?:sd|hd|vd|xvd|nvme|mmcblk|loop|md|dm-|nbd|zram)|"/(?:boot|efi|esp)(?:/|")' "$$tests.rs" "$$tests"; then exit 1; else status="$$?"; timeout 10s test "$$status" = 1; fi; \
	for file in \
		"$$module" "$$core"/*.rs "$$abi" \
		crates/forge/src/client/boot/active_reblit_root_filesystem_intent_tests.rs \
		crates/forge/src/client/boot/active_reblit_root_filesystem_intent_tests/*.rs \
		docs/examples/gluon/root-filesystem.glu \
		misc/make/active-reblit-root-filesystem-intent-tests.mk; do \
		timeout 10s test "$$( timeout 10s wc -l < "$$file" )" -le 1000; \
	done; \
	timeout 1200s $(CARGO) test -p forge --lib "$$prefix" -- --test-threads=1
