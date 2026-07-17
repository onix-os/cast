.PHONY: forge-startup-usr-rollback-fresh-db-invalidation-route-test

forge-startup-usr-rollback-fresh-db-invalidation-route-test:
	@set -eu; \
	listed="$$( timeout 300s $(CARGO) test -p forge --lib -- --list )"; \
	timeout 10s grep -q . <<<"$$listed"; \
	prefix='client::startup_recovery::usr_rollback_fresh_db_invalidation_route::tests::'; \
	count="$$( timeout 10s grep -c "^$$prefix.*: test$$" <<<"$$listed" )"; \
	timeout 10s test "$$count" = 11; \
	for name in \
		admission::startup_usr_rollback_fresh_db_invalidation_route_admits_exact_current_and_historical_evidence \
		admission::startup_usr_rollback_fresh_db_invalidation_route_defers_inexact_phase_plan_database_and_provenance \
		evidence_races::startup_usr_rollback_fresh_db_invalidation_route_rejects_mixed_and_cross_root_journals \
		evidence_races::startup_usr_rollback_fresh_db_invalidation_route_capture_and_final_evidence_races_never_advance \
		evidence_races::startup_usr_rollback_fresh_db_invalidation_route_refuses_namespace_lookalikes \
		matrix::startup_usr_rollback_fresh_db_invalidation_route_applied_matrix_persists_exact_intent \
		matrix::startup_usr_rollback_fresh_db_invalidation_route_finish_matrix_persists_exact_intent \
		storage_reopen::startup_usr_rollback_fresh_db_invalidation_route_storage_faults_reopen_exact_source_or_successor \
		storage_reopen::startup_usr_rollback_fresh_db_invalidation_route_consumes_old_store_and_returns_canonical_reopen \
		restart::startup_usr_rollback_fresh_db_invalidation_route_source_fault_restart_retries_only_the_route \
		restart::startup_usr_rollback_fresh_db_invalidation_route_successor_fault_restart_skips_the_route; do \
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
	tests=crates/forge/src/client/startup_recovery/usr_rollback_fresh_db_invalidation_route/tests.rs; \
	support=crates/forge/src/client/startup_recovery/usr_rollback_fresh_db_invalidation_route/tests/support.rs; \
	admission=crates/forge/src/client/startup_recovery/usr_rollback_fresh_db_invalidation_route/tests/admission.rs; \
	matrix=crates/forge/src/client/startup_recovery/usr_rollback_fresh_db_invalidation_route/tests/matrix.rs; \
	races=crates/forge/src/client/startup_recovery/usr_rollback_fresh_db_invalidation_route/tests/evidence_races.rs; \
	storage=crates/forge/src/client/startup_recovery/usr_rollback_fresh_db_invalidation_route/tests/storage_reopen.rs; \
	restart=crates/forge/src/client/startup_recovery/usr_rollback_fresh_db_invalidation_route/tests/restart.rs; \
	timeout 10s grep -Fqx 'mod usr_rollback_fresh_db_invalidation_route;' "$$recovery_root"; \
	timeout 10s test "$$( timeout 10s grep -Fc 'mod usr_rollback_fresh_db_invalidation_route;' "$$recovery_root" )" = 1; \
	timeout 10s grep -Fqx 'mod usr_rollback_fresh_db_invalidation_route_authority;' "$$reconciliation_root"; \
	timeout 10s grep -Fqx 'mod fresh_db_invalidation_route_proof;' "$$activation_root"; \
	timeout 10s rg -U -q '^pub\(in crate::client\) fn persist_usr_rollback_fresh_db_invalidation_route_and_reopen\(\n    journal: TransitionJournalStore,\n    authority: UsrRollbackFreshDbInvalidationRouteAuthority<'\''_>,\n\) -> Result<\(TransitionJournalStore, TransitionRecord\), UsrRollbackFreshDbInvalidationRoutePersistenceError> \{' "$$executor"; \
	timeout 10s test "$$( timeout 10s grep -Fc 'authority.revalidate(&journal)' "$$executor" )" = 2; \
	timeout 10s test "$$( timeout 10s rg -n '\.rollback_successor\(None\)' "$$executor" "$$authority" "$$proof" "$$topology" | timeout 10s wc -l )" = 1; \
	timeout 10s grep -Fqx '    let successor = match source_record.rollback_successor(None) {' "$$executor"; \
	timeout 10s grep -Fqx '        Ok(successor) if successor.phase == Phase::FreshDbInvalidationIntent => successor,' "$$executor"; \
	timeout 10s test "$$( timeout 10s rg -n '\.advance\(' "$$executor" "$$authority" "$$proof" "$$topology" "$$reopen" | timeout 10s wc -l )" = 1; \
	timeout 10s grep -Fqx '    let advance = journal.advance(&source_record, &successor);' "$$executor"; \
	timeout 10s test "$$( timeout 10s grep -Fc 'persist_usr_rollback_fresh_db_invalidation_route_and_reopen(' "$$executor" )" = 1; \
	timeout 10s grep -Fqx '    UsrRollbackFreshDbInvalidationRoutePersistenceError, persist_usr_rollback_fresh_db_invalidation_route_and_reopen,' "$$recovery_root"; \
	timeout 10s test "$$( timeout 10s grep -Fc 'persist_usr_rollback_fresh_db_invalidation_route_and_reopen,' "$$recovery_root" )" = 1; \
	timeout 10s test "$$( timeout 10s grep -Fc 'persist_usr_rollback_fresh_db_invalidation_route_and_reopen(journal, authority)?' "$$production_dispatch" )" = 1; \
	timeout 10s test "$$( timeout 10s grep -Fc 'UsrRollbackFreshDbInvalidationRouteAuthority::capture(' "$$production_dispatch" )" = 1; \
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
	unexpected_authority_file="$$( timeout 10s cut -d: -f1 <<<"$$authority_references" | timeout 10s grep -Fvx -e "$$reconciliation_root" -e "$$executor" -e "$$production_dispatch" || true )"; \
	timeout 10s test -z "$$unexpected_authority_file"; \
	first_revalidate_line="$$( timeout 10s grep -nF 'authority.revalidate(&journal)' "$$executor" | timeout 10s head -n 1 | timeout 10s cut -d: -f1 )"; \
	successor_line="$$( timeout 10s grep -nF '    let successor = match source_record.rollback_successor(None) {' "$$executor" | timeout 10s cut -d: -f1 )"; \
	seam_line="$$( timeout 10s grep -nF '    before_usr_rollback_fresh_db_invalidation_route_final_revalidation();' "$$executor" | timeout 10s cut -d: -f1 )"; \
	final_revalidate_line="$$( timeout 10s grep -nF 'authority.revalidate(&journal)' "$$executor" | timeout 10s tail -n 1 | timeout 10s cut -d: -f1 )"; \
	clone_line="$$( timeout 10s grep -nF '    let installation = authority.installation().clone();' "$$executor" | timeout 10s cut -d: -f1 )"; \
	advance_line="$$( timeout 10s grep -nF '    let advance = journal.advance(&source_record, &successor);' "$$executor" | timeout 10s cut -d: -f1 )"; \
	drop_authority_line="$$( timeout 10s grep -nF '    drop(authority);' "$$executor" | timeout 10s tail -n 1 | timeout 10s cut -d: -f1 )"; \
	drop_journal_line="$$( timeout 10s grep -nF '    drop(journal);' "$$executor" | timeout 10s tail -n 1 | timeout 10s cut -d: -f1 )"; \
	reopen_line="$$( timeout 10s grep -nF 'reopen_canonical_journal(&installation).map_err(UsrRollbackFreshDbInvalidationRouteReopenError::from);' "$$executor" | timeout 10s cut -d: -f1 )"; \
	timeout 10s test "$$first_revalidate_line" -lt "$$successor_line"; \
	timeout 10s test "$$successor_line" -lt "$$seam_line"; \
	timeout 10s test "$$seam_line" -lt "$$final_revalidate_line"; \
	timeout 10s test "$$final_revalidate_line" -lt "$$clone_line"; \
	timeout 10s test "$$clone_line" -lt "$$advance_line"; \
	timeout 10s test "$$advance_line" -lt "$$drop_authority_line"; \
	timeout 10s test "$$drop_authority_line" -lt "$$drop_journal_line"; \
	timeout 10s test "$$drop_journal_line" -lt "$$reopen_line"; \
	suffix="$$( timeout 10s sed -n '/    let advance = journal.advance(&source_record, &successor);/,/reopen_canonical_journal(&installation)/p' "$$executor" )"; \
	timeout 10s test "$$( timeout 10s grep -Fc '    drop(authority);' <<<"$$suffix" )" = 1; \
	timeout 10s test "$$( timeout 10s grep -Fc '    drop(journal);' <<<"$$suffix" )" = 1; \
	timeout 10s test "$$( timeout 10s grep -Fc 'reopen_canonical_journal(&installation)' <<<"$$suffix" )" = 1; \
	if timeout 10s rg -n 'retained_mutable_cast_directory|open_in_retained_cast|journal\.load\(' "$$executor"; then exit 1; fi; \
	timeout 10s grep -Fqx '                    durable: DurableUsrRollbackFreshDbInvalidationRouteRecord::CandidatePreserved,' "$$executor"; \
	timeout 10s grep -Fqx '                    durable: DurableUsrRollbackFreshDbInvalidationRouteRecord::FreshDbInvalidationIntent,' "$$executor"; \
	timeout 10s test "$$( timeout 10s grep -Fc '            Ok((reopened, Some(actual))) if actual == source_record => {' "$$executor" )" = 1; \
	timeout 10s test "$$( timeout 10s grep -Fc '            Ok((reopened, Some(actual))) if actual == successor => {' "$$executor" )" = 1; \
	timeout 10s grep -Fqx '            Ok((reopened, Some(actual))) if actual == successor => Ok((reopened, successor)),' "$$executor"; \
	timeout 10s grep -Fqx '            CanonicalJournalReopenError::Installation(source) => Self::Installation(source),' "$$executor"; \
	timeout 10s grep -Fqx '            CanonicalJournalReopenError::Journal(source) => Self::Journal(source),' "$$executor"; \
	timeout 10s grep -Fq 'DatabaseEvidence::CandidateOwnership {' "$$authority"; \
	timeout 10s grep -Fq 'ownership: db::state::TransitionOwnership::Matching' "$$authority"; \
	timeout 10s grep -Fq 'provenance: Some(_)' "$$authority"; \
	timeout 10s grep -Fq 'record.phase == Phase::CandidatePreserved' "$$authority"; \
	timeout 10s grep -Fq 'rollback.fresh_db == RollbackAction::Pending' "$$authority"; \
	timeout 10s grep -Fq 'require_exact_new_state_candidate_preserved_topology' "$$proof"; \
	timeout 10s test "$$( timeout 10s grep -Fc 'require_exact_new_state_candidate_preserved_topology(' "$$proof" )" = 6; \
	timeout 10s rg -U -q '^pub\(in crate::client::startup_reconciliation::activation_namespace\) fn require_exact_new_state_candidate_preserved_topology\(' "$$topology"; \
	topology_helper="$$( timeout 10s sed -n '/^pub(in crate::client::startup_reconciliation::activation_namespace) fn require_exact_new_state_candidate_preserved_topology/,/^}/p' "$$topology" )"; \
	timeout 10s test -n "$$topology_helper"; \
	production_code="$$( timeout 10s sed -E 's,//.*$$,,' "$$executor" "$$authority" "$$proof"; timeout 10s sed -E 's,//.*$$,,' <<<"$$topology_helper" )"; \
	if timeout 10s rg -n 'clear_transition_if_matches|remove_transition_if_matches|insert_fresh_metadata|delete_metadata|invalidate(_|[[:space:]]*\()|\.add\(|\.create\(|\.remove\(|\.batch_remove\(|\.delete\(' <<<"$$production_code"; then exit 1; fi; \
	if timeout 10s rg -n 'diesel::|SqliteConnection|sql_query|\.execute\(|\.transaction\(' <<<"$$production_code"; then exit 1; fi; \
	if timeout 10s rg -n 'renameat|std::fs|(^|[^_[:alnum:]])fs::|rename[[:space:]]*\(|unlink(at)?[[:space:]]*\(|linkat[[:space:]]*\(|sync_(all|data)|write_all|set_permissions|chmod|create_dir|remove_(dir|file)|hard_link|symlink|attempt_move|reconcile_move' <<<"$$production_code"; then exit 1; fi; \
	if timeout 10s rg -n 'run_transaction_triggers|run_system_triggers|root_links|archive_previous|rearchive_archived|preserve_failed|remove_exact_archived|cleanup|retry|dispatch' <<<"$$production_code"; then exit 1; fi; \
	if timeout 10s rg -n '^[[:space:]]*(loop|while|for)[[:space:]]|=[[:space:]]*(loop|while|for)[[:space:]]' <<<"$$production_code"; then exit 1; fi; \
	timeout 10s test "$$( timeout 10s rg -l '^pub\(in crate::client\) struct UsrRollbackFreshDbInvalidationRouteSeal \{' crates/forge/src/client --glob '*.rs' )" = "$$production_dispatch"; \
	timeout 10s grep -Fq '    UsrRollbackCandidatePreserveSeal, UsrRollbackCompleteRouteSeal, UsrRollbackFreshDbInvalidationRouteSeal,' "$$startup_gate"; \
	timeout 10s grep -Fqx 'pub(in crate::client) struct UsrRollbackFreshDbInvalidationRouteSeal {' "$$production_dispatch"; \
	timeout 10s awk '$$0 == "pub(in crate::client) struct UsrRollbackFreshDbInvalidationRouteSeal {" { state = 1; next } state == 1 && $$0 == "    _private: ()," { state = 2; next } state == 2 && $$0 == "}" { found = 1 } END { exit !found }' "$$production_dispatch"; \
	seal_impl="$$( timeout 10s sed -n '/^impl UsrRollbackFreshDbInvalidationRouteSeal {/,/^}/p' "$$production_dispatch" )"; \
	timeout 10s test "$$( timeout 10s grep -Ec '^[[:space:]]+pub\(in crate::client\) fn ' <<<"$$seal_impl" )" = 1; \
	timeout 10s test "$$( timeout 10s grep -Fc '    #[cfg(test)]' <<<"$$seal_impl" )" = 1; \
	timeout 10s grep -Fq 'pub(in crate::client) fn new_for_test() -> Self {' <<<"$$seal_impl"; \
	timeout 10s test "$$( timeout 10s grep -Fc 'UsrRollbackFreshDbInvalidationRouteSeal::new();' "$$production_dispatch" )" = 1; \
	seal_production_calls="$$( timeout 10s rg -n -F 'UsrRollbackFreshDbInvalidationRouteSeal::new();' crates/forge/src/client --glob '*.rs' --glob '!**/tests/**' --glob '!**/tests.rs' --glob '!**/*_tests.rs' --glob '!**/*_tests/**' )"; \
	timeout 10s test "$$( timeout 10s grep -c . <<<"$$seal_production_calls" )" = 1; \
	timeout 10s test "$$( timeout 10s cut -d: -f1 <<<"$$seal_production_calls" )" = "$$production_dispatch"; \
	timeout 10s grep -Fqx '        _startup_gate_seal: &UsrRollbackFreshDbInvalidationRouteSeal,' "$$authority"; \
	timeout 10s grep -Fq 'let seal = UsrRollbackFreshDbInvalidationRouteSeal::new_for_test();' "$$support"; \
	timeout 10s test "$$( timeout 10s grep -Fc 'for source in CandidateSource::ALL {' "$$matrix" )" = 1; \
	timeout 10s test "$$( timeout 10s grep -Fc 'exercise_success_matrix(CandidateOutcome::' "$$matrix" )" = 2; \
	timeout 10s grep -Fq 'for historical in [false, true] {' "$$admission"; \
	timeout 10s grep -Fq '.audit_in_flight_transition()' "$$admission"; \
	timeout 10s grep -Fq '.metadata_provenance(fixture.fixture.fixture.candidate_state)' "$$admission"; \
	timeout 10s test "$$( timeout 10s rg -n '^            arm_next_(temporary_sync|update_exchange|update_first_directory_sync|displaced_unlink|update_final_directory_sync)_fault,$$' "$$storage" | timeout 10s wc -l )" = 5; \
	timeout 10s test "$$( timeout 10s grep -Fc 'for case in 0..3 {' "$$races" )" = 1; \
	for race in Database Provenance Journal Installation Namespace; do timeout 10s grep -Fq "FinalRace::$$race" "$$races"; done; \
	timeout 10s grep -Fq 'arm_between_usr_rollback_fresh_db_invalidation_route_database_captures' "$$races"; \
	timeout 10s grep -Fq 'arm_before_usr_rollback_fresh_db_invalidation_route_fresh_namespace_capture' "$$races"; \
	timeout 10s test "$$( timeout 10s grep -Fc 'drop(reservation);' "$$restart" )" = 2; \
	timeout 10s grep -Fq 'DurableUsrRollbackFreshDbInvalidationRouteRecord::CandidatePreserved' "$$restart"; \
	timeout 10s grep -Fq 'DurableUsrRollbackFreshDbInvalidationRouteRecord::FreshDbInvalidationIntent' "$$restart"; \
	for file in "$$executor" "$$authority" "$$proof" "$$topology" "$$reopen" "$$recovery_root" "$$reconciliation_root" "$$activation_root" "$$tests" "$$support" "$$admission" "$$matrix" "$$races" "$$storage" "$$restart" misc/make/startup-fresh-db-invalidation-route-tests.mk Makefile; do \
		timeout 10s test "$$( timeout 10s wc -l < "$$file" )" -le 1000; \
	done; \
	timeout 1200s $(CARGO) test -p forge --lib "$$prefix" -- --test-threads=1
