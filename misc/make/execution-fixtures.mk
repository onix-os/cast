fixture-sources:
	@"$(TOP_DIR)/misc/scripts/build-execution-fixtures.sh"

fixture-sources-check:
	@"$(TOP_DIR)/misc/scripts/build-execution-fixtures.sh" --check

execution-fixtures: fixture-sources-check multiple-sources-fixture-test gettext-localization-fixture-test go-module-fixture-test python-module-fixture-test pgo-workload-fixture-test relation-policy-fixture-test desktop-integration-fixture-test font-family-fixture-test system-integration-assets-fixture-test external-test-vectors-fixture-test
	@echo "Checking locked offline execution-source fixtures..."
	@set -eu; \
	listed="$$( $(CARGO) test -p mason --lib \
		planner::hermetic_tests::offline_execution_fixture_archives_are_real_locked_and_complete -- \
		--exact --list )"; \
	printf '%s\n' "$$listed" | \
		grep -Fqx 'planner::hermetic_tests::offline_execution_fixture_archives_are_real_locked_and_complete: test'
	@$(CARGO) test -p mason --lib \
		planner::hermetic_tests::offline_execution_fixture_archives_are_real_locked_and_complete -- \
		--exact --nocapture
	@echo "Checking fail-closed external patch-source admission..."
	@set -eu; \
	listed="$$( $(CARGO) test -p mason --lib \
		planner::hermetic_tests::hooks_patch_external_source_contract_fails_closed -- \
		--exact --list )"; \
	printf '%s\n' "$$listed" | \
		grep -Fqx 'planner::hermetic_tests::hooks_patch_external_source_contract_fails_closed: test'
	@$(CARGO) test -p mason --lib \
		planner::hermetic_tests::hooks_patch_external_source_contract_fails_closed -- \
		--exact --nocapture
	@echo "Checking the declarative pinned Stone bootstrap manifest and index..."
	@set -eu; \
	listed="$$( $(CARGO) test -p mason --lib \
		planner::hermetic_tests::bootstrap::pinned_bootstrap_manifest_is_bounded_and_index_authoritative -- \
		--exact --list )"; \
	printf '%s\n' "$$listed" | \
		grep -Fqx 'planner::hermetic_tests::bootstrap::pinned_bootstrap_manifest_is_bounded_and_index_authoritative: test'
	@$(CARGO) test -p mason --lib \
		planner::hermetic_tests::bootstrap::pinned_bootstrap_manifest_is_bounded_and_index_authoritative -- \
		--exact --nocapture
	@echo "Resolving all twenty-eight execution fixtures against the pinned real Stone index..."
	@set -eu; \
	listed="$$( $(CARGO) test -p mason --lib \
		planner::hermetic_tests::bootstrap::all_execution_fixtures_resolve_exactly_the_pinned_real_stone_closure -- \
		--exact --list )"; \
	printf '%s\n' "$$listed" | \
		grep -Fqx 'planner::hermetic_tests::bootstrap::all_execution_fixtures_resolve_exactly_the_pinned_real_stone_closure: test'
	@$(CARGO) test -p mason --lib \
		planner::hermetic_tests::bootstrap::all_execution_fixtures_resolve_exactly_the_pinned_real_stone_closure -- \
		--exact --nocapture

bootstrap-fixtures-tmp:
	@set -eu; \
	tmpdir="$(BOOTSTRAP_TMP_DIR)"; \
	if [[ -L "$$tmpdir" || -e "$$tmpdir" && ! -d "$$tmpdir" ]]; then \
		echo "Refusing unsafe bootstrap TMPDIR: $$tmpdir" >&2; \
		exit 1; \
	fi; \
	if [[ -e "$$tmpdir" && ! -O "$$tmpdir" ]]; then \
		echo "Refusing bootstrap TMPDIR not owned by the current user: $$tmpdir" >&2; \
		exit 1; \
	fi; \
	install -d -m 700 "$$tmpdir"; \
	chmod 700 "$$tmpdir"; \
	[[ "$$(stat -c '%a' "$$tmpdir")" == 700 ]]

bootstrap-fixture-selection:
	@$(if $(VALID_FIXTURE_SELECTION),:,$(error FIXTURE must be exactly 'all' or one of: $(EXECUTION_FIXTURE_NAMES)))

bootstrap-execution-requirement:
	@$(if $(VALID_EXECUTION_REQUIREMENT),:,$(error REQUIRE_EXECUTION must be exactly '0' or '1'))

execution-capability-preflight-test:
	@$(CARGO) check -p mason --features delegated-fixture-test-support \
		--test delegated_execution_fixture
	@set -eu; \
	listed="$$( $(CARGO) test -p mason --lib -- --list )"; \
	for test in \
		delegated_preflight_tests::execution_requirement_rejects_missing_or_invalid_values \
		delegated_preflight_tests::successful_preflight_executes_fixture_materialization_once_for_both_policies \
		delegated_preflight_tests::optional_capability_denial_short_circuits_before_fixture_materialization \
		delegated_preflight_tests::required_capability_denial_fails_before_fixture_materialization \
		container::preflight::tests::execution_preflight_root_is_an_opath_directory_capability \
		container::preflight::tests::execution_preflight_classifies_only_known_namespace_setup_denials \
		planner::hermetic_tests::frozen_execution_capability_skip_never_hides_payload_or_ambiguous_nix_failures \
		planner::hermetic_tests::bootstrap::all_execution_fixtures_resolve_exactly_the_pinned_real_stone_closure; do \
		grep -Fqx "$$test: test" <<<"$$listed"; \
		$(CARGO) test -p mason --lib "$$test" -- --exact --test-threads=1; \
	done

