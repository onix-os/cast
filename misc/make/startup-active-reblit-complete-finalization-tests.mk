STARTUP_ACTIVE_REBLIT_COMPLETE_FINALIZATION_TOP_DIR := $(abspath $(dir $(lastword $(MAKEFILE_LIST)))/../..)
CARGO ?= cargo

.PHONY: forge-startup-active-reblit-complete-finalization-test

forge-startup-active-reblit-complete-finalization-test:
	@set -euo pipefail; \
	mkdir -p "$(STARTUP_ACTIVE_REBLIT_COMPLETE_FINALIZATION_TOP_DIR)/target"; \
	listed="$$( mktemp "$(STARTUP_ACTIVE_REBLIT_COMPLETE_FINALIZATION_TOP_DIR)/target/startup-active-reblit-complete-finalization-list.XXXXXXXXXXXX" )"; \
	trap 'rm -f "$$listed"' EXIT; \
	$(CARGO) test --manifest-path "$(STARTUP_ACTIVE_REBLIT_COMPLETE_FINALIZATION_TOP_DIR)/Cargo.toml" -p forge --lib -- --list | tee "$$listed" >/dev/null; \
	prefix='client::startup_gate::usr_rollback_active_reblit::tests::complete_forward_finalization::'; \
	test "$$( grep -Ec "^$$prefix.*: test$$" "$$listed" )" = 5; \
	for name in forward_complete_current_and_historical_finalizes_next_entry_and_clean_reentry forward_complete_exact_incompatibilities_stay_pending_without_unrelated_effects forward_complete_binding_substitution_fails_before_delete_and_converges forward_complete_delete_fault_states_remain_errors_and_next_entry_converges forward_complete_post_delete_database_selection_namespace_and_public_record_races_fail_closed; do \
		grep -Fqx "$$prefix$$name: test" "$$listed"; \
	done; \
	root="$(STARTUP_ACTIVE_REBLIT_COMPLETE_FINALIZATION_TOP_DIR)/crates/forge/src/client"; \
	gate="$$root/startup_gate.rs"; dispatch="$$root/startup_gate/active_reblit_complete_finalization.rs"; \
	recovery="$$root/startup_recovery/active_reblit_complete_finalization.rs"; \
	authority="$$root/startup_reconciliation/active_reblit_complete_finalization_authority.rs"; \
	proof="$$root/startup_reconciliation/activation_namespace/active_reblit_commit_cleanup_proof.rs"; \
	capture="$$root/startup_reconciliation/activation_namespace/capture/active_reblit_commit_cleanup.rs"; \
	tests="$$root/startup_gate/usr_rollback_active_reblit/tests/complete_forward_finalization.rs"; \
	grep -Fqx 'mod active_reblit_complete_finalization;' "$$gate"; \
	grep -Fq 'active_reblit_complete_finalization::dispatch(' "$$gate"; \
	grep -Fq 'Dispatch::Finalized { journal }' "$$gate"; \
	grep -Fq 'finalize_active_reblit_complete(journal, authority)' "$$dispatch"; \
	test "$$( grep -Fc '.load_exact_promoted_boot_publication_receipt_state(' "$$authority" )" = 1; \
	test "$$( grep -Fc '.delete_record_binding(' "$$authority" )" = 1; \
	grep -Fq 'revalidate_completed_namespace_after_journal_delete' "$$authority"; \
	grep -Fq 'require_exact_public_journal_absence(installation, journal)?;' "$$proof"; \
	test "$$( grep -Fc 'require_exact_public_journal_absence(installation, journal)?;' "$$proof" )" -ge 2; \
	if rg -n '[.]advance_record_binding|[.]advance\(|retire_promoted_boot_publication_receipt_head|promote_boot_publication_receipt|stage_boot_publication_receipt|reopen_canonical_journal|boot::|run_(transaction|system)_triggers|std::fs::(rename|remove|write|set_permissions)|Command::new|nix::mount|libc::mount' "$$dispatch" "$$recovery" "$$authority"; then exit 1; else status="$$?"; test "$$status" = 1; fi; \
	for file in "$$gate" "$$dispatch" "$$recovery" "$$authority" "$$proof" "$$capture" "$$tests" "$(STARTUP_ACTIVE_REBLIT_COMPLETE_FINALIZATION_TOP_DIR)/misc/make/startup-active-reblit-complete-finalization-tests.mk"; do test "$$( wc -l < "$$file" )" -le 1000; done; \
	$(CARGO) test --manifest-path "$(STARTUP_ACTIVE_REBLIT_COMPLETE_FINALIZATION_TOP_DIR)/Cargo.toml" -p forge --lib "$$prefix" -- --test-threads=1
