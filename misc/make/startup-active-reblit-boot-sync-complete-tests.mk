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
		startup_boot_sync_complete_advances_one_checkpoint_per_entry \
		startup_unpromoted_boot_sync_complete_stays_exactly_pending_and_never_rolls_back \
		startup_legacy_v2_boot_sync_complete_without_receipt_pair_stays_forward_pending \
		startup_stable_active_selection_mismatch_stays_boot_sync_complete_without_rollback; do \
		grep -Fqx "$$dispatch_prefix$$name: test" "$$listed"; \
	done; \
	started_guard_prefix='client::startup_gate::usr_rollback_active_reblit::tests::boot_sync_started_guard_dispatch::'; \
	test "$$( grep -Ec "^$$started_guard_prefix.*: test$$" "$$listed" )" = 6; \
	for name in \
		startup_promoted_boot_sync_started_enters_recovery_without_rollback \
		startup_exact_pending_boot_sync_started_remains_rollback_eligible \
		startup_legacy_boot_sync_started_remains_rollback_eligible \
		startup_conflicting_pending_receipt_correlation_fails_stop_without_rollback \
		startup_dangling_pending_receipt_body_fails_stop_without_journal_or_rollback_mutation \
		startup_cooperating_writer_cannot_promote_between_pending_guard_and_rollback; do \
		grep -Fqx "$$started_guard_prefix$$name: test" "$$listed"; \
	done; \
	started_authority_prefix='client::startup_gate::usr_rollback_active_reblit::tests::boot_sync_started_startup_authority::'; \
	test "$$( grep -Ec "^$$started_authority_prefix.*: test$$" "$$listed" )" = 4; \
	for name in \
		exact_promoted_chain_plan_state_selection_namespace_and_binding_admit \
		stable_wrong_selection_defers_but_unbound_record_fails_stop \
		database_receipt_chain_and_source_binding_races_fail_stop \
		fresh_namespace_race_fails_full_plan_revalidation_without_effects; do \
		grep -Fqx "$$started_authority_prefix$$name: test" "$$listed"; \
	done; \
	started_post_advance_prefix='client::startup_gate::usr_rollback_active_reblit::tests::boot_sync_started_post_advance_authority::'; \
	test "$$( grep -Ec "^$$started_post_advance_prefix.*: test$$" "$$listed" )" = 2; \
	for name in \
		caller_supplied_non_successor_is_rejected_before_bound_persistence \
		exact_successor_validates_same_store_and_canonical_reopen; do \
		grep -Fqx "$$started_post_advance_prefix$$name: test" "$$listed"; \
	done; \
	started_completion_prefix='client::startup_gate::usr_rollback_active_reblit::tests::boot_sync_started_completion_persistence::'; \
	test "$$( grep -Ec "^$$started_completion_prefix.*: test$$" "$$listed" )" = 3; \
	for name in \
		promoted_boot_sync_started_persists_exact_completion_and_returns_reopened_store \
		promoted_boot_sync_started_all_five_update_faults_reconcile_source_or_successor \
		promoted_boot_sync_started_all_six_binding_checks_reject_same_bytes_on_new_inode; do \
		grep -Fqx "$$started_completion_prefix$$name: test" "$$listed"; \
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
	started_guard_dispatch="$$root/startup_gate/active_reblit_boot_sync_started.rs"; \
	dispatch="$$root/startup_gate/active_reblit_boot_sync_complete.rs"; \
	recovery="$$root/startup_recovery.rs"; \
	persistence="$$root/startup_recovery/active_reblit_boot_sync_commit_decision.rs"; \
	started_persistence="$$root/startup_recovery/active_reblit_boot_sync_started_completion.rs"; \
	reconciliation="$$root/startup_reconciliation.rs"; \
	started_authority="$$root/startup_reconciliation/active_reblit_boot_sync_started_recovery_authority.rs"; \
	started_post_advance="$$root/startup_reconciliation/active_reblit_boot_sync_started_recovery_authority/post_advance.rs"; \
	started_recovery="$$root/startup_gate/active_reblit_boot_sync_started/recovery.rs"; \
	namespace_root="$$root/startup_reconciliation/activation_namespace.rs"; \
	started_proof="$$root/startup_reconciliation/activation_namespace/active_reblit_boot_sync_started_proof.rs"; \
	authority="$$root/startup_reconciliation/active_reblit_boot_sync_complete_authority.rs"; \
	proof="$$root/startup_reconciliation/activation_namespace/active_reblit_boot_sync_complete_proof.rs"; \
	tests_root="$$root/startup_gate/usr_rollback_active_reblit/tests"; \
	tests_mod="$$tests_root/mod.rs"; \
	authority_tests="$$tests_root/boot_sync_complete_startup_authority.rs"; \
	support_tests="$$tests_root/boot_sync_complete_support.rs"; \
	dispatch_tests="$$tests_root/boot_sync_complete_startup_dispatch.rs"; \
	started_guard_tests="$$tests_root/boot_sync_started_guard_dispatch.rs"; \
	started_authority_tests="$$tests_root/boot_sync_started_startup_authority.rs"; \
	started_post_advance_tests="$$tests_root/boot_sync_started_post_advance_authority.rs"; \
	started_persistence_tests="$$tests_root/boot_sync_started_completion_persistence.rs"; \
	races_tests="$$tests_root/boot_sync_complete_startup_evidence_races.rs"; \
	faults_tests="$$tests_root/boot_sync_complete_startup_storage_faults.rs"; \
	grep -Fqx 'mod active_reblit_boot_sync_complete;' "$$gate"; \
	grep -Fqx 'mod active_reblit_boot_sync_started;' "$$gate"; \
	grep -Fqx 'mod active_reblit_boot_sync_commit_decision;' "$$recovery"; \
	grep -Fqx 'mod active_reblit_boot_sync_started_completion;' "$$recovery"; \
	grep -Fqx 'mod active_reblit_boot_sync_complete_authority;' "$$reconciliation"; \
	grep -Fqx 'mod active_reblit_boot_sync_started_recovery_authority;' "$$reconciliation"; \
	grep -Fqx 'pub(in crate::client) mod recovery;' "$$started_guard_dispatch"; \
	grep -Fqx 'mod post_advance;' "$$started_authority"; \
	grep -Fqx 'mod active_reblit_boot_sync_started_proof;' "$$namespace_root"; \
	grep -Fqx 'mod active_reblit_boot_sync_complete_proof;' "$$namespace_root"; \
	for module in boot_sync_complete_support boot_sync_complete_startup_authority boot_sync_complete_startup_dispatch boot_sync_complete_startup_evidence_races boot_sync_complete_startup_storage_faults boot_sync_started_completion_persistence boot_sync_started_guard_dispatch boot_sync_started_post_advance_authority boot_sync_started_startup_authority; do \
		grep -Fqx "mod $$module;" "$$tests_mod"; \
	done; \
	grep -Fq 'load_exact_promoted_boot_publication_receipt_chain' "$$started_authority"; \
	grep -Fq 'PendingReceiptCorrelationMismatch' "$$started_authority"; \
	grep -Fq 'ActiveReblitBootSyncStartedRecoveryAdmission::Ready(authority) =>' "$$started_guard_dispatch"; \
	grep -Fq 'pub(in crate::client) struct ActiveReblitBootSyncStartedCleanupSeal {' "$$gate"; \
	grep -Fq 'pub(in crate::client) fn new_for_test(' "$$gate"; \
	grep -Fq 'pub(in crate::client) const fn promoted_receipt(' "$$gate"; \
	grep -Fq 'ActiveReblitBootSyncStartedNamespaceInspection::begin(' "$$started_authority"; \
	grep -Fq '.prepare_active_reblit_promoted_boot_cleanup_plan()?' "$$started_authority"; \
	grep -Fq 'require_exact_record_binding(' "$$started_authority"; \
	grep -Fq '.has_reopened_record_binding(cast, &self.binding, &self.record)' "$$started_guard_tests"; \
	grep -Fq '.delete_boot_publication_receipt_body_for_test(pair.pending);' "$$started_guard_tests"; \
	grep -Fq 'arm_between_usr_rollback_decision_database_captures(move || {' "$$started_guard_tests"; \
	grep -Fq 'let reservation = ActiveStateReservation::acquire().unwrap();' "$$started_guard_tests"; \
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
	test "$$( grep -Fc 'reconcile_and_cleanup_restart_receipt_entry(' "$$started_recovery" )" = 1; \
	test "$$( grep -Fc 'persist_active_reblit_boot_sync_started_completion_and_reopen(' "$$started_recovery" )" = 1; \
	grep -Fq 'ActiveReblitPromotedBootCleanupDisposition::NoOp' "$$started_recovery"; \
	grep -Fq 'ActiveReblitPromotedBootCleanupDisposition::PreserveUnownedStale' "$$started_recovery"; \
	grep -Fq 'let post_cleanup_authority = authority.revalidate(&journal);' "$$started_recovery"; \
	test "$$( grep -Fc '.boot_sync_complete_successor(pair)' "$$started_persistence" )" = 1; \
	test "$$( grep -Fc '.advance_record_binding(&journal, &successor)' "$$started_persistence" )" = 1; \
	grep -Fq 'reopen_canonical_journal(&installation)' "$$started_persistence"; \
	grep -Fq 'ReopenedOldBindingAfterFreshCapture' "$$started_persistence"; \
	grep -Fq 'let old_binding_revalidation =' "$$started_persistence"; \
	grep -Fq 'post_advance_authority.revalidate_successor_reopened(' "$$started_persistence"; \
	test "$$( grep -Fc '.advance_record_binding(cast, journal_record_binding, successor)' "$$started_post_advance" )" = 1; \
	grep -Fq 'pub(in crate::client) fn revalidate_successor_same_store(' "$$started_post_advance"; \
	grep -Fq 'pub(in crate::client) fn revalidate_successor_reopened(' "$$started_post_advance"; \
	grep -Fq 'journal.has_reopened_record_binding(cast, binding, successor)?' "$$started_post_advance"; \
	started_guard_line="$$( grep -nF 'match active_reblit_boot_sync_started::dispatch(' "$$gate" | head -n1 | cut -d: -f1 )"; \
	dispatch_line="$$( grep -nF 'match active_reblit_boot_sync_complete::dispatch(' "$$gate" | head -n1 | cut -d: -f1 )"; \
	mutation_line="$$( grep -nF 'ActiveReblitReplacementMutationAuthorityProvider::new(' "$$gate" | head -n1 | cut -d: -f1 )"; \
	root_abi_line="$$( grep -nF 'UsrExchangedRootAbiNormalizationAuthority::capture(' "$$gate" | head -n1 | cut -d: -f1 )"; \
	test -n "$$started_guard_line"; test -n "$$dispatch_line"; test -n "$$mutation_line"; test -n "$$root_abi_line"; \
	test "$$started_guard_line" -lt "$$dispatch_line"; \
	test "$$dispatch_line" -lt "$$mutation_line"; test "$$dispatch_line" -lt "$$root_abi_line"; \
	old_hook_line="$$( grep -nF 'after_active_reblit_boot_sync_commit_decision_old_binding_validation();' "$$persistence" | head -n1 | cut -d: -f1 )"; \
	fresh_capture_line="$$( grep -nF 'let fresh_binding = match recapture_reopened_successor_binding(' "$$persistence" | head -n1 | cut -d: -f1 )"; \
	old_revalidation_line="$$( grep -nF 'let old_binding_revalidation = post_advance_authority.revalidate_successor_reopened(' "$$persistence" | head -n1 | cut -d: -f1 )"; \
	fresh_validation_line="$$( grep -nF 'before_active_reblit_boot_sync_commit_decision_fresh_binding_validation();' "$$persistence" | head -n1 | cut -d: -f1 )"; \
	test "$$old_hook_line" -lt "$$fresh_capture_line"; test "$$fresh_capture_line" -lt "$$old_revalidation_line"; test "$$old_revalidation_line" -lt "$$fresh_validation_line"; \
	if rg -n '[.]advance\(' "$$dispatch" "$$persistence" "$$authority" "$$proof"; then exit 1; else status="$$?"; test "$$status" = 1; fi; \
	if rg -n '[.](advance|advance_record_binding)\(' "$$started_guard_dispatch" "$$started_authority" "$$started_proof"; then exit 1; else status="$$?"; test "$$status" = 1; fi; \
	if rg -n '[.]advance\(' "$$started_recovery" "$$started_persistence" "$$started_post_advance"; then exit 1; else status="$$?"; test "$$status" = 1; fi; \
	if rg -n 'boot::|synchronize_(boot|databases|excluding)|run_(transaction|system)_triggers\(|finalize_usr|journal[.]delete|delete_record_binding|promote_boot_publication_receipt|stage_boot_publication_receipt|fs::(rename|remove|write|set_permissions)|renameat|unlinkat|Command::new|nix::mount|libc::mount' "$$started_guard_dispatch" "$$started_authority" "$$started_proof" "$$started_recovery" "$$started_persistence" "$$started_post_advance" "$$dispatch" "$$persistence" "$$authority" "$$proof"; then exit 1; else status="$$?"; test "$$status" = 1; fi; \
	if rg --pcre2 -n '#\[derive\([^]]*(?:Clone|Copy)[^]]*\)\]\s*pub\(in crate::client\) struct ActiveReblitBootSyncComplete(?:PostAdvance)?Authority|impl(?:<[^>]+>)?\s+(?:Clone|Copy)\s+for\s+ActiveReblitBootSyncComplete(?:PostAdvance)?Authority' "$$authority"; then exit 1; else status="$$?"; test "$$status" = 1; fi; \
	if rg --pcre2 -n '#\[derive\([^]]*(?:Clone|Copy)[^]]*\)\]\s*pub\(in crate::client\) struct ActiveReblitBootSyncStarted(?:Recovery|PostAdvance)Authority|impl(?:<[^>]+>)?\s+(?:Clone|Copy)\s+for\s+ActiveReblitBootSyncStarted(?:Recovery|PostAdvance)Authority' "$$started_authority" "$$started_post_advance"; then exit 1; else status="$$?"; test "$$status" = 1; fi; \
	for file in "$$gate" "$$started_guard_dispatch" "$$started_recovery" "$$dispatch" "$$recovery" "$$started_persistence" "$$persistence" "$$reconciliation" "$$started_authority" "$$started_post_advance" "$$namespace_root" "$$started_proof" "$$authority" "$$proof" "$$tests_mod" "$$authority_tests" "$$support_tests" "$$dispatch_tests" "$$started_guard_tests" "$$started_authority_tests" "$$started_post_advance_tests" "$$started_persistence_tests" "$$races_tests" "$$faults_tests" "$(STARTUP_ACTIVE_REBLIT_BOOT_SYNC_COMPLETE_TOP_DIR)/misc/make/startup-active-reblit-boot-sync-complete-tests.mk"; do \
		test "$$( wc -l < "$$file" )" -le 1000; \
	done; \
	$(CARGO) test --manifest-path "$(STARTUP_ACTIVE_REBLIT_BOOT_SYNC_COMPLETE_TOP_DIR)/Cargo.toml" -p forge --lib "$$authority_prefix" -- --test-threads=1; \
	$(CARGO) test --manifest-path "$(STARTUP_ACTIVE_REBLIT_BOOT_SYNC_COMPLETE_TOP_DIR)/Cargo.toml" -p forge --lib "$$dispatch_prefix" -- --test-threads=1; \
	$(CARGO) test --manifest-path "$(STARTUP_ACTIVE_REBLIT_BOOT_SYNC_COMPLETE_TOP_DIR)/Cargo.toml" -p forge --lib "$$started_guard_prefix" -- --test-threads=1; \
	$(CARGO) test --manifest-path "$(STARTUP_ACTIVE_REBLIT_BOOT_SYNC_COMPLETE_TOP_DIR)/Cargo.toml" -p forge --lib "$$started_authority_prefix" -- --test-threads=1; \
	$(CARGO) test --manifest-path "$(STARTUP_ACTIVE_REBLIT_BOOT_SYNC_COMPLETE_TOP_DIR)/Cargo.toml" -p forge --lib "$$started_post_advance_prefix" -- --test-threads=1; \
	$(CARGO) test --manifest-path "$(STARTUP_ACTIVE_REBLIT_BOOT_SYNC_COMPLETE_TOP_DIR)/Cargo.toml" -p forge --lib "$$started_completion_prefix" -- --test-threads=1; \
	$(CARGO) test --manifest-path "$(STARTUP_ACTIVE_REBLIT_BOOT_SYNC_COMPLETE_TOP_DIR)/Cargo.toml" -p forge --lib "$$races_prefix" -- --test-threads=1; \
	$(CARGO) test --manifest-path "$(STARTUP_ACTIVE_REBLIT_BOOT_SYNC_COMPLETE_TOP_DIR)/Cargo.toml" -p forge --lib "$$faults_prefix" -- --test-threads=1; \
	$(CARGO) test --manifest-path "$(STARTUP_ACTIVE_REBLIT_BOOT_SYNC_COMPLETE_TOP_DIR)/Cargo.toml" -p forge --lib "$$classifier_prefix" -- --test-threads=1
