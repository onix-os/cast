STARTUP_ACTIVE_REBLIT_COMMIT_CLEANUP_COMPLETE_TOP_DIR := $(abspath $(dir $(lastword $(MAKEFILE_LIST)))/../..)
CARGO ?= cargo

.PHONY: forge-startup-active-reblit-commit-cleanup-complete-test

forge-startup-active-reblit-commit-cleanup-complete-test:
	@set -euo pipefail; \
	mkdir -p "$(STARTUP_ACTIVE_REBLIT_COMMIT_CLEANUP_COMPLETE_TOP_DIR)/target"; \
	listed="$$( mktemp "$(STARTUP_ACTIVE_REBLIT_COMMIT_CLEANUP_COMPLETE_TOP_DIR)/target/startup-active-reblit-commit-cleanup-complete-list.XXXXXXXXXXXX" )"; \
	trap 'rm -f "$$listed"' EXIT; \
	$(CARGO) test --manifest-path "$(STARTUP_ACTIVE_REBLIT_COMMIT_CLEANUP_COMPLETE_TOP_DIR)/Cargo.toml" -p forge --lib -- --list | tee "$$listed" >/dev/null; \
	prefix='client::startup_gate::usr_rollback_active_reblit::tests::commit_cleanup_complete_startup_dispatch::'; \
	test "$$( grep -Ec "^$$prefix.*: test$$" "$$listed" )" = 5; \
	for name in completed_cleanup_current_and_historical_apply_and_finish_reaches_complete_once committed_retirement_report_error_reenters_finish_and_completes completed_cleanup_all_five_journal_faults_classify_and_converge complete_persistence_all_binding_windows_reject_same_bytes_on_a_new_inode completed_cleanup_database_and_namespace_races_fail_closed; do \
		grep -Fqx "$$prefix$$name: test" "$$listed"; \
	done; \
	root="$(STARTUP_ACTIVE_REBLIT_COMMIT_CLEANUP_COMPLETE_TOP_DIR)/crates/forge/src/client"; \
	gate="$$root/startup_gate.rs"; dispatch="$$root/startup_gate/active_reblit_commit_cleanup_complete.rs"; \
	recovery="$$root/startup_recovery/active_reblit_commit_cleanup_complete_to_complete.rs"; \
	authority="$$root/startup_reconciliation/active_reblit_commit_cleanup_complete_authority.rs"; \
	proof="$$root/startup_reconciliation/activation_namespace/active_reblit_commit_cleanup_proof.rs"; \
	tests="$$root/startup_gate/usr_rollback_active_reblit/tests/commit_cleanup_complete_startup_dispatch.rs"; \
	grep -Fqx 'mod active_reblit_commit_cleanup_complete;' "$$gate"; \
	grep -Fq 'persist_active_reblit_commit_cleanup_complete_to_complete_and_reopen(journal, retired)' "$$dispatch"; \
	test "$$( grep -Fc '.retire_promoted_boot_publication_receipt_head(' "$$authority" )" = 1; \
	test "$$( grep -Fc '.advance_record_binding(' "$$authority" )" = 1; \
	grep -Fq 'drop(journal);' "$$recovery"; \
	grep -Fq 'reopen_canonical_journal(&installation)' "$$recovery"; \
	grep -Fq 'revalidate_completed_namespace' "$$proof"; \
	if rg -n '[.]advance\(|delete_record_binding|promote_boot_publication_receipt|stage_boot_publication_receipt|boot::|run_(transaction|system)_triggers|finalize_usr|std::fs::(rename|remove|write|set_permissions)|Command::new|nix::mount|libc::mount' "$$dispatch" "$$recovery" "$$authority"; then exit 1; else status="$$?"; test "$$status" = 1; fi; \
	for file in "$$gate" "$$dispatch" "$$recovery" "$$authority" "$$proof" "$$tests" "$(STARTUP_ACTIVE_REBLIT_COMMIT_CLEANUP_COMPLETE_TOP_DIR)/misc/make/startup-active-reblit-commit-cleanup-complete-tests.mk"; do test "$$( wc -l < "$$file" )" -le 1000; done; \
	$(CARGO) test --manifest-path "$(STARTUP_ACTIVE_REBLIT_COMMIT_CLEANUP_COMPLETE_TOP_DIR)/Cargo.toml" -p forge --lib "$$prefix" -- --test-threads=1
