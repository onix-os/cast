.PHONY: forge-startup-usr-rollback-fresh-db-invalidation-route-test

forge-startup-usr-rollback-fresh-db-invalidation-route-test:
	@set -eu; \
	listed="$$( timeout 300s $(CARGO) test -p forge --lib -- --list )"; \
	timeout 10s grep -q . <<<"$$listed"; \
	prefix='client::startup_recovery::usr_rollback_fresh_db_invalidation_route::tests::'; \
	timeout 10s test "$$( timeout 10s grep -c "^$$prefix.*: test$$" <<<"$$listed" )" = 17; \
	for name in \
		admission::startup_usr_rollback_fresh_db_invalidation_route_admits_exact_current_and_historical_evidence \
		admission::startup_usr_rollback_fresh_db_invalidation_route_defers_inexact_phase_plan_database_and_provenance \
		endpoint::startup_root_links_complete_new_state_reaches_generation_16_then_stays_closed_without_effects \
		evidence_races::startup_usr_rollback_fresh_db_invalidation_route_rejects_mixed_and_cross_root_journals \
		evidence_races::startup_usr_rollback_fresh_db_invalidation_route_capture_and_final_evidence_races_never_advance \
		evidence_races::startup_usr_rollback_fresh_db_invalidation_route_refuses_namespace_lookalikes \
		evidence_races::startup_root_links_fresh_db_route_capture_rejects_all_root_abi_mutations \
		evidence_races::startup_root_links_fresh_db_route_final_revalidation_rejects_all_root_abi_mutations \
		matrix::startup_usr_rollback_fresh_db_invalidation_route_applied_matrix_persists_exact_intent \
		matrix::startup_usr_rollback_fresh_db_invalidation_route_finish_matrix_persists_exact_intent \
		record_binding::startup_usr_rollback_fresh_db_invalidation_route_bound_advance_same_byte_replacements_never_succeed \
		record_binding::startup_usr_rollback_fresh_db_invalidation_route_same_byte_successor_replacement_fails_same_store_binding \
		record_binding::startup_usr_rollback_fresh_db_invalidation_route_same_byte_successor_replacement_fails_reopened_binding \
		storage_reopen::startup_usr_rollback_fresh_db_invalidation_route_storage_faults_reopen_exact_source_or_successor \
		storage_reopen::startup_usr_rollback_fresh_db_invalidation_route_consumes_old_store_and_returns_canonical_reopen \
		fresh_reopen::startup_usr_rollback_fresh_db_invalidation_route_source_durable_fresh_handle_reopen_retries_only_the_route \
		fresh_reopen::startup_usr_rollback_fresh_db_invalidation_route_successor_durable_fresh_handle_reopen_skips_the_route; do \
		timeout 10s grep -Fqx "$$prefix$$name: test" <<<"$$listed"; \
	done; \
	executor=crates/forge/src/client/startup_recovery/usr_rollback_fresh_db_invalidation_route.rs; \
	authority=crates/forge/src/client/startup_reconciliation/usr_rollback_fresh_db_invalidation_route_authority.rs; \
	proof=crates/forge/src/client/startup_reconciliation/activation_namespace/fresh_db_invalidation_route_proof.rs; \
	topology=crates/forge/src/client/startup_reconciliation/activation_namespace/candidate_preserve_proof.rs; \
	reopen=crates/forge/src/client/startup_recovery/canonical_journal_reopen.rs; \
	recovery_root=crates/forge/src/client/startup_recovery.rs; \
	reconciliation_root=crates/forge/src/client/startup_reconciliation.rs; \
	activation_root=crates/forge/src/client/startup_reconciliation/activation_namespace.rs; \
	startup_gate=crates/forge/src/client/startup_gate.rs; \
	production_dispatch=crates/forge/src/client/startup_gate/usr_rollback_new_state.rs; \
	downstream=crates/forge/src/client/startup_reconciliation/usr_rollback_fresh_db_invalidation_authority.rs; \
	complete_route=crates/forge/src/client/startup_reconciliation/usr_rollback_complete_route_authority.rs; \
	finalization=crates/forge/src/client/startup_reconciliation/usr_rollback_finalization_authority.rs; \
	candidate_sources=crates/forge/src/client/startup_reconciliation/usr_rollback_candidate_preserve_authority/tests/support.rs; \
	tests=crates/forge/src/client/startup_recovery/usr_rollback_fresh_db_invalidation_route/tests.rs; \
	support=crates/forge/src/client/startup_recovery/usr_rollback_fresh_db_invalidation_route/tests/support.rs; \
	admission=crates/forge/src/client/startup_recovery/usr_rollback_fresh_db_invalidation_route/tests/admission.rs; \
	endpoint=crates/forge/src/client/startup_recovery/usr_rollback_fresh_db_invalidation_route/tests/endpoint.rs; \
	matrix=crates/forge/src/client/startup_recovery/usr_rollback_fresh_db_invalidation_route/tests/matrix.rs; \
	races=crates/forge/src/client/startup_recovery/usr_rollback_fresh_db_invalidation_route/tests/evidence_races.rs; \
	binding=crates/forge/src/client/startup_recovery/usr_rollback_fresh_db_invalidation_route/tests/record_binding.rs; \
	storage=crates/forge/src/client/startup_recovery/usr_rollback_fresh_db_invalidation_route/tests/storage_reopen.rs; \
	fresh_reopen=crates/forge/src/client/startup_recovery/usr_rollback_fresh_db_invalidation_route/tests/fresh_reopen.rs; \
	timeout 10s grep -Fqx 'mod usr_rollback_fresh_db_invalidation_route;' "$$recovery_root"; \
	timeout 10s test "$$( timeout 10s grep -Fc 'mod usr_rollback_fresh_db_invalidation_route;' "$$recovery_root" )" = 1; \
	timeout 10s grep -Fqx 'mod usr_rollback_fresh_db_invalidation_route_authority;' "$$reconciliation_root"; \
	timeout 10s grep -Fqx 'mod fresh_db_invalidation_route_proof;' "$$activation_root"; \
	for module in admission endpoint evidence_races fresh_reopen matrix record_binding storage_reopen support; do \
		timeout 10s grep -Fqx "mod $$module;" "$$tests"; \
	done; \
	timeout 10s rg -U -q '^pub\(in crate::client\) fn persist_usr_rollback_fresh_db_invalidation_route_and_reopen\(\n    journal: TransitionJournalStore,\n    authority: UsrRollbackFreshDbInvalidationRouteAuthority<'\''_>,\n\) -> Result<\(TransitionJournalStore, TransitionRecord\), UsrRollbackFreshDbInvalidationRoutePersistenceError> \{' "$$executor"; \
	timeout 10s grep -Fqx '    journal_record_binding: TransitionJournalRecordBinding,' "$$authority"; \
	if timeout 10s rg -n 'TransitionJournalBinding|journal_binding' "$$authority" "$$proof"; then exit 1; fi; \
	timeout 10s test "$$( timeout 10s grep -Fc 'journal.record_binding(installation.retained_mutable_cast_directory()?, record)?;' "$$authority" )" = 1; \
	timeout 10s test "$$( timeout 10s grep -Fc 'pub(in crate::client) fn advance_record_binding(' "$$authority" )" = 1; \
	timeout 10s test "$$( timeout 10s grep -Fc '.advance_record_binding(cast, self.journal_record_binding, next)' "$$authority" )" = 1; \
	timeout 10s test "$$( timeout 10s rg -n '\.advance\(' "$$executor" "$$authority" "$$proof" "$$topology" "$$reopen" | timeout 10s wc -l )" = 0; \
	timeout 10s test "$$( timeout 10s grep -Fc 'authority.advance_record_binding(&journal, &successor)' "$$executor" )" = 1; \
	timeout 10s test "$$( timeout 10s grep -Fc '.has_record_binding(cast, successor_binding, successor)' "$$executor" )" = 1; \
	timeout 10s test "$$( timeout 10s grep -Fc '.has_reopened_record_binding(cast, successor_binding, successor)' "$$executor" )" = 1; \
	timeout 10s test "$$( timeout 10s grep -Fc 'journal.has_record_binding(cast, journal_record_binding, expected)?' "$$proof" )" = 1; \
	timeout 10s test "$$( timeout 10s grep -Fc 'journal.has_record_store_binding(journal_record_binding)' "$$proof" )" = 1; \
	timeout 10s grep -Fqx '    let successor = match source_record.rollback_successor(None) {' "$$executor"; \
	timeout 10s grep -Fqx '        Ok(successor) if successor.phase == Phase::FreshDbInvalidationIntent => successor,' "$$executor"; \
	timeout 10s test "$$( timeout 10s rg -n '\.rollback_successor\(None\)' "$$executor" "$$authority" "$$proof" "$$topology" | timeout 10s wc -l )" = 1; \
	advance_line="$$( timeout 10s grep -nF '    let advance = match authority.advance_record_binding(&journal, &successor) {' "$$executor" | timeout 10s cut -d: -f1 )"; \
	drop_line="$$( timeout 10s grep -nF '    drop(journal);' "$$executor" | timeout 10s tail -n 1 | timeout 10s cut -d: -f1 )"; \
	reopen_line="$$( timeout 10s grep -nF 'reopen_canonical_journal(&installation).map_err(UsrRollbackFreshDbInvalidationRouteReopenError::from);' "$$executor" | timeout 10s cut -d: -f1 )"; \
	timeout 10s test "$$advance_line" -lt "$$drop_line"; \
	timeout 10s test "$$drop_line" -lt "$$reopen_line"; \
	bound_suffix="$$( timeout 10s sed -n '/    let advance = match authority.advance_record_binding/,/reopen_canonical_journal(&installation)/p' "$$executor" )"; \
	timeout 10s test "$$( timeout 10s grep -Fc '    drop(journal);' <<<"$$bound_suffix" )" = 3; \
	if timeout 10s grep -Fq 'drop(authority)' <<<"$$bound_suffix"; then exit 1; fi; \
	timeout 10s test "$$( timeout 10s grep -Fc '.revalidate_mutable_namespace()' "$$executor" )" = 4; \
	timeout 10s grep -Fq 'UsrRollbackFreshDbInvalidationRouteAdvanceOutcome::Published' "$$executor"; \
	timeout 10s grep -Fq 'UsrRollbackFreshDbInvalidationRouteAdvanceOutcome::StorageFailed' "$$executor"; \
	timeout 10s grep -Fq 'UsrRollbackFreshDbInvalidationRouteAdvanceOutcome::SuccessorBindingFailed' "$$executor"; \
	timeout 10s grep -Fq 'DurableUsrRollbackFreshDbInvalidationRouteRecord::CandidatePreserved' "$$executor"; \
	timeout 10s grep -Fq 'DurableUsrRollbackFreshDbInvalidationRouteRecord::FreshDbInvalidationIntent' "$$executor"; \
	timeout 10s grep -Fq 'DatabaseEvidence::CandidateOwnership {' "$$authority"; \
	timeout 10s grep -Fq 'ownership: db::state::TransitionOwnership::Matching' "$$authority"; \
	timeout 10s grep -Fq 'provenance: Some(_)' "$$authority"; \
	timeout 10s test "$$( timeout 10s grep -Fc 'ForwardPhase::RootLinksComplete' "$$authority" )" = 2; \
	timeout 10s grep -Fq 'record.operation == Operation::NewState' "$$authority"; \
	timeout 10s grep -Fq 'record.phase == Phase::CandidatePreserved' "$$authority"; \
	timeout 10s grep -Fq 'rollback.fresh_db == RollbackAction::Pending' "$$authority"; \
	timeout 10s grep -Fq 'require_exact_new_state_candidate_preserved_topology' "$$proof"; \
	timeout 10s test "$$( timeout 10s grep -Fc 'require_exact_new_state_candidate_preserved_topology(' "$$proof" )" = 6; \
	source_axis="$$( timeout 10s sed -n '/^impl CandidateSource {/,/^}/p' "$$candidate_sources" )"; \
	timeout 10s grep -Fq 'pub(super) const ALL: [Self; 2] = [Self::Intent, Self::Exchanged];' <<<"$$source_axis"; \
	timeout 10s grep -Fq 'pub(super) const THROUGH_CANDIDATE_PRESERVED: [Self; 3] = [' <<<"$$source_axis"; \
	timeout 10s test "$$( timeout 10s grep -Fc 'Self::RootLinksComplete,' <<<"$$source_axis" )" = 1; \
	downstream_plan="$$( timeout 10s sed -n '/^fn fresh_db_invalidation_plan_is_exact/,/^}/p' "$$downstream" )"; \
	complete_route_plan="$$( timeout 10s sed -n '/^fn rollback_complete_route_plan_is_exact/,/^}/p' "$$complete_route" )"; \
	finalization_plan="$$( timeout 10s sed -n '/^fn rollback_finalization_plan_is_exact/,/^}/p' "$$finalization" )"; \
	timeout 10s grep -Fq 'record.phase == Phase::FreshDbInvalidationIntent' <<<"$$downstream_plan"; \
	timeout 10s grep -Fq 'record.phase == Phase::FreshDbInvalidated' <<<"$$complete_route_plan"; \
	timeout 10s grep -Fq 'record.phase == Phase::RollbackComplete' <<<"$$finalization_plan"; \
	for closed_plan in "$$downstream_plan" "$$complete_route_plan" "$$finalization_plan"; do \
		timeout 10s test "$$( timeout 10s grep -Fc 'ForwardPhase::UsrExchangeIntent | ForwardPhase::UsrExchanged' <<<"$$closed_plan" )" = 1; \
		if timeout 10s grep -Fq 'RootLinksComplete' <<<"$$closed_plan"; then exit 1; fi; \
	done; \
	timeout 10s test "$$( timeout 10s grep -Fc 'persist_usr_rollback_fresh_db_invalidation_route_and_reopen(journal, authority)?' "$$production_dispatch" )" = 1; \
	timeout 10s test "$$( timeout 10s grep -Fc 'UsrRollbackFreshDbInvalidationRouteAuthority::capture(' "$$production_dispatch" )" = 1; \
	timeout 10s test "$$( timeout 10s grep -Fc 'persist_usr_rollback_fresh_db_invalidation_route_and_reopen(' "$$executor" )" = 1; \
	timeout 10s grep -Fqx '    UsrRollbackFreshDbInvalidationRoutePersistenceError, persist_usr_rollback_fresh_db_invalidation_route_and_reopen,' "$$recovery_root"; \
	timeout 10s test "$$( timeout 10s grep -Fc 'persist_usr_rollback_fresh_db_invalidation_route_and_reopen,' "$$recovery_root" )" = 1; \
	production_references="$$( timeout 10s rg -n -F 'persist_usr_rollback_fresh_db_invalidation_route_and_reopen(journal, authority)?' crates/forge/src/client --glob '*.rs' --glob '!**/tests/**' --glob '!**/tests.rs' --glob '!**/*_tests.rs' --glob '!**/*_tests/**' )"; \
	timeout 10s test "$$( timeout 10s grep -c . <<<"$$production_references" )" = 1; \
	timeout 10s test "$$( timeout 10s cut -d: -f1 <<<"$$production_references" )" = "$$production_dispatch"; \
	production_name_references="$$( timeout 10s rg -n -F 'persist_usr_rollback_fresh_db_invalidation_route_and_reopen' crates/forge/src/client --glob '*.rs' --glob '!**/tests/**' --glob '!**/tests.rs' --glob '!**/*_tests.rs' --glob '!**/*_tests/**' --glob '!**/usr_rollback_fresh_db_invalidation_route.rs' )"; \
	timeout 10s test "$$( timeout 10s grep -c . <<<"$$production_name_references" )" = 3; \
	timeout 10s test "$$( timeout 10s grep -Fc "$$recovery_root:" <<<"$$production_name_references" )" = 1; \
	timeout 10s test "$$( timeout 10s grep -Fc "$$production_dispatch:" <<<"$$production_name_references" )" = 2; \
	capture_references="$$( timeout 10s rg -n -F 'UsrRollbackFreshDbInvalidationRouteAuthority::capture(' crates/forge/src/client --glob '*.rs' --glob '!**/tests/**' --glob '!**/tests.rs' --glob '!**/*_tests.rs' --glob '!**/*_tests/**' )"; \
	timeout 10s test "$$( timeout 10s grep -c . <<<"$$capture_references" )" = 1; \
	timeout 10s test "$$( timeout 10s cut -d: -f1 <<<"$$capture_references" )" = "$$production_dispatch"; \
	authority_references="$$( timeout 10s rg -n -F 'UsrRollbackFreshDbInvalidationRouteAuthority' crates/forge/src/client --glob '*.rs' --glob '!**/tests/**' --glob '!**/tests.rs' --glob '!**/*_tests.rs' --glob '!**/*_tests/**' --glob '!**/usr_rollback_fresh_db_invalidation_route_authority.rs' )"; \
	timeout 10s test "$$( timeout 10s grep -c . <<<"$$authority_references" )" = 8; \
	timeout 10s test "$$( timeout 10s grep -Fc 'UsrRollbackFreshDbInvalidationRouteAuthority' "$$reconciliation_root" )" = 2; \
	timeout 10s test "$$( timeout 10s grep -Fc 'UsrRollbackFreshDbInvalidationRouteAuthority' "$$executor" )" = 3; \
	timeout 10s test "$$( timeout 10s grep -Fc 'UsrRollbackFreshDbInvalidationRouteAuthority' "$$production_dispatch" )" = 3; \
	timeout 10s rg -U -q '^pub\(in crate::client::startup_reconciliation::activation_namespace\) fn require_exact_new_state_candidate_preserved_topology\(' "$$topology"; \
	topology_helper="$$( timeout 10s sed -n '/^pub(in crate::client::startup_reconciliation::activation_namespace) fn require_exact_new_state_candidate_preserved_topology/,/^}/p' "$$topology" )"; \
	timeout 10s test -n "$$topology_helper"; \
	production_code="$$( timeout 10s sed -E 's,//.*$$,,' "$$executor" "$$authority" "$$proof"; timeout 10s sed -E 's,//.*$$,,' <<<"$$topology_helper" )"; \
	if timeout 10s rg -n 'clear_transition_if_matches|remove_transition_if_matches|insert_fresh_metadata|delete_metadata|invalidate(_|[[:space:]]*\()|\.add\(|\.create\(|\.remove\(|\.batch_remove\(|\.delete\(' <<<"$$production_code"; then exit 1; fi; \
	if timeout 10s rg -n 'diesel::|SqliteConnection|sql_query|\.execute\(|\.transaction\(' <<<"$$production_code"; then exit 1; fi; \
	if timeout 10s rg -n 'renameat|std::fs|(^|[^_[:alnum:]])fs::|rename[[:space:]]*\(|unlink(at)?[[:space:]]*\(|linkat[[:space:]]*\(|sync_(all|data)|write_all|set_permissions|chmod|create_dir|remove_(dir|file)|hard_link|symlink|attempt_move|reconcile_move' <<<"$$production_code"; then exit 1; fi; \
	if timeout 10s rg -n 'run_transaction_triggers|run_system_triggers|root_links|exchange_forward|exchange_reverse|archive_previous|rearchive_archived|preserve_failed|remove_exact_archived|cleanup|retry|finalize|boot_synchronize|dispatch' <<<"$$production_code"; then exit 1; fi; \
	if timeout 10s rg -n '^[[:space:]]*(loop|while|for)[[:space:]]|=[[:space:]]*(loop|while|for)[[:space:]]' <<<"$$production_code"; then exit 1; fi; \
	timeout 10s test "$$( timeout 10s rg -l '^pub\(in crate::client\) struct UsrRollbackFreshDbInvalidationRouteSeal \{' crates/forge/src/client --glob '*.rs' )" = "$$production_dispatch"; \
	timeout 10s grep -Fq 'UsrRollbackFreshDbInvalidationRouteSeal' "$$startup_gate"; \
	timeout 10s grep -Fqx 'pub(in crate::client) struct UsrRollbackFreshDbInvalidationRouteSeal {' "$$production_dispatch"; \
	timeout 10s awk '$$0 == "pub(in crate::client) struct UsrRollbackFreshDbInvalidationRouteSeal {" { state = 1; next } state == 1 && $$0 == "    _private: ()," { state = 2; next } state == 2 && $$0 == "}" { found = 1 } END { exit !found }' "$$production_dispatch"; \
	seal_impl="$$( timeout 10s sed -n '/^impl UsrRollbackFreshDbInvalidationRouteSeal {/,/^}/p' "$$production_dispatch" )"; \
	timeout 10s test "$$( timeout 10s grep -Fc '    fn new() -> Self {' <<<"$$seal_impl" )" = 1; \
	timeout 10s test "$$( timeout 10s grep -Ec '^[[:space:]]+pub\(in crate::client\) fn ' <<<"$$seal_impl" )" = 1; \
	timeout 10s test "$$( timeout 10s grep -Fc '    #[cfg(test)]' <<<"$$seal_impl" )" = 1; \
	timeout 10s grep -Fq 'pub(in crate::client) fn new_for_test() -> Self {' <<<"$$seal_impl"; \
	timeout 10s test "$$( timeout 10s grep -Fc 'UsrRollbackFreshDbInvalidationRouteSeal::new();' "$$production_dispatch" )" = 1; \
	seal_production_calls="$$( timeout 10s rg -n -F 'UsrRollbackFreshDbInvalidationRouteSeal::new();' crates/forge/src/client --glob '*.rs' --glob '!**/tests/**' --glob '!**/tests.rs' --glob '!**/*_tests.rs' --glob '!**/*_tests/**' )"; \
	timeout 10s test "$$( timeout 10s grep -c . <<<"$$seal_production_calls" )" = 1; \
	timeout 10s test "$$( timeout 10s cut -d: -f1 <<<"$$seal_production_calls" )" = "$$production_dispatch"; \
	timeout 10s grep -Fqx '        _startup_gate_seal: &UsrRollbackFreshDbInvalidationRouteSeal,' "$$authority"; \
	timeout 10s grep -Fq 'let seal = UsrRollbackFreshDbInvalidationRouteSeal::new_for_test();' "$$support"; \
	admission_success="$$( timeout 10s sed -n '/^fn startup_usr_rollback_fresh_db_invalidation_route_admits_exact_current_and_historical_evidence/,/^}/p' "$$admission" )"; \
	route_success="$$( timeout 10s sed -n '/^fn exercise_success_matrix/,/^}/p' "$$matrix" )"; \
	for success_matrix in "$$admission_success" "$$route_success"; do \
		for axis in 'for historical in [false, true] {' 'for source in CandidateSource::THROUGH_CANDIDATE_PRESERVED {' 'for usr_outcome in [RollbackActionOutcome::Applied, RollbackActionOutcome::AlreadySatisfied] {' 'for candidate_outcome in CandidateOutcome::ALL {'; do \
			timeout 10s test "$$( timeout 10s grep -Fc "$$axis" <<<"$$success_matrix" )" = 1; \
		done; \
	done; \
	timeout 10s test "$$( timeout 10s grep -Fc 'exercise_success_matrix(CandidateOutcome::' "$$matrix" )" = 2; \
	timeout 10s grep -Fqx '    exercise_success_matrix(CandidateOutcome::Applied);' "$$matrix"; \
	timeout 10s grep -Fqx '    exercise_success_matrix(CandidateOutcome::AlreadySatisfied);' "$$matrix"; \
	timeout 10s grep -Fq '.audit_in_flight_transition()' <<<"$$admission_success"; \
	timeout 10s grep -Fq '.metadata_provenance(fixture.fixture.fixture.candidate_state)' <<<"$$admission_success"; \
	storage_fault_matrix="$$( timeout 10s sed -n '/^fn startup_usr_rollback_fresh_db_invalidation_route_storage_faults_reopen_exact_source_or_successor/,/^}/p' "$$storage" )"; \
	for axis in 'for historical in [false, true] {' 'for source in CandidateSource::THROUGH_CANDIDATE_PRESERVED {' 'for usr_outcome in [RollbackActionOutcome::Applied, RollbackActionOutcome::AlreadySatisfied] {' 'for candidate_outcome in CandidateOutcome::ALL {'; do \
		timeout 10s test "$$( timeout 10s grep -Fc "$$axis" <<<"$$storage_fault_matrix" )" = 1; \
	done; \
	timeout 10s grep -Fq 'let cases: [(fn(), fn(), DurableUsrRollbackFreshDbInvalidationRouteRecord); 5] = [' <<<"$$storage_fault_matrix"; \
	for constructor in arm_next_temporary_sync_fault arm_next_update_exchange_fault arm_next_update_first_directory_sync_fault arm_next_displaced_unlink_fault arm_next_update_final_directory_sync_fault; do \
		timeout 10s test "$$( timeout 10s grep -Fc "            $$constructor," <<<"$$storage_fault_matrix" )" = 1; \
	done; \
	bound_advance_matrix="$$( timeout 10s sed -n '/^fn startup_usr_rollback_fresh_db_invalidation_route_bound_advance_same_byte_replacements_never_succeed/,/^}/p' "$$binding" )"; \
	same_store_matrix="$$( timeout 10s sed -n '/^fn startup_usr_rollback_fresh_db_invalidation_route_same_byte_successor_replacement_fails_same_store_binding/,/^}/p' "$$binding" )"; \
	reopened_binding_matrix="$$( timeout 10s sed -n '/^fn startup_usr_rollback_fresh_db_invalidation_route_same_byte_successor_replacement_fails_reopened_binding/,/^}/p' "$$binding" )"; \
	for binding_matrix in "$$bound_advance_matrix" "$$same_store_matrix" "$$reopened_binding_matrix"; do \
		for axis in 'for historical in [false, true] {' 'for source in CandidateSource::THROUGH_CANDIDATE_PRESERVED {' 'for usr_outcome in [RollbackActionOutcome::Applied, RollbackActionOutcome::AlreadySatisfied] {' 'for candidate_outcome in CandidateOutcome::ALL {'; do \
			timeout 10s test "$$( timeout 10s grep -Fc "$$axis" <<<"$$binding_matrix" )" = 1; \
		done; \
	done; \
	timeout 10s test "$$( timeout 10s grep -Fc 'PublicBindingRevalidationBoundary::BeforeBoundAdvancePublish,' <<<"$$bound_advance_matrix" )" = 1; \
	timeout 10s test "$$( timeout 10s grep -Fc 'PublicBindingRevalidationBoundary::BeforeBoundAdvanceFinalBinding,' <<<"$$bound_advance_matrix" )" = 1; \
	timeout 10s test "$$( timeout 10s grep -Fc 'arm_before_usr_rollback_fresh_db_invalidation_route_successor_binding_revalidation(hook);' <<<"$$same_store_matrix" )" = 1; \
	timeout 10s test "$$( timeout 10s grep -Fc 'arm_after_usr_rollback_fresh_db_invalidation_route_successor_binding_check_before_reopen(hook);' <<<"$$reopened_binding_matrix" )" = 1; \
	source_reopen_matrix="$$( timeout 10s sed -n '/^fn startup_usr_rollback_fresh_db_invalidation_route_source_durable_fresh_handle_reopen_retries_only_the_route/,/^}/p' "$$fresh_reopen" )"; \
	successor_reopen_matrix="$$( timeout 10s sed -n '/^fn startup_usr_rollback_fresh_db_invalidation_route_successor_durable_fresh_handle_reopen_skips_the_route/,/^}/p' "$$fresh_reopen" )"; \
	for fresh_handle_matrix in "$$source_reopen_matrix" "$$successor_reopen_matrix"; do \
		for axis in 'for historical in [false, true] {' 'for source in CandidateSource::THROUGH_CANDIDATE_PRESERVED {' 'for usr_outcome in [RollbackActionOutcome::Applied, RollbackActionOutcome::AlreadySatisfied] {' 'for candidate_outcome in CandidateOutcome::ALL {'; do \
			timeout 10s test "$$( timeout 10s grep -Fc "$$axis" <<<"$$fresh_handle_matrix" )" = 1; \
		done; \
		for marker in 'fixture.install_persistent_database();' 'let retained = fixture.release_handles();' 'let fresh = FreshRouteHandles::open(retained.path());'; do \
			timeout 10s test "$$( timeout 10s grep -Fc "$$marker" <<<"$$fresh_handle_matrix" )" = 1; \
		done; \
	done; \
	timeout 10s grep -Fq 'DurableUsrRollbackFreshDbInvalidationRouteRecord::CandidatePreserved' <<<"$$source_reopen_matrix"; \
	timeout 10s grep -Fq 'assert_eq!(fresh.record, expected_source);' <<<"$$source_reopen_matrix"; \
	timeout 10s grep -Fq 'let authority = fresh.capture_ready(&reservation);' <<<"$$source_reopen_matrix"; \
	timeout 10s grep -Fq 'DurableUsrRollbackFreshDbInvalidationRouteRecord::FreshDbInvalidationIntent' <<<"$$successor_reopen_matrix"; \
	timeout 10s grep -Fq 'assert_eq!(fresh.record, expected);' <<<"$$successor_reopen_matrix"; \
	timeout 10s grep -Fq 'UsrRollbackFreshDbInvalidationRouteAdmission::NotApplicable' <<<"$$successor_reopen_matrix"; \
	timeout 10s test $$((2 * 3 * 2 * 2)) = 24; \
	timeout 10s test $$((24 * 5)) = 120; \
	timeout 10s test $$((24 * 2)) = 48; \
	timeout 10s test $$((24 * 4)) = 96; \
	timeout 10s grep -Fq 'for historical in [false, true] {' "$$endpoint"; \
	timeout 10s grep -Fq 'for candidate_outcome in CandidateOutcome::ALL {' "$$endpoint"; \
	timeout 10s grep -Fq 'assert_eq!(candidate_preserved.generation, 15' "$$endpoint"; \
	timeout 10s grep -Fq 'assert_eq!(invalidation_intent.generation, 16' "$$endpoint"; \
	timeout 10s grep -Fq '["bin", "sbin", "lib", "lib32", "lib64"]' "$$endpoint"; \
	timeout 10s test "$$( timeout 10s grep -Fc 'retained_exchange_syscall_count(), 1' "$$endpoint" )" -ge 3; \
	timeout 10s grep -Fq 'fresh_db_invalidation_removal_call_count(), removal_before' "$$endpoint"; \
	timeout 10s grep -Fq 'boot_synchronize_attempt_count(), 0' "$$endpoint"; \
	timeout 10s grep -Fq 'assert_eq!(fixture.canonical_bytes(), intent_bytes' "$$endpoint"; \
	for race in Database Provenance Journal Installation Namespace; do timeout 10s grep -Fq "FinalRace::$$race" "$$races"; done; \
	timeout 10s grep -Fq 'arm_between_usr_rollback_fresh_db_invalidation_route_database_captures' "$$races"; \
	timeout 10s grep -Fq 'arm_before_usr_rollback_fresh_db_invalidation_route_fresh_namespace_capture' "$$races"; \
	capture_root_matrix="$$( timeout 10s sed -n '/fn startup_root_links_fresh_db_route_capture_rejects_all_root_abi_mutations/,/fn startup_root_links_fresh_db_route_final_revalidation_rejects_all_root_abi_mutations/p' "$$races" )"; \
	final_root_matrix="$$( timeout 10s sed -n '/fn startup_root_links_fresh_db_route_final_revalidation_rejects_all_root_abi_mutations/,/fn startup_usr_rollback_fresh_db_invalidation_route_rejects_mixed_and_cross_root_journals/p' "$$races" )"; \
	for root_matrix in "$$capture_root_matrix" "$$final_root_matrix"; do \
		for axis in 'for historical in [false, true] {' 'for usr_outcome in [RollbackActionOutcome::Applied, RollbackActionOutcome::AlreadySatisfied] {' 'for candidate_outcome in CandidateOutcome::ALL {' 'for (name, target) in ROOT_ABI {' 'for mutation in RootAbiMutation::ALL {'; do \
			timeout 10s test "$$( timeout 10s grep -Fc "$$axis" <<<"$$root_matrix" )" = 1; \
		done; \
		timeout 10s test "$$( timeout 10s grep -Fc 'assert_exact_root_abi_mutation(' <<<"$$root_matrix" )" = 1; \
		timeout 10s test "$$( timeout 10s grep -Fc '&root_abi_snapshot(&root),' <<<"$$root_matrix" )" = 1; \
	done; \
	timeout 10s grep -Fq 'const ROOT_ABI: [(&str, &str); 5]' "$$races"; \
	root_abi_assertion="$$( timeout 10s sed -n '/^fn assert_exact_root_abi_mutation(/,/^}/p' "$$races" )"; \
	timeout 10s test "$$( timeout 10s grep -Fc 'fn assert_exact_root_abi_mutation(' "$$races" )" = 1; \
	timeout 10s test "$$( timeout 10s grep -Fc 'assert_exact_root_abi_mutation(' "$$races" )" = 3; \
	timeout 10s test "$$( timeout 10s grep -Fc 'if index != selected_index {' <<<"$$root_abi_assertion" )" = 1; \
	timeout 10s test "$$( timeout 10s grep -Fc '.tempdir_in(root.parent().unwrap())' "$$races" )" = 1; \
	timeout 10s test "$$( timeout 10s grep -Fc 'assert!(!displaced_directory.path().starts_with(&root));' "$$races" )" = 1; \
	if timeout 10s rg -n 'let displaced[[:space:]]*=[[:space:]]*root\.join' "$$races"; then exit 1; fi; \
	for mutation in Missing WrongTarget SameTargetDifferentInode; do timeout 10s grep -Fq "RootAbiMutation::$$mutation" "$$races"; done; \
	timeout 10s test $$((2 * 2 * 2 * 5 * 3)) = 120; \
	timeout 10s test $$((120 * 2)) = 240; \
	if timeout 10s rg -n 'process|fork|Command::|kill\(' "$$fresh_reopen"; then exit 1; fi; \
	for file in "$$executor" "$$authority" "$$proof" "$$topology" "$$reopen" "$$recovery_root" "$$reconciliation_root" "$$activation_root" "$$startup_gate" "$$production_dispatch" "$$downstream" "$$complete_route" "$$finalization" "$$tests" "$$support" "$$admission" "$$endpoint" "$$matrix" "$$races" "$$binding" "$$storage" "$$fresh_reopen" misc/make/startup-fresh-db-invalidation-route-tests.mk Makefile; do \
		timeout 10s test "$$( timeout 10s wc -l < "$$file" )" -le 1000; \
	done; \
	timeout 1200s $(CARGO) test -p forge --lib "$$prefix" -- --test-threads=1
