.PHONY: desktop-integration-contract-test \
	desktop-integration-supplemental-host-test \
	desktop-integration-fixture-test

DESKTOP_INTEGRATION_TEST_PREFIX := planner::hermetic_tests::desktop_integration_

desktop-integration-contract-test:
	@set -eu; \
	listed="$$(timeout 120s $(CARGO) test -p mason --lib "$(DESKTOP_INTEGRATION_TEST_PREFIX)" -- --list)"; \
	test "$$(timeout 10s grep -Fc "$(DESKTOP_INTEGRATION_TEST_PREFIX)" <<<"$$listed")" -eq 2; \
	for test in \
		planner::hermetic_tests::desktop_integration_declaration_and_assets_fail_closed \
		planner::hermetic_tests::desktop_integration_tampered_archive_never_becomes_consumable; do \
		timeout 10s grep -Fqx "$$test: test" <<<"$$listed"; \
		timeout 300s $(CARGO) test -p mason --lib "$$test" -- --exact --nocapture --test-threads=1; \
	done

desktop-integration-supplemental-host-test:
	@timeout 180s dash "$(TOP_DIR)/misc/scripts/test-desktop-integration-host.sh"

desktop-integration-fixture-test: desktop-integration-contract-test \
	desktop-integration-supplemental-host-test
