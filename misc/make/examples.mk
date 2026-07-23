cache-clean-test:
	@echo "Running the harness-free descriptor-anchored cache-clean proof..."
	@CAST_CACHE_CLEAN_TEST_RUNNER=1 $(CARGO) test -p mason \
		--features cache-clean-test-support --test cache_clean

cast-example-process-supervision-test:
	@echo "Proving every Cast example child is bounded and group-cleaned..."
	@set -eu; \
	if matches="$$( timeout 10s rg -n '\.output\s*\(\s*\)' \
		"$(TOP_DIR)/bin/cast/tests/gluon_examples.rs" \
		"$(TOP_DIR)/bin/cast/tests/gluon_examples" )"; then \
		timeout 10s printf '%s\n' "bare Command::output() is forbidden in the Cast Gluon example harness:" >&2; \
		timeout 10s printf '%s\n' "$$matches" >&2; \
		exit 1; \
	else \
		status=$$?; \
		timeout 10s test "$$status" -eq 1 || exit "$$status"; \
	fi; \
	listed="$$( timeout 300s $(CARGO) test -p cast --test gluon_examples -- --list )"; \
	for test in \
		'every_gluon_package_example_passes_the_public_cast_cli' \
		'process_supervision::bounded_cast_child_supervisor_drop_kills_and_reaps_group' \
		'process_supervision::bounded_cast_child_supervisor_escalates_ignored_term_to_kill' \
		'process_supervision::bounded_cast_child_supervisor_kills_and_reaps_descendant_tree' \
		'process_supervision::bounded_cast_child_supervisor_rejects_exited_leader_with_descendant' \
		'process_supervision::bounded_cast_child_supervisor_times_out_and_reaps_group' \
		'process_supervision::bounded_cast_child_supervisor_rejects_stdout_overflow_and_reaps_group' \
		'process_supervision::bounded_cast_child_supervisor_reuses_one_cleanup_deadline' \
		'process_supervision::cast_child_supervisor_helper'; do \
		timeout 10s grep -Fqx "$$test: test" <<<"$$listed"; \
	done
	@timeout 120s $(CARGO) test -p cast --test gluon_examples \
		process_supervision::bounded_cast_child_supervisor_ -- \
		--test-threads=1 --nocapture

examples: cast-example-process-supervision-test
	@echo "Checking every Gluon package example through the public Cast CLI..."
	@timeout 1200s $(CARGO) test -p cast --test gluon_examples \
		every_gluon_package_example_passes_the_public_cast_cli -- \
		--exact --nocapture
	@echo "Freezing every Gluon package example through the hermetic planner..."
	@set -eu; \
	listed="$$( timeout 300s $(CARGO) test -p mason --lib -- --list )"; \
	timeout 10s grep -Fqx 'planner::hermetic_tests::checked_in_package_examples_freeze_hermetically_and_reuse_exact_build_locks: test' <<<"$$listed"
	@timeout 1200s $(CARGO) test -p mason --lib \
		planner::hermetic_tests::checked_in_package_examples_freeze_hermetically_and_reuse_exact_build_locks -- \
		--exact --nocapture
	@echo "Proving metadata-only providers fail before frozen execution..."
	@set -eu; \
	listed="$$( timeout 300s $(CARGO) test -p mason --lib -- --list )"; \
	timeout 10s grep -Fqx 'planner::hermetic_tests::checked_in_metadata_only_example_fails_closed_before_execution: test' <<<"$$listed"
	@timeout 1200s $(CARGO) test -p mason --lib \
		planner::hermetic_tests::checked_in_metadata_only_example_fails_closed_before_execution -- \
		--exact --nocapture

examples-gate-test:
	@timeout 180s "$(TOP_DIR)/misc/scripts/test-examples-gate.sh"
