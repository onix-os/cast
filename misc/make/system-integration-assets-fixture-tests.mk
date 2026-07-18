.PHONY: system-integration-assets-contract-test \
	system-integration-assets-supplemental-host-test \
	system-integration-assets-fixture-test

SYSTEM_INTEGRATION_ASSETS_TEST_PREFIX := planner::hermetic_tests::system_integration_assets_

system-integration-assets-contract-test:
	@set -eu; \
	listed="$$(timeout 120s $(CARGO) test -p mason --lib "$(SYSTEM_INTEGRATION_ASSETS_TEST_PREFIX)" -- --list)"; \
	test "$$(timeout 10s grep -Fc "$(SYSTEM_INTEGRATION_ASSETS_TEST_PREFIX)" <<<"$$listed")" -eq 2; \
	for test in \
		planner::hermetic_tests::system_integration_assets_declaration_and_assets_fail_closed \
		planner::hermetic_tests::system_integration_assets_tampered_archive_never_becomes_consumable; do \
		timeout 10s grep -Fqx "$$test: test" <<<"$$listed"; \
		timeout 300s $(CARGO) test -p mason --lib "$$test" -- --exact --nocapture --test-threads=1; \
	done

system-integration-assets-supplemental-host-test:
	@timeout 180s dash "$(TOP_DIR)/misc/scripts/test-system-integration-assets-host.sh"

system-integration-assets-fixture-test: system-integration-assets-contract-test \
	system-integration-assets-supplemental-host-test
