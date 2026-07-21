ACTIVE_REBLIT_BOOT_PREFLIGHT_TOP_DIR := $(abspath $(dir $(lastword $(MAKEFILE_LIST)))/../..)
CARGO ?= cargo

.PHONY: forge-active-reblit-boot-publication-preflight-test

forge-active-reblit-boot-publication-preflight-test: host-storage-safety-test forge-active-reblit-boot-namespace-input-test forge-active-reblit-mounted-boot-topology-capture-test
	@set -euo pipefail; \
	mkdir -p "$(ACTIVE_REBLIT_BOOT_PREFLIGHT_TOP_DIR)/target"; \
	listed="$$( mktemp "$(ACTIVE_REBLIT_BOOT_PREFLIGHT_TOP_DIR)/target/active-reblit-boot-preflight-list.XXXXXXXXXXXX" )"; \
	trap 'rm -f "$$listed"' EXIT; \
	$(CARGO) test --manifest-path "$(ACTIVE_REBLIT_BOOT_PREFLIGHT_TOP_DIR)/Cargo.toml" -p forge --lib -- --list | tee "$$listed" >/dev/null; \
	test -s "$$listed"; \
	prefix='client::active_reblit_boot_publication_preflight::tests::'; \
	test "$$( grep -Ec "^$$prefix.*: test$$" "$$listed" )" = 5; \
	for name in \
		global_merge::alias_and_distinct_domains_restore_exact_global_plan_order \
		global_merge::different_count_index_and_identity_evidence_fail_closed \
		integration::full_alias_preflight_retains_global_states_and_deadline_without_effects \
		integration::inherited_deadline_fails_closed_at_each_outer_preflight_boundary \
		integration::collision_and_terminal_topology_drift_fail_after_read_only_assessment; do \
		grep -Fqx "$$prefix$$name: test" "$$listed"; \
	done; \
	core="$(ACTIVE_REBLIT_BOOT_PREFLIGHT_TOP_DIR)/crates/forge/src/client/boot/active_reblit_boot_publication_preflight.rs"; \
	targets="$(ACTIVE_REBLIT_BOOT_PREFLIGHT_TOP_DIR)/crates/forge/src/client/boot/active_reblit_mounted_boot_topology/capture/publication_targets.rs"; \
	test_root="$(ACTIVE_REBLIT_BOOT_PREFLIGHT_TOP_DIR)/crates/forge/src/client/boot/active_reblit_boot_publication_preflight_tests.rs"; \
	test_core="$(ACTIVE_REBLIT_BOOT_PREFLIGHT_TOP_DIR)/crates/forge/src/client/boot/active_reblit_boot_publication_preflight_tests"; \
	grep -Fq "plan: &'plan BoundActiveReblitBlsPublicationPlan" "$$core"; \
	grep -Fq "namespace_inputs: BoundActiveReblitBootNamespaceInputs<'plan>" "$$core"; \
	grep -Fq "targets: RevalidatedActiveReblitBootPublicationTargets<'plan>" "$$core"; \
	grep -Fq 'initial_states: Box<[BootNamespaceDestinationState]>' "$$core"; \
	grep -Fq "pub(in crate::client) fn prepare_boot_publication_preflight<'plan>" "$$core"; \
	grep -Fq 'fn prepare_boot_publication_preflight_fixture_with<' "$$core"; \
	test "$$( grep -Fc '.revalidate_publication_targets()' "$$core" )" = 2; \
	grep -Fq 'let initial_states = assess_bound_namespaces_with(' "$$core"; \
	grep -Fq 'if !self.collision_domains_still_match() {' "$$core"; \
	grep -Fq 'require_same_target_set(&targets, &terminal_targets)?;' "$$core"; \
	grep -Fq '.try_reserve_exact(publication_count)' "$$core"; \
	grep -Fq 'ActiveReblitBootPublicationPreflightError::DifferentDestination' "$$core"; \
	grep -Fq 'BootPublicationNamespaceAssessment::fixture' "$$test_core/support.rs"; \
	grep -Fq '&[1, 3]' "$$test_core/global_merge.rs"; \
	grep -Fq '&[0, 2, 4]' "$$test_core/global_merge.rs"; \
	grep -Fq 'pub(in crate::client) fn assess_boot_namespace(' "$$targets"; \
	grep -Fq 'BootNamespaceAssessmentLimits::default()' "$$targets"; \
	grep -Fq 'RetainedBootNamespaceAssessmentLimits::default()' "$$targets"; \
	grep -Fq 'self.deadline,' "$$targets"; \
	if rg --pcre2 -U -n '#\[derive\([^]]*(?:Clone|Copy)[^]]*\)\]\s*pub\(in crate::client\) struct RevalidatedActiveReblitBootPublicationPreflight|impl(?:<[^>]+>)?\s+(?:Clone|Copy)\s+for\s+RevalidatedActiveReblitBootPublicationPreflight' "$$core"; then exit 1; else status="$$?"; test "$$status" = 1; fi; \
	if rg --pcre2 -U -n 'pub\(in crate::client\)\s+fn\s+[[:alnum:]_]+[^\{;]{0,400}(?:std::fs::File|OwnedFd|BorrowedFd|RawFd|PathBuf|&Path|RevalidatedActiveReblitBootPublicationTarget|BoundActiveReblitBootNamespaceDomain)' "$$core"; then exit 1; else status="$$?"; test "$$status" = 1; fi; \
	if rg -n 'publish_immutable_boot_file\(|retain_boot_publication_parent\(|create_dir(?:_all)?\(|rename\(|unlink(?:at)?\(|remove_(?:file|dir)\(|sync_all\(|syncfs\(|nix::mount|libc::mount|std::process|process::Command|Command::new' "$$core"; then exit 1; else status="$$?"; test "$$status" = 1; fi; \
	for file in "$$core" "$$targets" "$$test_root" "$$test_core"/*.rs "$(ACTIVE_REBLIT_BOOT_PREFLIGHT_TOP_DIR)/misc/make/active-reblit-boot-publication-preflight-tests.mk"; do \
		test "$$( wc -l < "$$file" )" -le 1000; \
	done; \
	$(CARGO) test --manifest-path "$(ACTIVE_REBLIT_BOOT_PREFLIGHT_TOP_DIR)/Cargo.toml" -p forge --lib "$$prefix" -- --test-threads=1
