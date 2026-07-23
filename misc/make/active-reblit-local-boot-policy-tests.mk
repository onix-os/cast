.PHONY: forge-active-reblit-local-boot-policy-test

forge-active-reblit-local-boot-policy-test:
	@set -euo pipefail; \
	timeout 10s mkdir -p "$(TOP_DIR)/target"; \
	listed="$$( timeout 10s mktemp "$(TOP_DIR)/target/active-reblit-local-boot-policy-list.XXXXXXXXXXXX" )"; \
	trap 'timeout 10s rm -f "$$listed"' EXIT; \
	timeout 300s $(CARGO) test -p forge --lib -- --list | timeout 300s tee "$$listed" >/dev/null; \
	timeout 10s grep -q . "$$listed"; \
	prefix='client::active_reblit_local_boot_policy::tests::'; \
	timeout 10s test "$$( timeout 10s grep -Ec "^$$prefix.*: test$$" "$$listed" )" = 18; \
	for name in \
		absent_local_policy_is_retained_and_revalidated_without_entries \
		caller_owned_deadline_is_not_replaced_during_prepare_or_revalidation \
		exact_cmdline_files_and_dev_null_masks_are_sorted_and_normalized \
		non_cmdline_entries_are_inventoried_but_not_interpreted \
		a_non_dev_null_cmdline_symlink_is_a_hard_failure \
		hardlinked_or_group_writable_cmdline_files_are_rejected \
		control_bytes_and_oversized_cmdline_files_fail_closed \
		a_regular_file_change_between_capture_and_final_revalidation_is_rejected \
		an_absent_component_appearing_before_final_revalidation_is_rejected \
		retained_mask_reads_ignore_public_name_substitution_and_bind_exact_length \
		two_complete_passes_reject_an_earlier_entry_mutated_between_them \
		every_pass_rebinds_present_and_absent_public_locations \
		intermediate_ancestor_mode_acls_and_xattrs_are_revalidated \
		directory_entry_work_and_elapsed_time_bounds_are_inclusive \
		raw_syscall_interruption_ceiling_accepts_n_and_rejects_n_plus_one \
		descriptor_reads_do_not_change_regular_file_or_directory_atime \
		normalization_matches_the_accepted_upstream_subset_and_rejects_controls \
		relevant_names_must_be_canonical_ascii; do \
		timeout 10s grep -Fqx "$$prefix$$name: test" "$$listed"; \
	done; \
	module=crates/forge/src/client/boot/active_reblit_local_boot_policy.rs; \
	filesystem=crates/forge/src/client/boot/active_reblit_local_boot_policy/filesystem.rs; \
	tests=crates/forge/src/client/boot/active_reblit_local_boot_policy_tests.rs; \
	timeout 10s grep -Fq 'PreparedActiveReblitLocalBootPolicy' "$$module"; \
	timeout 10s grep -Fq 'fn prepare_until(' "$$module"; \
	timeout 10s grep -Fq 'fn revalidate_until' "$$module"; \
	timeout 10s grep -Fq 'RetainedLocalPolicyLocation' "$$filesystem"; \
	timeout 10s grep -Fq 'revalidate_root_directory_until(budget.deadline)' "$$module"; \
	timeout 10s grep -Fq 'readlinkat(' "$$filesystem"; \
	timeout 10s grep -Fq 'retained.as_raw_fd()' "$$filesystem"; \
	if timeout 10s rg -q 'blsforme::|use .*blsforme|read_dir\(|\.exists\(' "$$module" "$$filesystem"; then exit 1; else status="$$?"; timeout 10s test "$$status" = 1; fi; \
	for file in "$$module" "$$filesystem" "$$tests" misc/make/active-reblit-local-boot-policy-tests.mk; do \
		timeout 10s test "$$( timeout 10s wc -l < "$$file" )" -le 1000; \
	done; \
	timeout 900s $(CARGO) test -p forge --lib "$$prefix" -- --test-threads=1
