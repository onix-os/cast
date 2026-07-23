.PHONY: forge-active-reblit-boot-schema-input-test

forge-active-reblit-boot-schema-input-test:
	@set -euo pipefail; \
	timeout 10s mkdir -p "$(TOP_DIR)/target"; \
	listed="$$( timeout 10s mktemp "$(TOP_DIR)/target/active-reblit-boot-schema-input-list.XXXXXXXXXXXX" )"; \
	trap 'timeout 10s rm -f "$$listed"' EXIT; \
	timeout 300s $(CARGO) test -p forge --lib -- --list | timeout 300s tee "$$listed" >/dev/null; \
	timeout 10s grep -q . "$$listed"; \
	prefix='client::active_reblit_boot_schema_inputs::tests::'; \
	timeout 10s test "$$( timeout 10s grep -Ec "^$$prefix.*: test$$" "$$listed" )" = 28; \
	for name in \
		bounds_and_errors::per_source_byte_policy_admits_n_and_rejects_n_plus_one \
		bounds_and_errors::aggregate_byte_policy_accounts_for_each_authenticated_local_source \
		bounds_and_errors::work_policy_admits_observed_n_and_rejects_n_minus_one \
		bounds_and_errors::unrepresentable_deadline_is_rejected_before_source_work \
		bounds_and_errors::caller_owned_deadline_is_not_replaced_during_prepare_or_revalidation \
		bounds_and_errors::os_release_parser_rejects_duplicate_keys_and_fat_unsafe_ids \
		bounds_and_errors::os_info_parser_rejects_duplicate_or_current_former_identity \
		fallback_semantics::absent_historical_os_release_selects_the_authenticated_head_schema \
		fallback_semantics::malformed_historical_os_release_falls_back_only_as_semantic_invalidity \
		fallback_semantics::malformed_historical_os_info_falls_back_to_the_same_global_schema \
		fallback_semantics::required_head_never_downgrades_structural_or_semantic_failure \
		fallback_semantics::sticky_fallback_is_not_promoted_when_metadata_appears_later \
		fallback_semantics::state_root_exclusion_prevents_a_projected_history_schema_from_rendering \
		metadata_and_races::unsafe_mode_and_extra_hardlink_are_structural_history_failures \
		metadata_and_races::symlinked_generated_metadata_is_never_followed \
		metadata_and_races::arbitrary_xattr_is_rejected_when_the_fixture_filesystem_supports_it \
		metadata_and_races::same_byte_name_replacement_during_read_is_not_admitted \
		metadata_and_races::revalidation_rejects_replacement_after_successful_preparation \
		metadata_and_races::eio_is_operational_and_never_becomes_history_fallback \
		metadata_and_races::lib_replacement_inside_generated_name_walk_is_rejected \
		metadata_and_races::file_replacement_inside_generated_name_walk_is_rejected \
		metadata_and_races::file_replacement_after_generated_revalidation_read_is_rejected \
		source_binding::head_os_info_is_bound_to_its_exact_stone_coordinate \
		source_binding::generated_head_is_bound_beneath_the_revalidated_usr_descriptor \
		source_binding::os_info_preserves_bounded_unique_former_identities \
		source_binding::schema_order_follows_the_stone_requirements_for_eligible_roots \
		source_binding::eligible_roots_from_a_different_projection_order_are_rejected \
		source_binding::revalidation_rejects_foreign_stone_with_the_same_schema_requirements; do \
		timeout 10s grep -Fqx "$$prefix$$name: test" "$$listed"; \
	done; \
	module=crates/forge/src/client/boot/active_reblit_boot_schema_inputs.rs; \
	generated=crates/forge/src/client/boot/active_reblit_boot_schema_inputs/generated_os_release.rs; \
	validation=crates/forge/src/client/boot/active_reblit_boot_schema_inputs/schema_validation.rs; \
	timeout 10s grep -Fq 'PreparedActiveReblitBootSchemas' "$$module"; \
	timeout 10s grep -Fq 'fn prepare_until(' "$$module"; \
	timeout 10s grep -Fq 'fn revalidate_sources_until(' "$$module"; \
	timeout 10s grep -Fq 'RevalidatedActiveReblitBootStateRoots' "$$module"; \
	timeout 10s grep -Fq 'openat2_file_until' "$$generated"; \
	if timeout 10s rg -q 'blsforme|read_dir\(|\.exists\(|std::fs::read' "$$module" "$$generated" "$$validation"; then exit 1; else status="$$?"; timeout 10s test "$$status" = 1; fi; \
	for file in \
		"$$module" "$$generated" "$$validation" \
		crates/forge/src/client/boot/active_reblit_boot_schema_inputs_tests.rs \
		crates/forge/src/client/boot/active_reblit_boot_schema_inputs_tests/*.rs \
		misc/make/active-reblit-boot-schema-input-tests.mk; do \
		timeout 10s test "$$( timeout 10s wc -l < "$$file" )" -le 1000; \
	done; \
	timeout 1200s $(CARGO) test -p forge --lib "$$prefix" -- --test-threads=1
