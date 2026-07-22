STARTUP_ACTIVE_REBLIT_BOOT_SYNC_COMPLETE_TOP_DIR := $(abspath $(dir $(lastword $(MAKEFILE_LIST)))/../..)
CARGO ?= cargo

.PHONY: forge-startup-active-reblit-boot-sync-complete-test

forge-startup-active-reblit-boot-sync-complete-test: forge-boot-publication-receipt-promotion-test forge-active-reblit-boot-sync-completion-test
	@set -euo pipefail; \
	mkdir -p "$(STARTUP_ACTIVE_REBLIT_BOOT_SYNC_COMPLETE_TOP_DIR)/target"; \
	listed="$$( mktemp "$(STARTUP_ACTIVE_REBLIT_BOOT_SYNC_COMPLETE_TOP_DIR)/target/startup-active-reblit-boot-sync-complete-list.XXXXXXXXXXXX" )"; \
	trap 'rm -f "$$listed"' EXIT; \
	$(CARGO) test --manifest-path "$(STARTUP_ACTIVE_REBLIT_BOOT_SYNC_COMPLETE_TOP_DIR)/Cargo.toml" -p forge --lib -- --list | tee "$$listed" >/dev/null; \
	test -s "$$listed"; \
	authority_prefix='client::startup_gate::usr_rollback_active_reblit::tests::boot_sync_complete_startup_authority::'; \
	test "$$( grep -Ec "^$$authority_prefix.*: test$$" "$$listed" )" = 6; \
	for name in \
		exact_promoted_receipt_full_state_selection_and_source_binding_admit \
		stable_wrong_selection_defers_but_malformed_target_fails_stop \
		database_and_source_binding_races_fail_stop_instead_of_deferring \
		fresh_namespace_race_fails_revalidation_without_journal_advance \
		caller_supplied_non_successor_is_rejected_before_bound_persistence \
		post_advance_evidence_validates_same_store_and_canonical_reopen; do \
		grep -Fqx "$$authority_prefix$$name: test" "$$listed"; \
	done; \
	classifier_prefix='client::startup_reconciliation::activation_namespace::active_reblit_boot_sync_complete_proof::classification_tests::'; \
	test "$$( grep -Ec "^$$classifier_prefix.*: test$$" "$$listed" )" = 2; \
	for name in \
		stable_shape_mismatch_may_defer_but_changed_or_post_advance_evidence_does_not \
		missing_namespace_shape_may_defer_but_operational_capture_failure_does_not; do \
		grep -Fqx "$$classifier_prefix$$name: test" "$$listed"; \
	done; \
	root="$(STARTUP_ACTIVE_REBLIT_BOOT_SYNC_COMPLETE_TOP_DIR)/crates/forge/src/client"; \
	gate="$$root/startup_gate.rs"; \
	reconciliation="$$root/startup_reconciliation.rs"; \
	namespace_root="$$root/startup_reconciliation/activation_namespace.rs"; \
	authority="$$root/startup_reconciliation/active_reblit_boot_sync_complete_authority.rs"; \
	proof="$$root/startup_reconciliation/activation_namespace/active_reblit_boot_sync_complete_proof.rs"; \
	tests="$$root/startup_gate/usr_rollback_active_reblit/tests/boot_sync_complete_startup_authority.rs"; \
	grep -Fqx 'mod active_reblit_boot_sync_complete_authority;' "$$reconciliation"; \
	grep -Fqx 'mod active_reblit_boot_sync_complete_proof;' "$$namespace_root"; \
	grep -Fqx 'mod boot_sync_complete_startup_authority;' "$$root/startup_gate/usr_rollback_active_reblit/tests/mod.rs"; \
	grep -Fq 'pub(in crate::client) struct ActiveReblitBootSyncCompleteSeal {' "$$gate"; \
	grep -Fq 'load_exact_promoted_boot_publication_receipt_state' "$$authority"; \
	grep -Fq 'state_db.get(state_id)' "$$authority"; \
	grep -Fq 'ActiveReblitBootSyncCompleteNamespaceInspection::begin(' "$$authority"; \
	grep -Fq 'require_exact_record_binding(installation, journal, &journal_record_binding, record)?;' "$$authority"; \
	test "$$( grep -Fc '.advance_record_binding(cast, journal_record_binding, successor)' "$$authority" )" = 1; \
	if rg -n '[.]advance\(' "$$authority" "$$proof"; then exit 1; else status="$$?"; test "$$status" = 1; fi; \
	if rg -n 'boot::|synchronize_(boot|databases|excluding)|run_(transaction|system)_triggers|finalize_usr|journal[.]delete|delete_record_binding|promote_boot_publication_receipt|stage_boot_publication_receipt|fs::(rename|remove|write|set_permissions)|renameat|unlinkat|Command::new|nix::mount|libc::mount' "$$authority" "$$proof"; then exit 1; else status="$$?"; test "$$status" = 1; fi; \
	if rg --pcre2 -n '#\[derive\([^]]*(?:Clone|Copy)[^]]*\)\]\s*pub\(in crate::client\) struct ActiveReblitBootSyncComplete(?:PostAdvance)?Authority|impl(?:<[^>]+>)?\s+(?:Clone|Copy)\s+for\s+ActiveReblitBootSyncComplete(?:PostAdvance)?Authority' "$$authority"; then exit 1; else status="$$?"; test "$$status" = 1; fi; \
	for file in "$$gate" "$$reconciliation" "$$namespace_root" "$$authority" "$$proof" "$$tests" "$(STARTUP_ACTIVE_REBLIT_BOOT_SYNC_COMPLETE_TOP_DIR)/misc/make/startup-active-reblit-boot-sync-complete-tests.mk"; do \
		test "$$( wc -l < "$$file" )" -le 1000; \
	done; \
	$(CARGO) test --manifest-path "$(STARTUP_ACTIVE_REBLIT_BOOT_SYNC_COMPLETE_TOP_DIR)/Cargo.toml" -p forge --lib "$$authority_prefix" -- --test-threads=1; \
	$(CARGO) test --manifest-path "$(STARTUP_ACTIVE_REBLIT_BOOT_SYNC_COMPLETE_TOP_DIR)/Cargo.toml" -p forge --lib "$$classifier_prefix" -- --test-threads=1
