DESIRED_PUBLICATION_TOP_DIR := $(abspath $(dir $(lastword $(MAKEFILE_LIST)))/../..)
CARGO ?= cargo

.PHONY: forge-active-reblit-desired-publication-test

forge-active-reblit-desired-publication-test: host-storage-safety-test forge-active-reblit-bls-renderer-test forge-active-reblit-boot-publication-plan-test
	@set -euo pipefail; \
	timeout 10s mkdir -p "$(DESIRED_PUBLICATION_TOP_DIR)/target"; \
	listed="$$( timeout 10s mktemp "$(DESIRED_PUBLICATION_TOP_DIR)/target/active-reblit-desired-publication-list.XXXXXXXXXXXX" )"; \
	trap 'timeout 10s rm -f "$$listed"' EXIT; \
	timeout 300s $(CARGO) test --manifest-path "$(DESIRED_PUBLICATION_TOP_DIR)/Cargo.toml" -p forge --lib -- --list | timeout 300s tee "$$listed" >/dev/null; \
	timeout 10s grep -q . "$$listed"; \
	prefix='client::active_reblit_desired_publication::tests::'; \
	timeout 10s test "$$( timeout 10s grep -Ec "^$$prefix.*: test$$" "$$listed" )" = 8; \
	for name in \
		sensitivity::canonical_order_is_deterministic_independent_of_input_order \
		sensitivity::every_canonical_scalar_changes_the_fingerprint \
		sensitivity::canonical_v1_fingerprint_is_pinned \
		bounds_and_deadlines::count_path_canonical_and_work_bounds_admit_n_and_reject_n_minus_one \
		bounds_and_deadlines::caller_deadline_is_checked_at_entry_around_allocations_and_terminal_completion \
		bounds_and_deadlines::output_count_mismatch_fails_closed_without_materializing_an_inventory \
		integration::bound_renderer_plan_projects_to_owned_authority_free_canonical_records \
		integration::sealed_stone_binding_index_is_excluded_from_the_desired_fingerprint; do \
		timeout 10s grep -Fqx "$$prefix$$name: test" "$$listed"; \
	done; \
	root="$(DESIRED_PUBLICATION_TOP_DIR)/crates/forge/src/client/boot/active_reblit_desired_publication.rs"; \
	error="$(DESIRED_PUBLICATION_TOP_DIR)/crates/forge/src/client/boot/active_reblit_desired_publication/error.rs"; \
	tests="$(DESIRED_PUBLICATION_TOP_DIR)/crates/forge/src/client/boot/active_reblit_desired_publication_tests.rs"; \
	test_core="$(DESIRED_PUBLICATION_TOP_DIR)/crates/forge/src/client/boot/active_reblit_desired_publication_tests"; \
	renderer="$(DESIRED_PUBLICATION_TOP_DIR)/crates/forge/src/client/boot/active_reblit_bls_renderer.rs"; \
	timeout 10s grep -Fq 'const DESIRED_PUBLICATION_DOMAIN: &[u8] = b"os-tools/forge/active-reblit-desired-publication/v1\0";' "$$root"; \
	timeout 10s grep -Fq 'pub(in crate::client) fn prepare_desired_publication_inventory(' "$$root"; \
	timeout 10s grep -Fq 'output.mode(),' "$$root"; \
	timeout 10s grep -Fq 'pub(in crate::client) fn mode(&self) -> u32' "$$renderer"; \
	for label in domain destination-layout output-count logical-root publication-phase semantic-role relative-path mode xxh3-checksum exact-length content-sha256; do \
		timeout 10s grep -Fq "b\"$$label\"" "$$root"; \
	done; \
	timeout 10s grep -Fq 'hasher.update(name_length.to_le_bytes());' "$$root"; \
	timeout 10s grep -Fq 'hasher.update(value_length.to_le_bytes());' "$$root"; \
	timeout 10s grep -Fq 'output.checksum.to_le_bytes()' "$$root"; \
	timeout 10s grep -Fq 'output.content_identity.as_bytes()' "$$root"; \
	timeout 10s grep -Fq 'terminal_now())?' "$$root"; \
	timeout 10s grep -Fq '.try_reserve_exact(expected_publications)' "$$root"; \
	timeout 10s grep -Fq '.try_reserve_exact(bytes.len())' "$$root"; \
	timeout 10s grep -Fq 'max_single_path_bytes: MAX_DESIRED_PUBLICATION_SINGLE_PATH_BYTES' "$$root"; \
	timeout 10s grep -Fq 'max_logical_bytes: MAX_DESIRED_PUBLICATION_LOGICAL_BYTES' "$$root"; \
	timeout 10s grep -Fq 'max_canonical_bytes: MAX_DESIRED_PUBLICATION_CANONICAL_BYTES' "$$root"; \
	timeout 10s grep -Fq 'max_work: MAX_DESIRED_PUBLICATION_WORK' "$$root"; \
	timeout 10s grep -Fq 'logical_bytes: self.budget.logical_bytes' "$$root"; \
	timeout 10s grep -Fq 'builder.budget.path_bytes != plan.publication_path_bytes()' "$$root"; \
	timeout 10s grep -Fq 'builder.budget.logical_bytes != plan.logical_bytes()' "$$root"; \
	if timeout 10s rg -n 'generated_bytes\(|sealed_asset\(|binding_index\b|std::fs|fs_err|OpenOptions|File::(?:open|create)|BorrowedFd|OwnedFd|RawFd|AsFd|FileExt|read_at|read_exact_at|create_dir|remove_(?:file|dir)|rename\(|std::process|process::Command|Command::new' "$$root" "$$error"; then exit 1; else status="$$?"; timeout 10s test "$$status" = 1; fi; \
	if timeout 10s rg -n 'b"fingerprint"|pub\(in crate::client\) fn (?:outputs_mut|delete|persist|write|mutate|descriptor|source|binding_index|mount_id|inode|owner)' "$$root" "$$error"; then exit 1; else status="$$?"; timeout 10s test "$$status" = 1; fi; \
	for file in "$$root" "$$error" "$$tests" "$$test_core"/*.rs "$(DESIRED_PUBLICATION_TOP_DIR)/misc/make/active-reblit-desired-publication-tests.mk"; do \
		timeout 10s test "$$( timeout 10s wc -l < "$$file" )" -le 1000; \
	done; \
	timeout 1200s $(CARGO) test --manifest-path "$(DESIRED_PUBLICATION_TOP_DIR)/Cargo.toml" -p forge --lib "$$prefix" -- --test-threads=1
