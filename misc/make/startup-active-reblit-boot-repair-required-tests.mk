.PHONY: forge-startup-active-reblit-boot-repair-required-test

forge-startup-active-reblit-boot-repair-required-test: \
	forge-boot-publication-receipt-state-test \
	forge-startup-usr-rollback-decision-test \
	forge-startup-usr-rollback-resume-route-test \
	forge-startup-usr-rollback-reverse-admission-test \
	forge-startup-usr-rollback-candidate-preserve-admission-test \
	forge-startup-usr-rollback-active-reblit-complete-route-test
	@set -euo pipefail; \
	timeout 10s mkdir -p "$(TOP_DIR)/target"; \
	listed="$$( timeout 10s mktemp "$(TOP_DIR)/target/active-reblit-boot-required-list.XXXXXXXXXXXX" )"; \
	trap 'timeout 10s rm -f "$$listed"' EXIT; \
	timeout 300s $(CARGO) test -p forge --lib -- --list | timeout 300s tee "$$listed" >/dev/null; \
	timeout 10s grep -q . "$$listed"; \
	prefix='client::startup_gate::usr_rollback_active_reblit::tests::'; \
	timeout 10s test "$$( timeout 10s grep -c "^$$prefix"'boot_repair_required_.*: test$$' "$$listed" )" = 9; \
	for name in \
		boot_repair_required_authority_binding::startup_active_reblit_boot_repair_required_authority_rejects_reopened_and_cross_root_journal_bindings \
		boot_repair_required_evidence_races::startup_active_reblit_boot_repair_required_rejects_database_provenance_journal_and_namespace_races \
		boot_repair_required_exclusions::startup_active_reblit_boot_repair_required_rejects_each_inexact_plan_field \
		boot_repair_required_exclusions::startup_active_reblit_boot_repair_required_physical_pre_layout_defers_without_mutation \
		boot_repair_required_matrix::startup_active_reblit_boot_repair_required_covers_current_historical_applied_and_already_satisfied \
		boot_repair_required_prefix_boundaries::startup_active_reblit_boot_repair_required_prefix_boundaries_are_exact_and_effect_free \
		boot_repair_required_receipt_correlation::startup_active_reblit_boot_repair_required_requires_exact_receipts_and_preserves_legacy_route \
		boot_repair_required_receipt_correlation::startup_active_reblit_boot_repair_required_rejects_receipt_races_and_corruption_without_effects \
		boot_repair_required_storage_faults::startup_active_reblit_boot_repair_required_all_five_journal_faults_converge_fresh_without_boot; do \
		timeout 10s grep -Fqx "$$prefix$$name: test" "$$listed"; \
	done; \
	gate=crates/forge/src/client/startup_gate.rs; \
	orchestrator=crates/forge/src/client/startup_gate/usr_rollback_active_reblit.rs; \
	authority=crates/forge/src/client/startup_reconciliation/usr_rollback_active_reblit_boot_repair_required_authority.rs; \
	decision_authority=crates/forge/src/client/startup_reconciliation/usr_rollback_decision_authority.rs; \
	resume_authority=crates/forge/src/client/startup_reconciliation/usr_rollback_resume_route_authority.rs; \
	reverse_authority=crates/forge/src/client/startup_reconciliation/usr_rollback_reverse_authority.rs; \
	candidate_authority=crates/forge/src/client/startup_reconciliation/usr_rollback_candidate_preserve_authority.rs; \
	reconciliation_root=crates/forge/src/client/startup_reconciliation.rs; \
	namespace_root=crates/forge/src/client/startup_reconciliation/activation_namespace.rs; \
	recovery_root=crates/forge/src/client/startup_recovery.rs; \
	focused_exports=crates/forge/src/client/startup_reconciliation/focused_test_exports.rs; \
	proof=crates/forge/src/client/startup_reconciliation/activation_namespace/active_reblit_boot_repair_required_proof.rs; \
	executor=crates/forge/src/client/startup_recovery/usr_rollback_active_reblit_boot_repair_required.rs; \
	complete_authority=crates/forge/src/client/startup_reconciliation/usr_rollback_active_reblit_complete_route_authority.rs; \
	fixture=crates/forge/src/client/startup_recovery/test_support.rs; \
	boot_worker=crates/forge/src/client/boot.rs; \
	tests=crates/forge/src/client/startup_gate/usr_rollback_active_reblit/tests; \
	for module in boot_repair_required_authority_binding boot_repair_required_evidence_races boot_repair_required_exclusions boot_repair_required_matrix boot_repair_required_prefix_boundaries boot_repair_required_receipt_correlation boot_repair_required_storage_faults; do \
		timeout 10s grep -Fqx "mod $$module;" "$$tests/mod.rs"; \
	done; \
	timeout 10s grep -Fqx 'mod usr_rollback_active_reblit_boot_repair_required_authority;' "$$reconciliation_root"; \
	timeout 10s grep -Fq 'pub(in crate::client) use usr_rollback_active_reblit_boot_repair_required_authority::{' "$$reconciliation_root"; \
	timeout 10s grep -Fqx 'mod active_reblit_boot_repair_required_proof;' "$$namespace_root"; \
	timeout 10s grep -Fq 'pub(super) use active_reblit_boot_repair_required_proof::{' "$$namespace_root"; \
	timeout 10s grep -Fqx 'mod usr_rollback_active_reblit_boot_repair_required;' "$$recovery_root"; \
	timeout 10s grep -Fq 'pub(super) use usr_rollback_active_reblit_boot_repair_required::{' "$$recovery_root"; \
	for predicate in usr_rollback_decision_source_is_supported_for_test usr_rollback_resume_route_plan_is_exact_for_test usr_rollback_reverse_plan_is_exact_for_test usr_rollback_candidate_preserve_plan_is_exact_for_test; do \
		timeout 10s grep -Fq "$$predicate" "$$focused_exports"; \
	done; \
	timeout 10s test "$$( timeout 10s rg -n '^#\[test\]$$' "$$tests"/boot_repair_required_*.rs | timeout 10s wc -l )" = 9; \
	timeout 10s grep -Fq 'UsrRollbackActiveReblitBootRepairRequiredSeal,' "$$gate"; \
	timeout 10s grep -Fqx 'pub(in crate::client) struct UsrRollbackActiveReblitBootRepairRequiredSeal {' "$$orchestrator"; \
	timeout 10s test "$$( timeout 10s grep -Fc 'UsrRollbackActiveReblitBootRepairRequiredAuthority::capture(' "$$orchestrator" )" = 1; \
	timeout 10s test "$$( timeout 10s grep -Fc 'persist_usr_rollback_active_reblit_boot_repair_required_and_reopen(journal, authority)?' "$$orchestrator" )" = 1; \
	boot_line="$$( timeout 10s grep -nF 'UsrRollbackActiveReblitBootRepairRequiredAuthority::capture(' "$$orchestrator" | timeout 10s cut -d: -f1 )"; \
	complete_line="$$( timeout 10s grep -nF 'UsrRollbackActiveReblitCompleteRouteAuthority::capture(' "$$orchestrator" | timeout 10s cut -d: -f1 )"; \
	timeout 10s test "$$boot_line" -lt "$$complete_line"; \
	candidate_arm="$$( timeout 10s sed -n '/Phase::CandidatePreserved => {/,/Phase::RollbackComplete => {/p' "$$orchestrator" | timeout 10s sed '$$d' )"; \
	timeout 10s test "$$( timeout 10s grep -Fc 'return Ok(Dispatch::Handled { journal, record });' <<<"$$candidate_arm" )" = 1; \
	boot_persist_line="$$( timeout 10s grep -nF 'persist_usr_rollback_active_reblit_boot_repair_required_and_reopen(journal, authority)?' <<<"$$candidate_arm" | timeout 10s cut -d: -f1 )"; \
	boot_return_line="$$( timeout 10s grep -nF 'return Ok(Dispatch::Handled { journal, record });' <<<"$$candidate_arm" | timeout 10s cut -d: -f1 )"; \
	ordinary_capture_line="$$( timeout 10s grep -nF 'UsrRollbackActiveReblitCompleteRouteAuthority::capture(' <<<"$$candidate_arm" | timeout 10s cut -d: -f1 )"; \
	timeout 10s test "$$boot_persist_line" -lt "$$boot_return_line"; \
	timeout 10s test "$$boot_return_line" -lt "$$ordinary_capture_line"; \
	if timeout 10s rg -n 'boot::|synchronize_boot|synchronize_databases|synchronize_excluding|run_(transaction|system)_triggers|finalize_usr_rollback|journal\.delete|stage_boot_publication_receipt(_pair)?' <<<"$$candidate_arm"; then exit 1; else status="$$?"; timeout 10s test "$$status" = 1; fi; \
	timeout 10s grep -Fq 'record.operation != Operation::ActiveReblit || record.phase != Phase::CandidatePreserved' "$$authority"; \
	timeout 10s grep -Fq 'rollback.source == ForwardPhase::BootSyncStarted' "$$authority"; \
	timeout 10s grep -Fq 'rollback.previous_archive == RollbackAction::NotRequired' "$$authority"; \
	timeout 10s test "$$( timeout 10s grep -Fc 'RollbackAction::Applied | RollbackAction::AlreadySatisfied' "$$authority" )" = 2; \
	timeout 10s grep -Fq 'rollback.candidate.disposition == AbortDisposition::Quarantine' "$$authority"; \
	timeout 10s grep -Fq 'rollback.fresh_db == RollbackAction::NotRequired' "$$authority"; \
	timeout 10s grep -Fq 'rollback.boot == BootRollback::PendingUnverifiable' "$$authority"; \
	timeout 10s grep -Fq 'rollback.external_effects_may_remain' "$$authority"; \
	timeout 10s grep -Fq 'record.operation == Operation::ActiveReblit && record.phase == Phase::BootSyncStarted' "$$decision_authority"; \
	timeout 10s grep -Fq '(Phase::BootSyncStarted, UsrExchangeLayout::Post) if active_reblit_boot_sync' "$$decision_authority"; \
	timeout 10s grep -Fq '(Phase::BootSyncStarted, UsrExchangeLayout::Pre) if active_reblit_boot_sync' "$$decision_authority"; \
	for prefix_authority in "$$resume_authority" "$$reverse_authority" "$$candidate_authority"; do \
		timeout 10s grep -Fq 'record.operation == Operation::ActiveReblit && rollback.source == ForwardPhase::BootSyncStarted' "$$prefix_authority"; \
		timeout 10s grep -Fq 'BootRollback::PendingUnverifiable' "$$prefix_authority"; \
		timeout 10s grep -Fq 'BootRollback::NotRequired' "$$prefix_authority"; \
	done; \
	timeout 10s grep -Fq 'let journal_binding = journal.binding();' "$$authority"; \
	timeout 10s grep -Fq 'if !journal.has_binding(&journal_binding)' "$$authority"; \
	timeout 10s grep -Fq 'let database_before = match inspect_current_database(record, state_db)? {' "$$authority"; \
	timeout 10s grep -Fq 'let database_after = match inspect_current_database(record, state_db)? {' "$$authority"; \
	grep -Fq 'let receipt_state = state_db.boot_publication_receipt_state()?;' "$$authority"; \
	grep -Fq 'record.boot_publication_receipt_correlation()?' "$$authority"; \
	grep -Fq 'receipt_state.receipt_pair_for(&record.transition_id) == Some(pair)' "$$authority"; \
	grep -Fq 'None if receipt_state.head().pending().is_none()' "$$authority"; \
	if rg -n 'boot_publication_receipt_head\(\)' "$$authority"; then exit 1; else status="$$?"; test "$$status" = 1; fi; \
	grep -Fq 'ReadyAuthenticated' "$$authority"; \
	grep -Fq 'ReadyLegacyUnverified' "$$authority"; \
	timeout 10s grep -Fq 'UsrRollbackActiveReblitBootRepairRequiredNamespaceInspection::begin(installation, journal, record)' "$$authority"; \
	timeout 10s grep -Fq 'database_before != database_after' "$$authority"; \
	timeout 10s grep -Fq 'database_ownership_evidence_compatible(record, evidence)' "$$authority"; \
	timeout 10s grep -Fq 'metadata_provenance_evidence_compatible(record, evidence)' "$$authority"; \
	timeout 10s grep -Fq 'DatabaseEvidence::ExistingCandidate {' "$$authority"; \
	timeout 10s grep -Fq 'provenance: Some(_),' "$$authority"; \
	timeout 10s grep -Fq 'existing.ownership == db::state::TransitionOwnership::Cleared' "$$authority"; \
	timeout 10s grep -Fq 'require_exact_active_reblit_candidate_preserved_topology(expected, &before)?' "$$proof"; \
	timeout 10s grep -Fq 'require_exact_wrapper_index(expected, &fresh, self.wrapper_index)?;' "$$proof"; \
	timeout 10s grep -Fq 'run_before_fresh_namespace_capture();' "$$proof"; \
	timeout 10s test "$$( timeout 10s grep -Fc 'authority.revalidate(&journal)' "$$executor" )" = 2; \
	timeout 10s test "$$( timeout 10s grep -Fc 'source_record.rollback_successor(None)' "$$executor" )" = 1; \
	timeout 10s test "$$( timeout 10s grep -Fc 'journal.advance(&source_record, &successor)' "$$executor" )" = 1; \
	timeout 10s grep -Fq 'successor.phase == Phase::BootRepairRequired' "$$executor"; \
	handoff="$$( timeout 10s sed -n '/Canonical reopen begins only after/,/reopen_canonical_journal/p' "$$executor" )"; \
	drop_authority_line="$$( timeout 10s grep -nF 'drop(authority);' <<<"$$handoff" | timeout 10s cut -d: -f1 )"; \
	drop_journal_line="$$( timeout 10s grep -nF 'drop(journal);' <<<"$$handoff" | timeout 10s cut -d: -f1 )"; \
	reopen_line="$$( timeout 10s grep -nF 'reopen_canonical_journal(&installation)' <<<"$$handoff" | timeout 10s cut -d: -f1 )"; \
	timeout 10s test "$$drop_authority_line" -lt "$$drop_journal_line"; \
	timeout 10s test "$$drop_journal_line" -lt "$$reopen_line"; \
	if timeout 10s rg -n '^[[:space:]]*(loop|while)[[:space:]]|^[[:space:]]*for[[:space:]].*[[:space:]]in[[:space:]]|boot::|synchronize_boot|synchronize_databases|synchronize_excluding|run_(transaction|system)_triggers|finalize_usr_rollback|journal\.delete|stage_boot_publication_receipt(_pair)?|remove_exact_fresh_transition|renameat|unlink|mkdir|create_dir|set_permissions|chmod|Phase::BootRepair(Started|Unverified)' "$$authority" "$$proof" "$$executor"; then exit 1; else status="$$?"; timeout 10s test "$$status" = 1; fi; \
	if timeout 10s rg -n '#\[derive\([^]]*Clone' "$$authority" "$$proof"; then exit 1; else status="$$?"; timeout 10s test "$$status" = 1; fi; \
	timeout 10s grep -Fq 'ForwardPhase::UsrExchangeIntent | ForwardPhase::UsrExchanged | ForwardPhase::RootLinksComplete' "$$complete_authority"; \
	timeout 10s grep -Fq 'rollback.boot == BootRollback::NotRequired' "$$complete_authority"; \
	if timeout 10s rg -n 'rollback\.source == ForwardPhase::BootSyncStarted' "$$complete_authority"; then exit 1; else status="$$?"; timeout 10s test "$$status" = 1; fi; \
	timeout 10s grep -Fq 'assert_completion_refused(&fixture.fixture, &journal, &reservation, &exact);' "$$tests/boot_repair_required_exclusions.rs"; \
	for source in BootSyncComplete TransactionTriggersComplete SystemTriggersComplete UsrExchangeIntent UsrExchanged; do timeout 10s grep -Fq "ForwardPhase::$$source" "$$tests/boot_repair_required_exclusions.rs"; done; \
	for boot in NotRequired Unverified; do timeout 10s grep -Fq "BootRollback::$$boot" "$$tests/boot_repair_required_exclusions.rs"; done; \
	timeout 10s grep -Fq 'for epoch in Epoch::ALL {' "$$tests/boot_repair_required_matrix.rs"; \
	timeout 10s grep -Fq 'for usr_outcome in UsrRestoreOrigin::ALL {' "$$tests/boot_repair_required_matrix.rs"; \
	timeout 10s grep -Fq 'for candidate_outcome in CandidateOrigin::ALL {' "$$tests/boot_repair_required_matrix.rs"; \
	timeout 10s grep -Fq 'build_boot_sync_started(epoch, BootSyncStartedLayout::Post)' "$$tests/boot_repair_required_matrix.rs"; \
	timeout 10s grep -Fq 'assert_eq!(fixture.source.generation, 11);' "$$tests/support.rs"; \
	timeout 10s grep -Fq 'Phase::BootSyncStarted' "$$fixture"; \
	grep -Fq 'let receipts = stage_test_boot_publication_receipts(' "$$fixture"; \
	grep -Fq 'database.stage_boot_publication_receipt(&pending).unwrap();' "$$fixture"; \
	grep -Fq 'record.boot_sync_started_successor(receipts)' "$$fixture"; \
	receipt_tests="$$tests/boot_repair_required_receipt_correlation.rs"; \
	for mismatch in MissingPending WrongTransition WrongPending WrongCommitted; do grep -Fq "ReceiptMismatch::$$mismatch" "$$receipt_tests"; done; \
	grep -Fq 'ReadyLegacyUnverified' "$$receipt_tests"; \
	grep -Fq 'arm_between_usr_rollback_active_reblit_boot_repair_required_database_captures' "$$receipt_tests"; \
	grep -Fq 'arm_before_usr_rollback_active_reblit_boot_repair_required_final_revalidation' "$$receipt_tests"; \
	grep -Fq 'replace_boot_publication_receipt_head_raw_for_test' "$$receipt_tests"; \
	grep -Fq 'delete_boot_publication_receipt_head_for_test' "$$receipt_tests"; \
	grep -Fq 'delete_boot_publication_receipt_body_for_test' "$$receipt_tests"; \
	prefix_boundaries="$$tests/boot_repair_required_prefix_boundaries.rs"; \
	timeout 10s grep -Fq 'build_boot_sync_started(Epoch::Current, BootSyncStartedLayout::Post)' "$$prefix_boundaries"; \
	timeout 10s grep -Fq 'build_boot_sync_started(Epoch::Historical, BootSyncStartedLayout::Pre)' "$$prefix_boundaries"; \
	timeout 10s grep -Fq 'assert!(matches!(post_admission, UsrRollbackDecisionAdmission::Ready(_)));' "$$prefix_boundaries"; \
	timeout 10s grep -Fq 'assert!(matches!(pre_admission, UsrRollbackDecisionAdmission::Deferred(_)));' "$$prefix_boundaries"; \
	timeout 10s test "$$( timeout 10s grep -Fc 'assert_resume_ready(&fixture,' "$$prefix_boundaries" )" = 2; \
	timeout 10s grep -Fq 'assert_reverse_admission(&fixture, &reverse, true);' "$$prefix_boundaries"; \
	timeout 10s grep -Fq 'assert_reverse_admission(&fixture, &reverse, false);' "$$prefix_boundaries"; \
	timeout 10s grep -Fq 'assert_candidate_admission(&fixture, &candidate_intent, false);' "$$prefix_boundaries"; \
	timeout 10s grep -Fq 'assert_candidate_admission(&fixture, &candidate_intent, true);' "$$prefix_boundaries"; \
	timeout 10s test "$$( timeout 10s grep -Fc 'assert_eq!(fixture.fixture.namespace_snapshot(), namespace_before);' "$$prefix_boundaries" )" = 4; \
	timeout 10s test "$$( timeout 10s grep -Fc 'assert_eq!(active_reblit_candidate_preserve_exchange_attempt_count(), 0);' "$$prefix_boundaries" )" = 2; \
	timeout 10s test "$$( timeout 10s grep -Fc 'Fixture::boot_sync_started(kind, BootSyncStartedLayout::Post, false)' "$$prefix_boundaries" )" = 2; \
	timeout 10s grep -Fq 'for operation in [Operation::NewState, Operation::ActivateArchived] {' "$$prefix_boundaries"; \
	timeout 10s grep -Fq 'wrong_operation.operation = operation;' "$$prefix_boundaries"; \
	timeout 10s grep -Fq 'for source in [ForwardPhase::UsrExchangeIntent, ForwardPhase::UsrExchanged] {' "$$prefix_boundaries"; \
	timeout 10s grep -Fq 'for boot in [BootRollback::NotRequired, BootRollback::Unverified] {' "$$prefix_boundaries"; \
	for predicate in usr_rollback_decision_source_is_supported_for_test usr_rollback_resume_route_plan_is_exact_for_test usr_rollback_reverse_plan_is_exact_for_test usr_rollback_candidate_preserve_plan_is_exact_for_test; do \
		timeout 10s grep -Fq "$$predicate" "$$prefix_boundaries"; \
	done; \
	boot_sync_worker="$$( timeout 10s sed -n '/^fn synchronize_excluding_databases(/,/^}/p' "$$boot_worker" )"; \
	timeout 10s test "$$( timeout 10s grep -Fc 'observe_boot_synchronize_attempt();' <<<"$$boot_sync_worker" )" = 1; \
	observer_line="$$( timeout 10s grep -nF 'observe_boot_synchronize_attempt();' <<<"$$boot_sync_worker" | timeout 10s cut -d: -f1 )"; \
	first_return_line="$$( timeout 10s grep -nF 'if excluded.contains(&state.id) {' <<<"$$boot_sync_worker" | timeout 10s sed -n '1p' | timeout 10s cut -d: -f1 )"; \
	timeout 10s test "$$observer_line" -lt "$$first_return_line"; \
	timeout 10s grep -Fq 'reset_boot_synchronize_observer();' "$$tests/boot_repair_required_matrix.rs"; \
	timeout 10s grep -Fq 'assert_no_boot_synchronize_attempts();' "$$tests/boot_repair_required_matrix.rs"; \
	for fault in temporary_sync update_exchange update_first_directory_sync displaced_unlink update_final_directory_sync; do timeout 10s grep -Fq "$$fault" "$$tests/boot_repair_required_storage_faults.rs"; done; \
	timeout 10s grep -Fq 'release_boot_handles(fixture)' "$$tests/boot_repair_required_storage_faults.rs"; \
	timeout 10s grep -Fq 'enter_fresh_handles(retained.path())' "$$tests/boot_repair_required_storage_faults.rs"; \
	for file in "$$gate" "$$orchestrator" "$$authority" "$$decision_authority" "$$resume_authority" "$$reverse_authority" "$$candidate_authority" "$$reconciliation_root" "$$namespace_root" "$$recovery_root" "$$focused_exports" "$$proof" "$$executor" "$$fixture" "$$boot_worker" "$$tests/mod.rs" "$$tests"/boot_repair_required_*.rs "$$tests/support.rs" misc/make/startup-active-reblit-boot-repair-required-tests.mk; do \
		timeout 10s test "$$( timeout 10s wc -l < "$$file" )" -le 1000; \
	done; \
	timeout 1800s $(CARGO) test -p forge --lib startup_active_reblit_boot_repair_required -- --test-threads=1
