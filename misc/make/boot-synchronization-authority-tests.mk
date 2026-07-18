.PHONY: forge-clean-boot-synchronization-test \
	forge-legacy-boot-repair-test

forge-clean-boot-synchronization-test:
	@set -euo pipefail; \
	listed="$$( timeout 300s $(CARGO) test -p forge --lib -- --list )"; \
	timeout 10s grep -q . <<<"$$listed"; \
	count="$$( timeout 10s grep -Ec '^client::clean_boot_synchronization::tests::[^:]+: test$$' <<<"$$listed" )"; \
	timeout 10s test "$$count" = 5; \
	for test in \
		client::clean_boot_synchronization::tests::clean_standalone_boot_synchronization_retains_authority_through_one_worker_attempt \
		client::clean_boot_synchronization::tests::final_public_journal_binding_rejects_replacement_after_the_leading_check \
		client::clean_boot_synchronization::tests::unresolved_journal_blocks_standalone_boot_before_the_worker \
		client::clean_boot_synchronization::tests::orphan_transition_row_blocks_standalone_boot_before_the_worker \
		client::clean_boot_synchronization::tests::post_authority_failure_supersedes_the_boot_backend_error; do \
		timeout 10s grep -Fqx "$$test: test" <<<"$$listed"; \
	done; \
	timeout 900s $(CARGO) test -p forge --lib "client::clean_boot_synchronization::tests::" -- --test-threads=1

forge-legacy-boot-repair-test:
	@set -euo pipefail; \
	listed="$$( timeout 300s $(CARGO) test -p forge --lib -- --list )"; \
	timeout 10s grep -q . <<<"$$listed"; \
	count="$$( timeout 10s grep -Ec '^client::legacy_boot_repair::tests::[^:]+: test$$' <<<"$$listed" )"; \
	timeout 10s test "$$count" = 4; \
	for test in \
		client::legacy_boot_repair::tests::legacy_worker_rejects_a_client_with_a_different_state_database_capability \
		client::legacy_boot_repair::tests::legacy_worker_rejects_public_journal_replacement_during_boot \
		client::legacy_boot_repair::tests::legacy_worker_retains_the_exact_journal_lock_through_boot \
		client::legacy_boot_repair::tests::legacy_authorization_rechecks_orphan_transition_ownership; do \
		timeout 10s grep -Fqx "$$test: test" <<<"$$listed"; \
	done; \
	timeout 900s $(CARGO) test -p forge --lib "client::legacy_boot_repair::tests::" -- --test-threads=1
