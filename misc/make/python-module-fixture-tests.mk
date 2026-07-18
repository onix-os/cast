.PHONY: python-module-contract-test \
	python-module-supplemental-host-test \
	python-module-fixture-test

PYTHON_MODULE_TEST_PREFIX := planner::hermetic_tests::python_module_

python-module-contract-test:
	@set -eu; \
	listed="$$(timeout 120s $(CARGO) test -p mason --lib "$(PYTHON_MODULE_TEST_PREFIX)" -- --list)"; \
	test "$$(timeout 10s grep -Fc "$(PYTHON_MODULE_TEST_PREFIX)" <<<"$$listed")" -eq 2; \
	for test in \
		planner::hermetic_tests::python_module_declaration_and_pep517_tree_fail_closed \
		planner::hermetic_tests::python_module_tampered_archive_never_becomes_consumable; do \
		timeout 10s grep -Fqx "$$test: test" <<<"$$listed"; \
		timeout 300s $(CARGO) test -p mason --lib "$$test" -- --exact --nocapture --test-threads=1; \
	done

python-module-supplemental-host-test:
	@timeout 300s bash "$(TOP_DIR)/misc/scripts/test-python-module-host.sh"

python-module-fixture-test: python-module-contract-test \
	python-module-supplemental-host-test
