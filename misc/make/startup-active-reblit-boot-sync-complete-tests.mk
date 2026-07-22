STARTUP_ACTIVE_REBLIT_BOOT_SYNC_COMPLETE_TOP_DIR := $(abspath $(dir $(lastword $(MAKEFILE_LIST)))/../..)
CARGO ?= cargo

.PHONY: forge-startup-active-reblit-boot-sync-complete-test forge-startup-active-reblit-boot-sync-complete-leaf-test

forge-startup-active-reblit-boot-sync-complete-test: forge-boot-publication-receipt-promotion-test forge-active-reblit-boot-sync-completion-test forge-startup-active-reblit-boot-sync-complete-leaf-test

forge-startup-active-reblit-boot-sync-complete-leaf-test:
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
	dispatch_prefix='client::startup_gate::usr_rollback_active_reblit::tests::boot_sync_complete_startup_dispatch::'; \
	test "$$( grep -Ec "^$$dispatch_prefix.*: test$$" "$$listed" )" = 4; \
	for name in \
		startup_boot_sync_complete_current_and_historical_advance_once_to_exact_commit_decided \
		startup_unpromoted_boot_sync_complete_stays_exactly_pending_and_never_rolls_back \
		startup_legacy_v2_boot_sync_complete_without_receipt_pair_stays_forward_pending \
		startup_stable_active_selection_mismatch_stays_boot_sync_complete_without_rollback; do \
		grep -Fqx "$$dispatch_prefix$$name: test" "$$listed"; \
	done; \
	races_prefix='client::startup_gate::usr_rollback_active_reblit::tests::boot_sync_complete_startup_evidence_races::'; \
	test "$$( grep -Ec "^$$races_prefix.*: test$$" "$$listed" )" = 5; \
	for name in \
		boot_sync_commit_decision_all_six_validation_hooks_reject_same_bytes_on_a_new_inode \
		boot_sync_commit_decision_same_store_rejects_full_state_change_after_advance \
		boot_sync_commit_decision_fresh_binding_rejects_namespace_change_after_reopen \
		startup_boot_sync_complete_database_race_fails_stop_before_any_journal_advance \
		startup_boot_sync_complete_namespace_race_fails_stop_before_any_journal_advance; do \
		grep -Fqx "$$races_prefix$$name: test" "$$listed"; \
	done; \
	faults_prefix='client::startup_gate::usr_rollback_active_reblit::tests::boot_sync_complete_startup_storage_faults::'; \
	test "$$( grep -Ec "^$$faults_prefix.*: test$$" "$$listed" )" = 1; \
	grep -Fqx "$$faults_prefix"'startup_boot_sync_complete_all_five_journal_faults_classify_and_converge_without_false_success: test' "$$listed"; \
	classifier_prefix='client::startup_reconciliation::activation_namespace::active_reblit_boot_sync_complete_proof::classification_tests::'; \
	test "$$( grep -Ec "^$$classifier_prefix.*: test$$" "$$listed" )" = 2; \
	for name in \
		stable_shape_mismatch_may_defer_but_changed_or_post_advance_evidence_does_not \
		missing_namespace_shape_may_defer_but_operational_capture_failure_does_not; do \
		grep -Fqx "$$classifier_prefix$$name: test" "$$listed"; \
	done; \
	root="$(STARTUP_ACTIVE_REBLIT_BOOT_SYNC_COMPLETE_TOP_DIR)/crates/forge/src/client"; \
	gate="$$root/startup_gate.rs"; \
	dispatch="$$root/startup_gate/active_reblit_boot_sync_complete.rs"; \
	recovery="$$root/startup_recovery.rs"; \
	persistence="$$root/startup_recovery/active_reblit_boot_sync_commit_decision.rs"; \
	reconciliation="$$root/startup_reconciliation.rs"; \
	namespace_root="$$root/startup_reconciliation/activation_namespace.rs"; \
	authority="$$root/startup_reconciliation/active_reblit_boot_sync_complete_authority.rs"; \
	proof="$$root/startup_reconciliation/activation_namespace/active_reblit_boot_sync_complete_proof.rs"; \
	tests_root="$$root/startup_gate/usr_rollback_active_reblit/tests"; \
	tests_mod="$$tests_root/mod.rs"; \
	authority_tests="$$tests_root/boot_sync_complete_startup_authority.rs"; \
	support_tests="$$tests_root/boot_sync_complete_support.rs"; \
	dispatch_tests="$$tests_root/boot_sync_complete_startup_dispatch.rs"; \
	races_tests="$$tests_root/boot_sync_complete_startup_evidence_races.rs"; \
	faults_tests="$$tests_root/boot_sync_complete_startup_storage_faults.rs"; \
	grep -Fqx 'mod active_reblit_boot_sync_complete;' "$$gate"; \
	grep -Fqx 'mod active_reblit_boot_sync_commit_decision;' "$$recovery"; \
	grep -Fqx 'mod active_reblit_boot_sync_complete_authority;' "$$reconciliation"; \
	grep -Fqx 'mod active_reblit_boot_sync_complete_proof;' "$$namespace_root"; \
	for module in boot_sync_complete_support boot_sync_complete_startup_authority boot_sync_complete_startup_dispatch boot_sync_complete_startup_evidence_races boot_sync_complete_startup_storage_faults; do \
		grep -Fqx "mod $$module;" "$$tests_mod"; \
	done; \
	grep -Fq 'pub(in crate::client) struct ActiveReblitBootSyncCompleteSeal {' "$$gate"; \
	grep -Fq 'load_exact_promoted_boot_publication_receipt_state' "$$authority"; \
	grep -Fq 'state_db.get(state_id)' "$$authority"; \
	grep -Fq 'ActiveReblitBootSyncCompleteNamespaceInspection::begin(' "$$authority"; \
	grep -Fq 'require_exact_record_binding(installation, journal, &journal_record_binding, record)?;' "$$authority"; \
	test "$$( grep -Fc '.advance_record_binding(cast, journal_record_binding, successor)' "$$authority" )" = 1; \
	test "$$( grep -Fc 'persist_active_reblit_boot_sync_commit_decision_and_reopen(journal, authority)?;' "$$dispatch" )" = 1; \
	test "$$( grep -Fc '.advance_record_binding(&journal, &successor)' "$$persistence" )" = 1; \
	grep -Fq 'ActiveReblitBootSyncCompleteAdmission::Deferred => Ok(Dispatch::Handled { journal, record })' "$$dispatch"; \
	grep -Fq 'ReopenedOldBindingAfterFreshCapture' "$$persistence"; \
	grep -Fq 'after_active_reblit_boot_sync_commit_decision_old_binding_validation();' "$$persistence"; \
	grep -Fq 'let old_binding_revalidation = post_advance_authority.revalidate_successor_reopened(' "$$persistence"; \
	dispatch_line="$$( grep -nF 'match active_reblit_boot_sync_complete::dispatch(' "$$gate" | head -n1 | cut -d: -f1 )"; \
	mutation_line="$$( grep -nF 'ActiveReblitReplacementMutationAuthorityProvider::new(' "$$gate" | head -n1 | cut -d: -f1 )"; \
	root_abi_line="$$( grep -nF 'UsrExchangedRootAbiNormalizationAuthority::capture(' "$$gate" | head -n1 | cut -d: -f1 )"; \
	test -n "$$dispatch_line"; test -n "$$mutation_line"; test -n "$$root_abi_line"; \
	test "$$dispatch_line" -lt "$$mutation_line"; test "$$dispatch_line" -lt "$$root_abi_line"; \
	old_hook_line="$$( grep -nF 'after_active_reblit_boot_sync_commit_decision_old_binding_validation();' "$$persistence" | head -n1 | cut -d: -f1 )"; \
	fresh_capture_line="$$( grep -nF 'let fresh_binding = match recapture_reopened_successor_binding(' "$$persistence" | head -n1 | cut -d: -f1 )"; \
	old_revalidation_line="$$( grep -nF 'let old_binding_revalidation = post_advance_authority.revalidate_successor_reopened(' "$$persistence" | head -n1 | cut -d: -f1 )"; \
	fresh_validation_line="$$( grep -nF 'before_active_reblit_boot_sync_commit_decision_fresh_binding_validation();' "$$persistence" | head -n1 | cut -d: -f1 )"; \
	test "$$old_hook_line" -lt "$$fresh_capture_line"; test "$$fresh_capture_line" -lt "$$old_revalidation_line"; test "$$old_revalidation_line" -lt "$$fresh_validation_line"; \
	if rg -n '[.]advance\(' "$$dispatch" "$$persistence" "$$authority" "$$proof"; then exit 1; else status="$$?"; test "$$status" = 1; fi; \
	if rg -n 'boot::|synchronize_(boot|databases|excluding)|run_(transaction|system)_triggers|finalize_usr|journal[.]delete|delete_record_binding|promote_boot_publication_receipt|stage_boot_publication_receipt|fs::(rename|remove|write|set_permissions)|renameat|unlinkat|Command::new|nix::mount|libc::mount' "$$dispatch" "$$persistence" "$$authority" "$$proof"; then exit 1; else status="$$?"; test "$$status" = 1; fi; \
	if rg --pcre2 -n '#\[derive\([^]]*(?:Clone|Copy)[^]]*\)\]\s*pub\(in crate::client\) struct ActiveReblitBootSyncComplete(?:PostAdvance)?Authority|impl(?:<[^>]+>)?\s+(?:Clone|Copy)\s+for\s+ActiveReblitBootSyncComplete(?:PostAdvance)?Authority' "$$authority"; then exit 1; else status="$$?"; test "$$status" = 1; fi; \
	for file in "$$gate" "$$dispatch" "$$recovery" "$$persistence" "$$reconciliation" "$$namespace_root" "$$authority" "$$proof" "$$tests_mod" "$$authority_tests" "$$support_tests" "$$dispatch_tests" "$$races_tests" "$$faults_tests" "$(STARTUP_ACTIVE_REBLIT_BOOT_SYNC_COMPLETE_TOP_DIR)/misc/make/startup-active-reblit-boot-sync-complete-tests.mk"; do \
		test "$$( wc -l < "$$file" )" -le 1000; \
	done; \
	$(CARGO) test --manifest-path "$(STARTUP_ACTIVE_REBLIT_BOOT_SYNC_COMPLETE_TOP_DIR)/Cargo.toml" -p forge --lib "$$authority_prefix" -- --test-threads=1; \
	$(CARGO) test --manifest-path "$(STARTUP_ACTIVE_REBLIT_BOOT_SYNC_COMPLETE_TOP_DIR)/Cargo.toml" -p forge --lib "$$dispatch_prefix" -- --test-threads=1; \
	$(CARGO) test --manifest-path "$(STARTUP_ACTIVE_REBLIT_BOOT_SYNC_COMPLETE_TOP_DIR)/Cargo.toml" -p forge --lib "$$races_prefix" -- --test-threads=1; \
	$(CARGO) test --manifest-path "$(STARTUP_ACTIVE_REBLIT_BOOT_SYNC_COMPLETE_TOP_DIR)/Cargo.toml" -p forge --lib "$$faults_prefix" -- --test-threads=1; \
	$(CARGO) test --manifest-path "$(STARTUP_ACTIVE_REBLIT_BOOT_SYNC_COMPLETE_TOP_DIR)/Cargo.toml" -p forge --lib "$$classifier_prefix" -- --test-threads=1
