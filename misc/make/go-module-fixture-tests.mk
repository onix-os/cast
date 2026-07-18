.PHONY: go-module-contract-test \
	go-module-supplemental-host-test \
	go-module-fixture-test

GO_MODULE_TEST_PREFIX := planner::hermetic_tests::go_module_

go-module-contract-test:
	@set -eu; \
	listed="$$(timeout 120s $(CARGO) test -p mason --lib "$(GO_MODULE_TEST_PREFIX)" -- --list)"; \
	test "$$(timeout 10s grep -Fc "$(GO_MODULE_TEST_PREFIX)" <<<"$$listed")" -eq 2; \
	for test in \
		planner::hermetic_tests::go_module_declaration_and_vendor_tree_fail_closed \
		planner::hermetic_tests::go_module_tampered_archive_never_becomes_consumable; do \
		timeout 10s grep -Fqx "$$test: test" <<<"$$listed"; \
		timeout 300s $(CARGO) test -p mason --lib "$$test" -- --exact --nocapture --test-threads=1; \
	done

go-module-supplemental-host-test:
	@timeout 300s dash "$(TOP_DIR)/misc/scripts/test-go-module-host.sh"

go-module-fixture-test: go-module-contract-test \
	go-module-supplemental-host-test
