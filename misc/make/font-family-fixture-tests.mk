.PHONY: font-family-source-test \
	font-family-contract-test \
	font-family-supplemental-host-test \
	font-family-fixture-test

FONT_FAMILY_TEST_PREFIX := planner::hermetic_tests::font_family_

font-family-source-test:
	@timeout 120s dash "$(TOP_DIR)/misc/scripts/build-font-family-fixture-fonts.sh" --check

font-family-contract-test:
	@set -eu; \
	listed="$$(timeout 120s $(CARGO) test -p mason --lib "$(FONT_FAMILY_TEST_PREFIX)" -- --list)"; \
	test "$$(timeout 10s grep -Fc "$(FONT_FAMILY_TEST_PREFIX)" <<<"$$listed")" -eq 2; \
	for test in \
		planner::hermetic_tests::font_family_declaration_and_assets_fail_closed \
		planner::hermetic_tests::font_family_tampered_archive_never_becomes_consumable; do \
		timeout 10s grep -Fqx "$$test: test" <<<"$$listed"; \
		timeout 300s $(CARGO) test -p mason --lib "$$test" -- --exact --nocapture --test-threads=1; \
	done

font-family-supplemental-host-test:
	@timeout 180s dash "$(TOP_DIR)/misc/scripts/test-font-family-host.sh"

font-family-fixture-test: font-family-source-test \
	font-family-contract-test \
	font-family-supplemental-host-test
