STARTUP_ACTIVE_REBLIT_COMMIT_CLEANUP_DISPATCH_TOP_DIR := $(abspath $(dir $(lastword $(MAKEFILE_LIST)))/../..)
CARGO ?= cargo

.PHONY: forge-startup-active-reblit-commit-cleanup-dispatch-test

forge-startup-active-reblit-commit-cleanup-dispatch-test:
	@set -euo pipefail; \
	mkdir -p "$(STARTUP_ACTIVE_REBLIT_COMMIT_CLEANUP_DISPATCH_TOP_DIR)/target"; \
	listed="$$( mktemp "$(STARTUP_ACTIVE_REBLIT_COMMIT_CLEANUP_DISPATCH_TOP_DIR)/target/startup-active-reblit-commit-cleanup-dispatch-list.XXXXXXXXXXXX" )"; \
	trap 'rm -f "$$listed"' EXIT; \
	$(CARGO) test --manifest-path "$(STARTUP_ACTIVE_REBLIT_COMMIT_CLEANUP_DISPATCH_TOP_DIR)/Cargo.toml" -p forge --lib -- --list | tee "$$listed" >/dev/null; \
	prefix='client::startup_gate::usr_rollback_active_reblit::tests::commit_cleanup_startup_dispatch::'; \
	test "$$( grep -Ec "^$$prefix.*: test$$" "$$listed" )" = 4; \
	for name in startup_cleanup_current_and_historical_apply_and_finish_persist_once startup_cleanup_all_five_journal_faults_classify_and_converge_without_second_exchange cleanup_persistence_all_binding_windows_reject_same_bytes_on_a_new_inode cleanup_persistence_rejects_database_and_namespace_changes_in_new_windows; do \
		grep -Fqx "$$prefix$$name: test" "$$listed"; \
	done; \
	root="$(STARTUP_ACTIVE_REBLIT_COMMIT_CLEANUP_DISPATCH_TOP_DIR)/crates/forge/src/client"; \
	gate="$$root/startup_gate.rs"; dispatch="$$root/startup_gate/active_reblit_commit_cleanup.rs"; \
	recovery="$$root/startup_recovery/active_reblit_commit_cleanup_complete.rs"; \
	advance="$$root/startup_reconciliation/active_reblit_commit_cleanup_authority/effect/record_advance.rs"; \
	tests="$$root/startup_gate/usr_rollback_active_reblit/tests/commit_cleanup_startup_dispatch.rs"; \
	grep -Fqx 'mod active_reblit_commit_cleanup;' "$$gate"; \
	grep -Fq 'persist_active_reblit_commit_cleanup_complete_and_reopen(journal, durable)' "$$dispatch"; \
	test "$$( grep -Fc '.advance_record_binding(cast, journal_record_binding, successor)' "$$advance" )" = 1; \
	grep -Fq 'let successor = match source_record.forward_successor(None)' "$$recovery"; \
	grep -Fq 'drop(journal);' "$$recovery"; \
	grep -Fq 'reopen_canonical_journal(&installation)' "$$recovery"; \
	if rg -n '[.]advance\(|delete_record_binding|promote_boot_publication_receipt|stage_boot_publication_receipt|boot::|run_(transaction|system)_triggers|finalize_usr|std::fs::(rename|remove|write|set_permissions)|Command::new|nix::mount|libc::mount' "$$dispatch" "$$recovery" "$$advance"; then exit 1; else status="$$?"; test "$$status" = 1; fi; \
	for file in "$$gate" "$$dispatch" "$$recovery" "$$advance" "$$tests" "$(STARTUP_ACTIVE_REBLIT_COMMIT_CLEANUP_DISPATCH_TOP_DIR)/misc/make/startup-active-reblit-commit-cleanup-dispatch-tests.mk"; do test "$$( wc -l < "$$file" )" -le 1000; done; \
	$(CARGO) test --manifest-path "$(STARTUP_ACTIVE_REBLIT_COMMIT_CLEANUP_DISPATCH_TOP_DIR)/Cargo.toml" -p forge --lib "$$prefix" -- --test-threads=1