delegated-execution-preflight: bootstrap-fixtures-tmp
	@echo "Checking the exact production execution capability before bootstrap download..."
	@TMPDIR="$(BOOTSTRAP_TMP_DIR)" \
		CAST_REQUIRE_EXECUTION=1 \
		CARGO="$(CARGO)" \
		"$(TOP_DIR)/misc/scripts/run-delegated-execution-fixture.sh" --preflight-only

bootstrap-fixtures-prepare: bootstrap-fixtures-tmp
	@echo "Fetching and verifying the exact contentful Stone bootstrap closure..."
	@set -eu; \
	listed="$$( TMPDIR="$(BOOTSTRAP_TMP_DIR)" $(CARGO) test -p mason --lib \
		planner::hermetic_tests::bootstrap::fetch_pinned_bootstrap_package_files -- \
		--ignored --exact --list )"; \
	printf '%s\n' "$$listed" | \
		grep -Fqx 'planner::hermetic_tests::bootstrap::fetch_pinned_bootstrap_package_files: test'
	@TMPDIR="$(BOOTSTRAP_TMP_DIR)" $(CARGO) test -p mason --lib \
		planner::hermetic_tests::bootstrap::fetch_pinned_bootstrap_package_files -- \
		--ignored --exact --nocapture

bootstrap-fixtures-offline: bootstrap-fixture-selection bootstrap-execution-requirement bootstrap-fixtures-tmp
	@echo "Requiring the complete verified bootstrap store; this lane performs no downloads..."
	@echo "Materializing the complete closure as a production-format offline root mirror..."
	@set -eu; \
	listed="$$( TMPDIR="$(BOOTSTRAP_TMP_DIR)" $(CARGO) test -p mason --lib \
		planner::hermetic_tests::bootstrap::contentful_bootstrap_materializes_a_complete_offline_root_mirror -- \
		--ignored --exact --list )"; \
	printf '%s\n' "$$listed" | \
		grep -Fqx 'planner::hermetic_tests::bootstrap::contentful_bootstrap_materializes_a_complete_offline_root_mirror: test'
	@TMPDIR="$(BOOTSTRAP_TMP_DIR)" $(CARGO) test -p mason --lib \
		planner::hermetic_tests::bootstrap::contentful_bootstrap_materializes_a_complete_offline_root_mirror -- \
		--ignored --exact --nocapture
	@$(MAKE) --no-print-directory delegated-execution-fixtures \
		FIXTURE=$(FIXTURE_SELECTION) REQUIRE_EXECUTION=$(EXECUTION_REQUIREMENT)

delegated-execution-fixtures: bootstrap-fixture-selection bootstrap-execution-requirement bootstrap-fixtures-tmp
	@echo "Building, packaging, and reproducing fixture selection '$(FIXTURE_SELECTION)' in an explicit delegated unit..."
	@TMPDIR="$(BOOTSTRAP_TMP_DIR)" \
		CAST_BOOTSTRAP_PACKAGE_STORE="$(BOOTSTRAP_PACKAGE_STORE)" \
		CAST_FIXTURE_EVIDENCE_DIR="$${CAST_FIXTURE_EVIDENCE_DIR:-$(TOP_DIR)/target/fixture-evidence}" \
		CAST_REQUIRE_EXECUTION="$(EXECUTION_REQUIREMENT)" \
		CARGO="$(CARGO)" \
		"$(TOP_DIR)/misc/scripts/run-delegated-execution-fixture.sh" "$(FIXTURE_SELECTION)"

delegated-fixture-runner-test: fixture-proof-test
	@"$(TOP_DIR)/misc/scripts/test-fixture-runtime-budgets.sh"
	@"$(TOP_DIR)/misc/scripts/test-stop-owned-fixture-unit.sh"
	@"$(TOP_DIR)/misc/scripts/test-latched-command-faults.sh"
	@"$(TOP_DIR)/misc/scripts/test-run-delegated-execution-fixture.sh"
	@"$(TOP_DIR)/misc/scripts/test-run-fixtures-ci-with-evidence.sh"

bootstrap-fixtures: bootstrap-fixture-selection bootstrap-execution-requirement bootstrap-fixtures-prepare
	@$(MAKE) --no-print-directory bootstrap-fixtures-offline \
		FIXTURE=$(FIXTURE_SELECTION) REQUIRE_EXECUTION=$(EXECUTION_REQUIREMENT)

fixtures-ci: execution-fixtures
	@$(MAKE) --no-print-directory bootstrap-fixtures-prepare
	@$(MAKE) --no-print-directory bootstrap-fixtures-offline REQUIRE_EXECUTION=1 FIXTURE=all
