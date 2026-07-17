include misc/make/startup-candidate-preserve-target-preparation-tests.mk
include misc/make/startup-candidate-preserve-target-creation-tests.mk
include misc/make/startup-candidate-preserve-target-normalization-tests.mk

.PHONY: forge-startup-usr-rollback-candidate-preserve-target-test

forge-startup-usr-rollback-candidate-preserve-target-test: \
	forge-startup-usr-rollback-candidate-preserve-target-preparation-test \
	forge-startup-usr-rollback-candidate-preserve-target-creation-test \
	forge-startup-usr-rollback-candidate-preserve-target-normalization-test
	@set -eu; \
	listed="$$( timeout 300s $(CARGO) test -p forge --lib -- --list )"; \
	count="$$( timeout 10s grep -Ec '^client::startup_reconciliation::usr_rollback_candidate_preserve_authority::tests::target_(preparation|creation|normalization)::.*: test$$' <<<"$$listed" )"; \
	timeout 10s test "$$count" = 26
