.PHONY: forge-startup-usr-rollback-active-reblit-complete-route-test

forge-startup-usr-rollback-active-reblit-complete-route-test:
	@set -euo pipefail; \
	timeout 10s mkdir -p "$(TOP_DIR)/target"; \
	listed="$$( timeout 10s mktemp "$(TOP_DIR)/target/active-reblit-complete-route-list.XXXXXXXXXXXX" )"; \
	trap 'timeout 10s rm -f "$$listed"' EXIT; \
	timeout 300s $(CARGO) test -p forge --lib -- --list | timeout 30s tee "$$listed" >/dev/null; \
	timeout 10s grep -q . "$$listed"; \
	prefix='client::startup_gate::usr_rollback_active_reblit::tests::'; \
	timeout 10s test "$$( timeout 10s grep -c "^$$prefix"'complete_.*: test$$' "$$listed" )" = 7; \
	for name in \
		complete_authority_binding::startup_active_reblit_complete_route_authority_rejects_reopened_and_cross_root_journal_bindings \
		complete_evidence_races::startup_active_reblit_complete_route_rejects_database_provenance_journal_and_namespace_races \
		complete_exclusions::startup_active_reblit_complete_route_preserves_operation_and_phase_ordering \
		complete_matrix::startup_active_reblit_complete_route_covers_all_sixteen_exact_candidate_preserved_cases \
		complete_restart::startup_active_reblit_complete_route_source_durable_failure_converges_with_fresh_handles \
		complete_restart::startup_active_reblit_complete_route_successor_durable_failure_finalizes_with_fresh_handles \
		complete_storage_faults::startup_active_reblit_complete_route_all_five_journal_faults_converge_on_second_entry; do \
		timeout 10s grep -Fqx "$$prefix$$name: test" "$$listed"; \
	done; \
	gate=crates/forge/src/client/startup_gate.rs; \
	orchestrator=crates/forge/src/client/startup_gate/usr_rollback_active_reblit.rs; \
	authority=crates/forge/src/client/startup_reconciliation/usr_rollback_active_reblit_complete_route_authority.rs; \
	proof=crates/forge/src/client/startup_reconciliation/activation_namespace/active_reblit_complete_route_proof.rs; \
	executor=crates/forge/src/client/startup_recovery/usr_rollback_active_reblit_complete_route.rs; \
	tests=crates/forge/src/client/startup_gate/usr_rollback_active_reblit/tests; \
	for module in complete_authority_binding complete_evidence_races complete_exclusions complete_matrix complete_restart complete_storage_faults; do \
		timeout 10s grep -Fqx "mod $$module;" "$$tests/mod.rs"; \
	done; \
	timeout 10s test "$$( timeout 10s rg -n '^#\[test\]$$' "$$tests"/complete_*.rs | timeout 10s wc -l )" = 7; \
	timeout 10s test "$$( timeout 10s grep -Fc 'CleanSystemStartup::enter(' "$$tests/support.rs" )" = 3; \
	if timeout 10s rg -n 'new_for_test|UsrRollbackActiveReblitCompleteRouteAuthority::capture|usr_rollback_active_reblit::dispatch|persist_usr_rollback_active_reblit_complete_route_and_reopen' "$$tests"/complete_{evidence_races,exclusions,matrix,restart,storage_faults}.rs; then exit 1; else status="$$?"; timeout 10s test "$$status" = 1; fi; \
	timeout 10s grep -Fq 'UsrRollbackActiveReblitCompleteRouteSeal::new_for_test();' "$$tests/complete_authority_binding.rs"; \
	timeout 10s test "$$( timeout 10s grep -Fc 'UsrRollbackActiveReblitCompleteRouteAuthority::capture(' "$$tests/complete_authority_binding.rs" )" = 1; \
	timeout 10s grep -Fq 'authority.revalidate(&journal).unwrap();' "$$tests/complete_authority_binding.rs"; \
	timeout 10s grep -Fq 'authority.revalidate(&reopened_journal).unwrap_err();' "$$tests/complete_authority_binding.rs"; \
	timeout 10s grep -Fq 'authority.revalidate(&other_journal).unwrap_err();' "$$tests/complete_authority_binding.rs"; \
	timeout 10s grep -Fq 'assert_eq!(authority.wrapper_index(), WRAPPER_INDEX);' "$$tests/complete_authority_binding.rs"; \
	timeout 10s rg -U -q '^pub\(in crate::client\) use usr_rollback_active_reblit::\{\n    UsrRollbackActiveReblitBootRepairRequiredSeal, UsrRollbackActiveReblitCompleteRouteSeal,\n    UsrRollbackActiveReblitFinalizationSeal,\n\};' "$$gate"; \
	if timeout 10s awk 'previous == "#[cfg(test)]" && $$0 == "pub(in crate::client) use usr_rollback_active_reblit::{" { found = 1 } { previous = $$0 } END { exit !found }' "$$gate"; then exit 1; else status="$$?"; timeout 10s test "$$status" = 1; fi; \
	timeout 10s grep -Fqx 'pub(in crate::client) struct UsrRollbackActiveReblitCompleteRouteSeal {' "$$orchestrator"; \
	timeout 10s grep -Fqx '        Phase::CandidatePreserved => {' "$$orchestrator"; \
	timeout 10s test "$$( timeout 10s grep -Fc 'UsrRollbackActiveReblitCompleteRouteAuthority::capture(' "$$orchestrator" )" = 1; \
	timeout 10s test "$$( timeout 10s grep -Fc 'persist_usr_rollback_active_reblit_complete_route_and_reopen(journal, authority)?' "$$orchestrator" )" = 1; \
	candidate_line="$$( timeout 10s grep -nF 'Phase::CandidatePreserveIntent => {' "$$orchestrator" | timeout 10s cut -d: -f1 )"; \
	complete_line="$$( timeout 10s grep -nF 'Phase::CandidatePreserved => {' "$$orchestrator" | timeout 10s cut -d: -f1 )"; \
	finalize_line="$$( timeout 10s grep -nF 'Phase::RollbackComplete => {' "$$orchestrator" | timeout 10s cut -d: -f1 )"; \
	timeout 10s test "$$candidate_line" -lt "$$complete_line"; \
	timeout 10s test "$$complete_line" -lt "$$finalize_line"; \
	complete_arm="$$( timeout 10s sed -n '/Phase::CandidatePreserved => {/,/Phase::RollbackComplete => {/p' "$$orchestrator" | timeout 10s sed '$$d' )"; \
	if timeout 10s rg -n 'Phase::(FreshDbInvalidationIntent|FreshDbInvalidated|RollbackComplete)|finalize_usr_rollback|run_(transaction|system)_triggers' <<<"$$complete_arm"; then exit 1; else status="$$?"; timeout 10s test "$$status" = 1; fi; \
	timeout 10s grep -Fq 'record.operation != Operation::ActiveReblit || record.phase != Phase::CandidatePreserved' "$$authority"; \
	timeout 10s grep -Fq 'let journal_binding = journal.binding();' "$$authority"; \
	timeout 10s grep -Fq 'if !journal.has_binding(&journal_binding)' "$$authority"; \
	timeout 10s grep -Fq 'let database_before = match inspect_current_database(record, state_db)? {' "$$authority"; \
	timeout 10s grep -Fq 'let database_after = match inspect_current_database(record, state_db)? {' "$$authority"; \
	timeout 10s grep -Fq 'UsrRollbackActiveReblitCompleteRouteNamespaceInspection::begin(installation, journal, record)' "$$authority"; \
	timeout 10s grep -Fq 'run_between_database_captures();' "$$authority"; \
	timeout 10s grep -Fq 'database_before != database_after' "$$authority"; \
	timeout 10s grep -Fq 'record.candidate.id == record.previous.id' "$$authority"; \
	timeout 10s grep -Fq 'ForwardPhase::UsrExchangeIntent | ForwardPhase::UsrExchanged' "$$authority"; \
	timeout 10s grep -Fq 'rollback.previous_archive == RollbackAction::NotRequired' "$$authority"; \
	timeout 10s test "$$( timeout 10s grep -Fc 'RollbackAction::Applied | RollbackAction::AlreadySatisfied' "$$authority" )" = 2; \
	timeout 10s grep -Fq 'rollback.candidate.disposition == AbortDisposition::Quarantine' "$$authority"; \
	timeout 10s grep -Fq 'rollback.fresh_db == RollbackAction::NotRequired' "$$authority"; \
	timeout 10s grep -Fq 'rollback.boot == BootRollback::NotRequired' "$$authority"; \
	timeout 10s grep -Fq 'rollback.external_effects_may_remain' "$$authority"; \
	timeout 10s grep -Fq 'database_ownership_evidence_compatible(record, evidence)' "$$authority"; \
	timeout 10s grep -Fq 'metadata_provenance_evidence_compatible(record, evidence)' "$$authority"; \
	timeout 10s grep -Fq 'DatabaseEvidence::ExistingCandidate {' "$$authority"; \
	timeout 10s grep -Fq 'candidate: existing,' "$$authority"; \
	timeout 10s grep -Fq 'provenance: Some(_),' "$$authority"; \
	timeout 10s grep -Fq 'previous: None,' "$$authority"; \
	timeout 10s grep -Fq 'existing.state == candidate' "$$authority"; \
	timeout 10s grep -Fq 'existing.ownership == db::state::TransitionOwnership::Cleared' "$$authority"; \
	capture="$$( timeout 10s sed -n '/pub(in crate::client) fn capture(/,/pub(in crate::client) fn revalidate(/p' "$$authority" )"; \
	capture_binding_line="$$( timeout 10s grep -nF 'let journal_binding = journal.binding();' <<<"$$capture" | timeout 10s cut -d: -f1 )"; \
	capture_database_before_line="$$( timeout 10s grep -nF 'let database_before = match inspect_current_database(record, state_db)? {' <<<"$$capture" | timeout 10s cut -d: -f1 )"; \
	capture_namespace_begin_line="$$( timeout 10s grep -nF 'match UsrRollbackActiveReblitCompleteRouteNamespaceInspection::begin(installation, journal, record)' <<<"$$capture" | timeout 10s cut -d: -f1 )"; \
	capture_namespace_finish_line="$$( timeout 10s grep -nF 'let namespace = match namespace_inspection.finish(installation, journal, record) {' <<<"$$capture" | timeout 10s cut -d: -f1 )"; \
	capture_database_after_line="$$( timeout 10s grep -nF 'let database_after = match inspect_current_database(record, state_db)? {' <<<"$$capture" | timeout 10s cut -d: -f1 )"; \
	timeout 10s test "$$capture_binding_line" -lt "$$capture_database_before_line"; \
	timeout 10s test "$$capture_database_before_line" -lt "$$capture_namespace_begin_line"; \
	timeout 10s test "$$capture_namespace_begin_line" -lt "$$capture_namespace_finish_line"; \
	timeout 10s test "$$capture_namespace_finish_line" -lt "$$capture_database_after_line"; \
	revalidate="$$( timeout 10s sed -n '/pub(in crate::client) fn revalidate(/,/pub(in crate::client) fn installation(/p' "$$authority" )"; \
	revalidate_binding_line="$$( timeout 10s grep -nF 'if !journal.has_binding(&self.journal_binding) {' <<<"$$revalidate" | timeout 10s cut -d: -f1 )"; \
	revalidate_database_before_line="$$( timeout 10s grep -nF 'let database_before =' <<<"$$revalidate" | timeout 10s cut -d: -f1 )"; \
	revalidate_namespace_line="$$( timeout 10s grep -nF 'self.namespace.revalidate(&self.installation, journal, &self.record)?;' <<<"$$revalidate" | timeout 10s cut -d: -f1 )"; \
	revalidate_database_after_line="$$( timeout 10s grep -nF 'let database_after =' <<<"$$revalidate" | timeout 10s cut -d: -f1 )"; \
	timeout 10s test "$$revalidate_binding_line" -lt "$$revalidate_database_before_line"; \
	timeout 10s test "$$revalidate_database_before_line" -lt "$$revalidate_namespace_line"; \
	timeout 10s test "$$revalidate_namespace_line" -lt "$$revalidate_database_after_line"; \
	timeout 10s grep -Fq 'require_exact_active_reblit_candidate_preserved_topology(expected, &before)?' "$$proof"; \
	timeout 10s grep -Fq 'require_exact_wrapper_index(expected, &fresh, self.wrapper_index)?;' "$$proof"; \
	timeout 10s grep -Fq 'run_before_fresh_namespace_capture();' "$$proof"; \
	timeout 10s test "$$( timeout 10s grep -Fc 'authority.revalidate(&journal)' "$$executor" )" = 2; \
	timeout 10s test "$$( timeout 10s grep -Fc 'source_record.rollback_successor(None)' "$$executor" )" = 1; \
	timeout 10s test "$$( timeout 10s grep -Fc 'journal.advance(&source_record, &successor)' "$$executor" )" = 1; \
	handoff="$$( timeout 10s sed -n '/Canonical reopen begins only after/,/reopen_canonical_journal/p' "$$executor" )"; \
	drop_authority_line="$$( timeout 10s grep -nF 'drop(authority);' <<<"$$handoff" | timeout 10s cut -d: -f1 )"; \
	drop_journal_line="$$( timeout 10s grep -nF 'drop(journal);' <<<"$$handoff" | timeout 10s cut -d: -f1 )"; \
	reopen_line="$$( timeout 10s grep -nF 'reopen_canonical_journal(&installation)' <<<"$$handoff" | timeout 10s cut -d: -f1 )"; \
	timeout 10s test "$$drop_authority_line" -lt "$$drop_journal_line"; \
	timeout 10s test "$$drop_journal_line" -lt "$$reopen_line"; \
	if timeout 10s rg -n '^[[:space:]]*(loop|while|for)[[:space:]]|=[[:space:]]*(loop|while|for)[[:space:]]|diesel::|SqliteConnection|run_(transaction|system)_triggers|finalize_usr_rollback|journal\.delete|remove_exact_fresh_transition|renameat|unlink|mkdir|create_dir|set_permissions|chmod' "$$executor"; then exit 1; else status="$$?"; timeout 10s test "$$status" = 1; fi; \
	timeout 10s grep -Fq 'for epoch in Epoch::ALL {' "$$tests/complete_matrix.rs"; \
	timeout 10s grep -Fq 'for source in CandidateSource::ALL {' "$$tests/complete_matrix.rs"; \
	timeout 10s grep -Fq 'for usr_outcome in USR_OUTCOMES {' "$$tests/complete_matrix.rs"; \
	timeout 10s grep -Fq 'for candidate_outcome in CandidateOrigin::ALL {' "$$tests/complete_matrix.rs"; \
	timeout 10s grep -Fq 'pub(super) const WRAPPER_INDEX: usize = 13;' "$$tests/support.rs"; \
	for fault in temporary_sync update_exchange update_first_directory_sync displaced_unlink update_final_directory_sync; do timeout 10s grep -Fq "$$fault" "$$tests/complete_storage_faults.rs"; done; \
	timeout 10s test "$$( timeout 10s grep -Fc 'release_candidate_handles(fixture)' "$$tests/complete_restart.rs" )" = 2; \
	timeout 10s test "$$( timeout 10s grep -Fc 'enter_fresh_handles(retained.path())' "$$tests/complete_restart.rs" )" = 1; \
	timeout 10s test "$$( timeout 10s grep -Fc 'enter_clean_fresh_handles(retained.path())' "$$tests/complete_restart.rs" )" = 1; \
	timeout 10s grep -Fq 'assert_canonical_absent(retained.path());' "$$tests/complete_restart.rs"; \
	for blocker in DatabaseConflict MetadataProvenanceConflict; do timeout 10s grep -Fq "RecoveryBlocker::$$blocker" "$$tests/complete_evidence_races.rs"; done; \
	for hook in arm_between_usr_rollback_active_reblit_complete_route_database_captures arm_before_usr_rollback_active_reblit_complete_route_final_revalidation arm_before_usr_rollback_active_reblit_complete_route_fresh_namespace_capture; do timeout 10s grep -Fq "$$hook" "$$tests/complete_evidence_races.rs"; done; \
	for operation in Archived NewState; do timeout 10s grep -Fq "OperationKind::$$operation" "$$tests/complete_exclusions.rs"; done; \
	timeout 10s grep -Fq 'Phase::RollbackComplete' "$$tests/complete_exclusions.rs"; \
	timeout 10s grep -Fq 'rollback.source = ForwardPhase::BootSyncStarted;' "$$tests/complete_exclusions.rs"; \
	timeout 10s grep -Fq 'assert_eq!(rollback.previous_archive, RollbackAction::NotRequired);' "$$tests/complete_exclusions.rs"; \
	timeout 10s grep -Fq 'rollback.boot = BootRollback::PendingUnverifiable;' "$$tests/complete_exclusions.rs"; \
	timeout 10s grep -Fq 'Phase::BootRepairRequired' "$$tests/complete_exclusions.rs"; \
	timeout 10s grep -Fq 'rollback.source = ForwardPhase::TransactionTriggersComplete;' "$$tests/complete_exclusions.rs"; \
	timeout 10s grep -Fq 'rollback.usr_exchange = RollbackAction::NotRequired;' "$$tests/complete_exclusions.rs"; \
	timeout 10s grep -Fq 'completion_lookalike.rollback_successor(None).unwrap().phase' "$$tests/complete_exclusions.rs"; \
	timeout 10s grep -Fq 'i32::from(wrong_topology.fixture.previous_state) + 1' "$$tests/complete_exclusions.rs"; \
	timeout 10s grep -Fq 'fs::rename(&exact_wrapper, &lookalike).unwrap();' "$$tests/complete_exclusions.rs"; \
	for blocker in ActivationNamespaceRejected PhaseNamespaceConflict; do timeout 10s grep -Fq "RecoveryBlocker::$$blocker" "$$tests/complete_exclusions.rs"; done; \
	timeout 10s grep -Fq 'take_active_reblit_candidate_preserve_post_exchange_durability_events().is_empty()' "$$tests/support.rs"; \
	for file in "$$gate" "$$orchestrator" "$$authority" "$$proof" "$$executor" "$$tests"/complete_*.rs "$$tests/support.rs" misc/make/startup-rollback-active-reblit-complete-route-tests.mk; do \
		timeout 10s test "$$( timeout 10s wc -l < "$$file" )" -le 1000; \
	done; \
	timeout 1800s $(CARGO) test -p forge --lib startup_active_reblit_complete_route -- --test-threads=1
