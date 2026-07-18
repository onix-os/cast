.PHONY: gettext-localization-contract-test \
	gettext-localization-supplemental-host-test \
	gettext-localization-fixture-test

GETTEXT_LOCALIZATION_TEST_PREFIX := planner::hermetic_tests::gettext_localization_

gettext-localization-contract-test:
	@set -eu; \
	listed="$$(timeout 120s $(CARGO) test -p mason --lib "$(GETTEXT_LOCALIZATION_TEST_PREFIX)" -- --list)"; \
	timeout 10s test "$$(timeout 10s grep -Fc "$(GETTEXT_LOCALIZATION_TEST_PREFIX)" <<<"$$listed")" -eq 2; \
	for test in \
		planner::hermetic_tests::gettext_localization_declaration_and_catalogs_fail_closed \
		planner::hermetic_tests::gettext_localization_tampered_archive_never_becomes_consumable; do \
		timeout 10s grep -Fqx "$$test: test" <<<"$$listed"; \
		timeout 300s $(CARGO) test -p mason --lib "$$test" -- --exact --nocapture --test-threads=1; \
	done

gettext-localization-supplemental-host-test:
	@timeout 180s bash "$(TOP_DIR)/misc/scripts/test-gettext-localization-host.sh"

gettext-localization-fixture-test: gettext-localization-contract-test \
	gettext-localization-supplemental-host-test
