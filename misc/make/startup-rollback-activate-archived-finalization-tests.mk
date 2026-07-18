.PHONY: forge-startup-usr-rollback-activate-archived-finalization-test

forge-startup-usr-rollback-activate-archived-finalization-test:
	@set -euo pipefail; \
	timeout 10s mkdir -p "$(TOP_DIR)/target"; \
	listed="$$( timeout 10s mktemp "$(TOP_DIR)/target/activate-archived-finalization-list.XXXXXXXXXXXX" )"; \
	refs="$$( timeout 10s mktemp "$(TOP_DIR)/target/activate-archived-finalization-refs.XXXXXXXXXXXX" )"; \
	executor_code="$$( timeout 10s mktemp "$(TOP_DIR)/target/activate-archived-finalization-code.XXXXXXXXXXXX" )"; \
	trap 'timeout 10s rm -f "$$listed" "$$refs" "$$executor_code"' EXIT; \
	timeout 300s $(CARGO) test -p forge --lib -- --list | timeout 30s tee "$$listed" >/dev/null; \
	timeout 10s grep -q . "$$listed"; \
	gate_prefix='client::startup_gate::usr_rollback_activate_archived::tests::finalization_'; \
	timeout 10s test "$$( timeout 10s grep -c "^$$gate_prefix.*: test$$" "$$listed" )" = 16; \
	for name in \
		authority_binding::startup_activate_archived_finalization_authority_covers_both_epochs_and_rejects_wrong_bindings \
		authority_binding::startup_activate_archived_finalization_authority_refuses_inexact_terminal_identities \
		boundaries::startup_activate_archived_finalization_authority_excludes_other_operations_and_phases \
		boundaries::startup_activate_archived_finalization_keeps_completion_and_terminal_deletion_on_separate_entries \
		boundaries::startup_activate_archived_finalization_rejects_valid_terminal_lookalike_plan_and_wrong_topology \
		evidence_races::startup_activate_archived_finalization_rejects_capture_and_final_pre_races \
		evidence_races::startup_activate_archived_finalization_rejects_final_pre_and_post_delete_evidence_changes \
		evidence_races::startup_activate_archived_finalization_requires_exact_two_rows_and_candidate_provenance \
		lock_handoff::startup_activate_archived_finalization_hands_the_same_lock_into_clean_startup_until_proof_drop \
		matrix::startup_activate_archived_finalization_covers_all_sixteen_exact_terminal_cases \
		public_binding_races::startup_activate_archived_finalization_rejects_hidden_entry_set_substitution \
		public_binding_races::startup_activate_archived_finalization_rejects_public_directory_and_lock_substitution \
		public_binding_races::startup_activate_archived_finalization_rejects_source_recreation_after_delete_and_absence_proof \
		restart::startup_activate_archived_finalization_restarts_from_observed_absence_with_fresh_handles \
		restart::startup_activate_archived_finalization_restarts_from_retained_terminal_source_with_fresh_handles \
		storage_faults::startup_activate_archived_finalization_classifies_both_delete_faults_and_converges; do \
		timeout 10s grep -Fqx "$$gate_prefix$$name: test" "$$listed"; \
	done; \
	executor_prefix='client::startup_recovery::usr_rollback_activate_archived_finalization::tests::reconcile_delete::'; \
	timeout 10s test "$$( timeout 10s grep -c "^$$executor_prefix.*: test$$" "$$listed" )" = 4; \
	for name in \
		activate_archived_finalization_delete_error_preserves_ambiguous_verification \
		activate_archived_finalization_false_delete_report_classifies_authenticated_absence \
		activate_archived_finalization_false_delete_report_classifies_exact_source \
		activate_archived_finalization_false_delete_report_rejects_an_unexpected_record; do \
		timeout 10s grep -Fqx "$$executor_prefix$$name: test" "$$listed"; \
	done; \
	authority=crates/forge/src/client/startup_reconciliation/usr_rollback_activate_archived_finalization_authority.rs; \
	proof=crates/forge/src/client/startup_reconciliation/activation_namespace/activate_archived_finalization_proof.rs; \
	topology=crates/forge/src/client/startup_reconciliation/activation_namespace/candidate_preserve_proof.rs; \
	executor=crates/forge/src/client/startup_recovery/usr_rollback_activate_archived_finalization.rs; \
	orchestrator=crates/forge/src/client/startup_gate/usr_rollback_activate_archived.rs; \
	startup_gate=crates/forge/src/client/startup_gate.rs; \
	recovery_root=crates/forge/src/client/startup_recovery.rs; \
	reconciliation_root=crates/forge/src/client/startup_reconciliation.rs; \
	namespace_root=crates/forge/src/client/startup_reconciliation/activation_namespace.rs; \
	gate_tests=crates/forge/src/client/startup_gate/usr_rollback_activate_archived/tests; \
	executor_tests=crates/forge/src/client/startup_recovery/usr_rollback_activate_archived_finalization/tests; \
	timeout 10s grep -Fqx 'mod usr_rollback_activate_archived_finalization_authority;' "$$reconciliation_root"; \
	timeout 10s grep -Fqx 'mod activate_archived_finalization_proof;' "$$namespace_root"; \
	timeout 10s grep -Fqx 'mod usr_rollback_activate_archived_finalization;' "$$recovery_root"; \
	for module in finalization_authority_binding finalization_boundaries finalization_evidence_races finalization_lock_handoff finalization_matrix finalization_public_binding_races finalization_restart finalization_storage_faults; do \
		timeout 10s grep -Fqx "mod $$module;" "$$gate_tests/mod.rs"; \
	done; \
	if timeout 10s rg -n 'finalization_process_kill|kill_self|nix::libc::SIGKILL' "$$gate_tests/mod.rs" "$$gate_tests"/finalization_*.rs; then exit 1; else status="$$?"; timeout 10s test "$$status" = 1; fi; \
	for module in reconcile_delete support; do timeout 10s grep -Fqx "mod $$module;" "$$executor_tests/mod.rs"; done; \
	timeout 10s grep -Fqx 'pub(in crate::client) struct UsrRollbackActivateArchivedFinalizationSeal {' "$$orchestrator"; \
	seal_impl="$$( timeout 10s sed -n '/^impl UsrRollbackActivateArchivedFinalizationSeal {/,/^}/p' "$$orchestrator" )"; \
	timeout 10s test "$$( timeout 10s grep -Fc '    fn new() -> Self {' <<<"$$seal_impl" )" = 1; \
	timeout 10s test "$$( timeout 10s grep -Fc '    pub(in crate::client) fn new_for_test() -> Self {' <<<"$$seal_impl" )" = 1; \
	timeout 10s awk '$$0 == "    #[cfg(test)]" { gated = 1; next } gated && $$0 == "    pub(in crate::client) fn new_for_test() -> Self {" { found++; gated = 0; next } gated { gated = 0 } END { exit found != 1 }' <<<"$$seal_impl"; \
	timeout 10s rg -U -q '^pub\(in crate::client\) use usr_rollback_activate_archived::\{\n    UsrRollbackActivateArchivedCompleteRouteSeal, UsrRollbackActivateArchivedFinalizationSeal,\n\};' "$$startup_gate"; \
	for field in \
		'record.operation == Operation::ActivateArchived' \
		'record.phase == Phase::RollbackComplete' \
		'record.candidate.origin == CandidateOrigin::Archived' \
		'record.previous.origin == PreviousOrigin::ActiveState' \
		'record.candidate.id != record.previous.id' \
		'ForwardPhase::UsrExchangeIntent | ForwardPhase::UsrExchanged' \
		'rollback.previous_archive == RollbackAction::NotRequired' \
		'rollback.candidate.disposition == AbortDisposition::Rearchive' \
		'rollback.fresh_db == RollbackAction::NotRequired' \
		'rollback.boot == BootRollback::NotRequired' \
		'!rollback.external_effects_may_remain'; do \
		timeout 10s grep -Fq "$$field" "$$authority"; \
	done; \
	timeout 10s test "$$( timeout 10s grep -Fc 'RollbackAction::Applied | RollbackAction::AlreadySatisfied' "$$authority" )" = 2; \
	for field in 'DatabaseEvidence::ExistingCandidate {' 'provenance: Some(_),' 'previous: Some(previous_existing),' 'existing.ownership == db::state::TransitionOwnership::Cleared' 'previous_existing.ownership == db::state::TransitionOwnership::Cleared'; do \
		timeout 10s grep -Fq "$$field" "$$authority"; \
	done; \
	capture="$$( timeout 10s sed -n '/pub(in crate::client) fn capture(/,/pub(in crate::client) fn revalidate(/p' "$$authority" )"; \
	binding_line="$$( timeout 10s grep -nF 'let journal_binding = journal.binding();' <<<"$$capture" | timeout 10s cut -d: -f1 )"; \
	database_before_line="$$( timeout 10s grep -nF 'let database_before = match inspect_current_database(record, state_db)? {' <<<"$$capture" | timeout 10s cut -d: -f1 )"; \
	namespace_line="$$( timeout 10s grep -nF 'UsrRollbackActivateArchivedFinalizationNamespaceInspection::begin' <<<"$$capture" | timeout 10s cut -d: -f1 )"; \
	database_after_line="$$( timeout 10s grep -nF 'let database_after = match inspect_current_database(record, state_db)? {' <<<"$$capture" | timeout 10s cut -d: -f1 )"; \
	timeout 10s test "$$binding_line" -lt "$$database_before_line"; \
	timeout 10s test "$$database_before_line" -lt "$$namespace_line"; \
	timeout 10s test "$$namespace_line" -lt "$$database_after_line"; \
	if timeout 10s rg -U -n '#\[derive\([^]]*Clone[^]]*\)\]\n(?:#\[[^\n]*\]\n)*(?:pub\([^)]*\)[[:space:]]+)?(?:struct|enum)[[:space:]]+UsrRollbackActivateArchivedFinalization(?:Authority|DatabaseEvidence|NamespaceProof)' "$$authority" "$$proof"; then exit 1; else status="$$?"; timeout 10s test "$$status" = 1; fi; \
	if timeout 10s rg -n 'impl Clone for UsrRollbackActivateArchivedFinalization(?:Authority|DatabaseEvidence|NamespaceProof)' "$$authority" "$$proof"; then exit 1; else status="$$?"; timeout 10s test "$$status" = 1; fi; \
	timeout 10s rg -n -F 'UsrRollbackActivateArchivedFinalizationAuthority::capture(' crates/forge/src/client --glob '*.rs' --glob '!**/tests/**' --glob '!**/tests.rs' --glob '!**/*_tests.rs' --glob '!**/*_tests/**' --glob '!**/usr_rollback_activate_archived_finalization_authority.rs' > "$$refs"; \
	timeout 10s test "$$( timeout 10s wc -l < "$$refs" )" = 1; \
	timeout 10s test "$$( timeout 10s cut -d: -f1 "$$refs" )" = "$$orchestrator"; \
	timeout 10s grep -Fqx '        Phase::RollbackComplete => {' "$$orchestrator"; \
	timeout 10s test "$$( timeout 10s grep -Fc 'finalize_usr_rollback_activate_archived(journal, authority)?' "$$orchestrator" )" = 1; \
	timeout 10s grep -Fq 'Ok(Dispatch::Finalized { journal })' "$$orchestrator"; \
	timeout 10s grep -Fq 'usr_rollback_activate_archived::Dispatch::Finalized { journal } => {' "$$startup_gate"; \
	timeout 10s grep -Fq 'return Self::admit_clean_after_terminal_finalization(installation, state_db, journal);' "$$startup_gate"; \
	timeout 10s test "$$( timeout 10s grep -Fc '.revalidate(&journal)' "$$executor" )" = 2; \
	timeout 10s test "$$( timeout 10s grep -Fc 'require_exact_public_source(&installation, &journal, &source_record)' "$$executor" )" = 2; \
	timeout 10s test "$$( timeout 10s grep -Fc 'journal.delete_revalidated_retained_cast(cast, &source_record)' "$$executor" )" = 1; \
	timeout 10s grep -Fq 'authority.revalidate_after_journal_delete(journal)?' "$$executor"; \
	for branch in 'Ok(true) => match durable {' 'Ok(false) => match durable {' 'Err(source) => match durable {' 'DeleteReportedFalse { durable }' 'DeleteReportedFalseAndVerification { source }' 'Delete { durable, source }' 'DeleteAndVerification {'; do \
		timeout 10s grep -Fq "$$branch" "$$executor"; \
	done; \
	timeout 10s grep -Fq 'require_exact_activate_archived_rollback_complete_topology' "$$proof" "$$topology"; \
	timeout 10s test "$$( timeout 10s grep -Fc 'require_exact_public_journal_absence(installation, journal)?;' "$$proof" )" = 2; \
	if timeout 10s rg -n 'canonical_journal_reopen|reopen_canonical_journal|TransitionJournalStore::(?:open|try_open)' "$$executor"; then exit 1; else status="$$?"; timeout 10s test "$$status" = 1; fi; \
	timeout 10s sed -E 's,//.*$$,,' "$$executor" > "$$executor_code"; \
	if timeout 10s rg -n '^[[:space:]]*(loop|while|for)[[:space:]]|=[[:space:]]*(loop|while|for)[[:space:]]|retry|cleanup|run_(transaction|system)_triggers|\.advance[[:space:]]*\(' "$$executor_code"; then exit 1; else status="$$?"; timeout 10s test "$$status" = 1; fi; \
	for hook in arm_between_usr_rollback_activate_archived_finalization_database_captures arm_before_usr_rollback_activate_archived_finalization_fresh_namespace_capture arm_before_usr_rollback_activate_archived_finalization_final_revalidation arm_after_usr_rollback_activate_archived_finalization_delete arm_before_usr_rollback_activate_archived_finalization_final_durable_inspection; do \
		timeout 10s rg -q "$$hook" "$$authority" "$$proof" "$$executor" "$$reconciliation_root" "$$recovery_root"; \
	done; \
	for loop in 'for epoch in Epoch::ALL {' 'for source in CandidateSource::ALL {' 'for usr_outcome in USR_OUTCOMES {' 'for candidate_outcome in CandidateOutcome::ALL {'; do timeout 10s grep -Fq "$$loop" "$$gate_tests/finalization_matrix.rs"; done; \
	for cardinality in 'Epoch::ALL.len(), 2' 'CandidateSource::ALL.len(), 2' 'USR_OUTCOMES.len(), 2' 'CandidateOutcome::ALL.len(), 2'; do timeout 10s grep -Fq "$$cardinality" "$$gate_tests/finalization_matrix.rs"; done; \
	timeout 10s grep -Fq 'assert_eq!(cases, 16);' "$$gate_tests/finalization_matrix.rs"; \
	for fault in arm_next_delete_canonical_unlink_fault arm_next_delete_directory_sync_fault assert_delete_canonical_unlink_fault_consumed assert_delete_directory_sync_fault_consumed; do timeout 10s grep -Fq "$$fault" "$$gate_tests/finalization_storage_faults.rs"; done; \
	timeout 10s test "$$( timeout 10s grep -Fc 'release_route_handles(fixture)' "$$gate_tests/finalization_restart.rs" )" = 2; \
	timeout 10s test "$$( timeout 10s grep -Fc 'assert_fresh_exact_database_pair(' "$$gate_tests/finalization_restart.rs" )" = 4; \
	for disclaimer in 'fresh process-like' 'do not claim SIGKILL' 'reboot' 'power-loss durability'; do timeout 10s grep -Fq "$$disclaimer" "$$gate_tests/finalization_restart.rs"; done; \
	timeout 10s grep -Fq 'StorageError::AcquireLock' "$$gate_tests/finalization_lock_handoff.rs"; \
	for file in "$$authority" "$$proof" "$$topology" "$$executor" "$$orchestrator" "$$startup_gate" "$$recovery_root" "$$reconciliation_root" "$$namespace_root" "$$gate_tests"/finalization_*.rs "$$gate_tests/support.rs" "$$executor_tests"/*.rs misc/make/startup-rollback-activate-archived-finalization-tests.mk; do \
		timeout 10s test "$$( timeout 10s wc -l < "$$file" )" -le 1000; \
	done; \
	timeout 600s $(CARGO) test -p forge --lib activate_archived_finalization -- --test-threads=1
