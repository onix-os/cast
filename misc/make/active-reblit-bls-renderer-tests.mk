SHELL := /bin/bash

BLS_RENDERER_TOP_DIR := $(abspath $(dir $(lastword $(MAKEFILE_LIST)))/../..)
CARGO ?= cargo

.PHONY: forge-active-reblit-bls-renderer-test

forge-active-reblit-bls-renderer-test: host-storage-safety-test
	@set -euo pipefail; \
	timeout 10s mkdir -p "$(BLS_RENDERER_TOP_DIR)/target"; \
	listed="$$( timeout 10s mktemp "$(BLS_RENDERER_TOP_DIR)/target/active-reblit-bls-renderer-list.XXXXXXXXXXXX" )"; \
	trap 'timeout 10s rm -f "$$listed"' EXIT; \
	timeout 300s $(CARGO) test --manifest-path "$(BLS_RENDERER_TOP_DIR)/Cargo.toml" -p forge --lib -- --list | timeout 300s tee "$$listed" >/dev/null; \
	timeout 10s grep -q . "$$listed"; \
	prefix='client::active_reblit_bls_renderer::tests::'; \
	timeout 10s test "$$( timeout 10s grep -Ec "^$$prefix.*: test$$" "$$listed" )" = 19; \
	for name in \
		schemas_and_identity::historical_local_schema_is_used_and_unavailable_history_uses_sticky_global_fallback \
		schemas_and_identity::former_identities_emit_no_outputs_and_do_not_change_rendered_bytes \
		payloads_and_collisions::initrd_basenames_are_preserved_and_sorted_ascii_case_insensitively \
		payloads_and_collisions::identical_payload_bytes_and_leaf_reuse_one_path_across_versions_and_bindings \
		payloads_and_collisions::same_namespace_version_and_leaf_with_different_bytes_use_distinct_paths \
		payloads_and_collisions::case_insensitive_payload_alias_is_rejected_even_when_content_matches \
		payloads_and_collisions::same_xxh3_and_length_with_different_sha256_is_rejected_before_deduplication \
		ownership_and_effects::aliased_and_distinct_topologies_preserve_rendered_bytes_but_change_collision_domains \
		ownership_and_effects::bound_plan_retains_exact_inputs_topology_and_sources_without_namespace_mutation \
		golden_documents::golden_alias_plan_matches_pinned_loader_entry_payload_and_bootloader_bytes \
		golden_documents::zero_initrd_entry_retains_blank_line_and_final_newline \
		golden_documents::entry_payload_and_loader_paths_match_exact_bls_shapes \
		bounds_and_deadlines::fat_unsafe_version_initrd_and_entry_components_fail_closed \
		bounds_and_deadlines::checksum_identity_has_fixed_lowercase_widths_and_no_version_or_state_component \
		bounds_and_deadlines::initrd_leaf_fat_limit_admits_255_bytes_and_rejects_256 \
		bounds_and_deadlines::generated_file_and_total_byte_bounds_admit_n_and_reject_n_plus_one_before_materialization \
		bounds_and_deadlines::request_path_initrd_and_work_bounds_admit_n_and_reject_n_plus_one \
		bounds_and_deadlines::mismatched_input_and_topology_deadlines_fail_before_publication_planning \
		bounds_and_deadlines::expired_and_injected_post_sort_post_plan_and_terminal_deadlines_fail_closed; do \
		timeout 10s grep -Fqx "$$prefix$$name: test" "$$listed"; \
	done; \
	root="$(BLS_RENDERER_TOP_DIR)/crates/forge/src/client/boot/active_reblit_bls_renderer.rs"; \
	core="$(BLS_RENDERER_TOP_DIR)/crates/forge/src/client/boot/active_reblit_bls_renderer"; \
	content_identity="$(BLS_RENDERER_TOP_DIR)/crates/forge/src/client/boot/boot_content_identity.rs"; \
	tests="$(BLS_RENDERER_TOP_DIR)/crates/forge/src/client/boot/active_reblit_bls_renderer_tests.rs"; \
	test_core="$(BLS_RENDERER_TOP_DIR)/crates/forge/src/client/boot/active_reblit_bls_renderer_tests"; \
	timeout 10s grep -Fq "struct RenderedActiveReblitBlsRequests<'input, 'attempt, 'stone, 'roots>" "$$root"; \
	timeout 10s grep -Fq "inputs: &'input RevalidatedActiveReblitBootRenderInputs<'attempt, 'stone, 'roots>" "$$root"; \
	timeout 10s grep -Fq "topology: &'topology_view RevalidatedActiveReblitMountedBootTopology<'topology_authority>" "$$root"; \
	timeout 10s grep -Fq 'render_with_policy_until(inputs, BLS_POLICY, inputs.deadline(), Instant::now)' "$$root"; \
	timeout 10s grep -Fq 'require_matching_deadlines(self.deadline, topology_deadline)?;' "$$root"; \
	timeout 10s grep -Fq 'PreparedActiveReblitBootPublicationPlan::prepare_until(self.requests, topology.topology(), self.deadline)?;' "$$root"; \
	timeout 10s grep -Fq 'budget.admit_requests(candidate_requests)?;' "$$root"; \
	timeout 10s grep -Fq 'budget.require_deadline("initrd sort completion")?;' "$$root"; \
	timeout 10s grep -Fq 'require_deadline(deadline, "terminal rendered BLS requests", terminal_now())?;' "$$root"; \
	timeout 10s grep -Fq 'require_deadline(self.deadline, "terminal bound publication plan", Instant::now())?;' "$$root"; \
	timeout 10s grep -Fq 'write!(&mut token, "xxh3-{digest:032x}-l{length:016x}")' "$$core/paths.rs"; \
	timeout 10s grep -Fq 'build_relative_path(&["EFI", namespace, &token, leaf], budget)' "$$core/paths.rs"; \
	timeout 10s grep -Fq 'let kernel_digest = kernel.kernel_digest();' "$$root"; \
	timeout 10s grep -Fq 'let kernel_length = kernel.kernel_length();' "$$root"; \
	timeout 10s grep -Fq 'digest: kernel_digest,' "$$root"; \
	timeout 10s grep -Fq 'length: kernel_length,' "$$root"; \
	timeout 10s test "$$( timeout 10s grep -Fc 'paths::payload_path(' "$$root" )" = 2; \
	timeout 10s rg -U --pcre2 -q 'paths::payload_path\(\s*schema\.namespace\(\),\s*kernel_digest,\s*kernel_length,\s*"vmlinuz",' "$$root"; \
	timeout 10s rg -U --pcre2 -q 'paths::payload_path\(\s*schema\.namespace\(\),\s*digest,\s*length,\s*basename,' "$$root"; \
	timeout 10s grep -Fq '.into_publication_plan(&topology_view)' "$$test_core/ownership_and_effects.rs"; \
	timeout 10s grep -Fq 'sealed_sources: SealedSourceCatalog<' "$$root"; \
	timeout 10s grep -Fq '.contains_publication_source(self.planned.source())' "$$root"; \
	timeout 10s grep -Fq '.asset_for_publication_source(source)' "$$root"; \
	timeout 10s grep -Fq 'pub(in crate::client) fn expected_content_identity(&self) -> BootContentIdentity' "$$root"; \
	timeout 10s grep -Fq 'content_identity: kernel_content_identity,' "$$root"; \
	timeout 10s grep -Fq 'content_identity: initrd.content_identity,' "$$root"; \
	timeout 10s grep -Fq '|| previous.content_identity != candidate.content_identity' "$$core/payload_catalog.rs"; \
	timeout 10s grep -Fq '(u16, u128, u64, BootContentIdentity)' "$$core/payload_catalog.rs"; \
	timeout 10s grep -Fq 'asset_for_publication_source(&mismatched_source)' "$$test_core/ownership_and_effects.rs"; \
	if timeout 10s rg -U --pcre2 -n '#\[derive\((?s:[^]]*Clone[^]]*)\)\]\s*(?:pub(?:\([^)]*\))?\s+)?struct\s+(?:(?:Rendered|Bound)ActiveReblitBls[A-Za-z0-9_]*)|impl(?:<(?s:[^;{}]*)>)?\s+Clone\s+for\s+(?:(?:Rendered|Bound)ActiveReblitBls[A-Za-z0-9_]*)|pub\(in crate::client\)\s+fn\s+(?:into_inner|into_plan|plan|detach|into_unbound)\b|pub\(in crate::client\)\s+fn\s+[A-Za-z0-9_]+(?s:[^{;]*)PreparedActiveReblitBootPublicationPlan' "$$root" "$$core"; then exit 1; else status="$$?"; timeout 10s test "$$status" = 1; fi; \
	if timeout 10s rg -n 'blsforme|std::fs|fs_err|OpenOptions|File::(?:open|create)|std::process|process::Command|Command::new|nix::mount|libc::mount|mount_partitions|canonicalize\(|create_dir|create_dir_all|rename\(|remove_file|remove_dir|(?:fs::|File::)write\(|\bdescriptor\s*\(|FileExt|(?:std::io::|io::)?Read\b|\.read(?:_exact|_to_end|_at|_exact_at)?\s*\(|\bpread(?:64)?\b|\bread_at\b|\bread_exact_at\b|\b(?:AsFd|BorrowedFd|OwnedFd|RawFd)\b|\bBLK[A-Z_]+\b|/dev/(disk|sd|hd|vd|xvd|nvme|mmcblk|loop|md|dm-|nbd|zram)' "$$root" "$$core"; then exit 1; else status="$$?"; timeout 10s test "$$status" = 1; fi; \
	host_root_pattern='/''(boot|efi|esp)(/|["[:space:]]|$$)'; \
	if timeout 10s rg -n "$$host_root_pattern" "$$root" "$$core"; then exit 1; else status="$$?"; timeout 10s test "$$status" = 1; fi; \
	if timeout 10s rg -U --pcre2 -n 'build_relative_path\(&\["EFI", namespace, version|pub\(super\) fn payload_path\((?s:[^)]*)\bversion\b' "$$root" "$$core"; then exit 1; else status="$$?"; timeout 10s test "$$status" = 1; fi; \
	for file in "$$root" "$$core"/*.rs "$$content_identity" "$$tests" "$$test_core"/*.rs "$(BLS_RENDERER_TOP_DIR)/misc/make/active-reblit-bls-renderer-tests.mk"; do \
		timeout 10s test "$$( timeout 10s wc -l < "$$file" )" -le 1000; \
	done; \
	timeout 1200s $(CARGO) test --manifest-path "$(BLS_RENDERER_TOP_DIR)/Cargo.toml" -p forge --lib "$$prefix" -- --test-threads=1
