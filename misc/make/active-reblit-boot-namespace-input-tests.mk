SHELL := /bin/bash

BOOT_NAMESPACE_INPUT_TOP_DIR := $(abspath $(dir $(lastword $(MAKEFILE_LIST)))/../..)
CARGO ?= cargo

.PHONY: forge-active-reblit-boot-namespace-input-test forge-active-reblit-boot-namespace-input-unit-test

forge-active-reblit-boot-namespace-input-test: host-storage-safety-test forge-active-reblit-bls-renderer-test forge-active-reblit-boot-publication-plan-test forge-linux-descriptor-boot-namespace-production-test forge-active-reblit-boot-namespace-input-unit-test

forge-active-reblit-boot-namespace-input-unit-test:
	@set -euo pipefail; \
	timeout 10s mkdir -p "$(BOOT_NAMESPACE_INPUT_TOP_DIR)/target"; \
	listed="$$( timeout 10s mktemp "$(BOOT_NAMESPACE_INPUT_TOP_DIR)/target/active-reblit-boot-namespace-input-list.XXXXXXXXXXXX" )"; \
	trap 'timeout 10s rm -f "$$listed"' EXIT; \
	timeout 300s $(CARGO) test --manifest-path "$(BOOT_NAMESPACE_INPUT_TOP_DIR)/Cargo.toml" -p forge --lib -- --list | timeout 300s tee "$$listed" >/dev/null; \
	timeout 10s grep -q . "$$listed"; \
	prefix='client::active_reblit_boot_namespace_inputs::tests::'; \
	timeout 10s test "$$( timeout 10s grep -Ec "^$$prefix.*: test$$" "$$listed" )" = 4; \
	for name in \
		alias_plan_binds_one_ordered_domain_and_streams_exact_generated_and_sealed_sources \
		retained_layout_routes_distinct_roots_without_reordering_global_indices \
		count_path_logical_generated_and_work_bounds_accept_n_and_reject_n_minus_one \
		inherited_deadline_is_checked_at_entry_and_after_complete_binding; do \
		timeout 10s grep -Fqx "$$prefix$$name: test" "$$listed"; \
	done; \
	root="$(BOOT_NAMESPACE_INPUT_TOP_DIR)/crates/forge/src/client/boot/active_reblit_boot_namespace_inputs.rs"; \
	error="$(BOOT_NAMESPACE_INPUT_TOP_DIR)/crates/forge/src/client/boot/active_reblit_boot_namespace_inputs/error.rs"; \
	tests="$(BOOT_NAMESPACE_INPUT_TOP_DIR)/crates/forge/src/client/boot/active_reblit_boot_namespace_inputs_tests.rs"; \
	renderer="$(BOOT_NAMESPACE_INPUT_TOP_DIR)/crates/forge/src/client/boot/active_reblit_bls_renderer.rs"; \
	plan="$(BOOT_NAMESPACE_INPUT_TOP_DIR)/crates/forge/src/client/boot/active_reblit_publication_plan.rs"; \
	timeout 10s grep -Fq "fn bind_boot_namespace_inputs<'plan>(" "$$root"; \
	timeout 10s grep -Fq "'input: 'plan" "$$root"; \
	timeout 10s grep -Fq "requests: Box<[BootNamespaceRequest<'plan>]>" "$$root"; \
	timeout 10s grep -Fq "expected_sources: Box<[RetainedBootNamespaceExpectedSource<'plan>]>" "$$root"; \
	timeout 10s grep -Fq "plan_indices: Box<[usize]>" "$$root"; \
	timeout 10s grep -Fq '_same_thread: PhantomData<Rc<()>>' "$$root"; \
	timeout 10s grep -Fq 'let preflight = scan_plan(plan, &mut budget)?;' "$$root"; \
	timeout 10s grep -Fq 'if rebound != preflight {' "$$root"; \
	timeout 10s grep -Fq 'preflight.path_bytes != plan.publication_path_bytes()' "$$root"; \
	timeout 10s grep -Fq 'preflight.generated_bytes != plan.publication_generated_bytes()' "$$root"; \
	timeout 10s grep -Fq 'preflight.logical_bytes != plan.logical_bytes()' "$$root"; \
	timeout 10s grep -Fq 'RetainedBootNamespaceExpectedSource::generated(bytes)' "$$root"; \
	timeout 10s grep -Fq 'RetainedBootNamespaceExpectedSource::sealed_descriptor(asset.descriptor())' "$$root"; \
	timeout 10s grep -Fq 'asset.digest() != output.expected_digest() || asset.length() != output.expected_length()' "$$root"; \
	timeout 10s grep -Fq '.try_reserve_exact(count)' "$$root"; \
	timeout 10s grep -Fq 'actual >= self.expected_count' "$$root"; \
	timeout 10s grep -Fq 'pub(in crate::client) enum ActiveReblitBootDestinationLayout' "$$plan"; \
	timeout 10s grep -Fq 'pub(in crate::client) fn outputs<'"'"'plan>' "$$renderer"; \
	timeout 10s grep -Fq "Item = BoundActiveReblitBlsPublication<'plan, 'input>" "$$renderer"; \
	if timeout 10s rg -U --pcre2 -n '#\[derive\((?s:[^]]*(?:Clone|Copy)[^]]*)\)\]\s*(?:pub(?:\([^)]*\))?\s+)?(?:struct|enum)\s+(?:BoundActiveReblitBootNamespaceInputs|BoundActiveReblitBootNamespaceDomain)|impl(?:<(?s:[^;{}]*)>)?\s+Clone\s+for\s+(?:BoundActiveReblitBootNamespaceInputs|BoundActiveReblitBootNamespaceDomain)|fn\s+(?:descriptor|generated_bytes|sealed_asset|into_inner|detach|into_unbound)\b' "$$root" "$$error"; then exit 1; else status="$$?"; timeout 10s test "$$status" = 1; fi; \
	if timeout 10s rg -n 'F_DUPFD|\bdup(?:2|3)?\s*\(|File::open|OpenOptions|canonicalize\(|read_to_end|to_vec\(|\.clone\(\)|std::process|process::Command|Command::new|nix::mount|libc::mount|mount_partitions|/dev/(disk|sd|hd|vd|xvd|nvme|mmcblk|loop|md|dm-|nbd|zram)' "$$root" "$$error"; then exit 1; else status="$$?"; timeout 10s test "$$status" = 1; fi; \
	for file in "$$root" "$$error" "$$tests" "$$renderer" "$$plan" "$(BOOT_NAMESPACE_INPUT_TOP_DIR)/misc/make/active-reblit-boot-namespace-input-tests.mk"; do \
		timeout 10s test "$$( timeout 10s wc -l < "$$file" )" -le 1000; \
	done; \
	timeout 1200s $(CARGO) test --manifest-path "$(BOOT_NAMESPACE_INPUT_TOP_DIR)/Cargo.toml" -p forge --lib "$$prefix" -- --test-threads=1
