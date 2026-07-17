.PHONY: forge-startup-usr-rollback-complete-route-test

forge-startup-usr-rollback-complete-route-test:
	@set -euo pipefail; \
	timeout 10s mkdir -p "$(TOP_DIR)/target"; \
	listed="$$( timeout 10s mktemp "$(TOP_DIR)/target/rollback-complete-route-list.XXXXXXXXXXXX" )"; \
	production_code="$$( timeout 10s mktemp "$(TOP_DIR)/target/rollback-complete-route-code.XXXXXXXXXXXX" )"; \
	symbol_refs="$$( timeout 10s mktemp "$(TOP_DIR)/target/rollback-complete-route-symbols.XXXXXXXXXXXX" )"; \
	capture_body="$$( timeout 10s mktemp "$(TOP_DIR)/target/rollback-complete-route-capture.XXXXXXXXXXXX" )"; \
	revalidate_body="$$( timeout 10s mktemp "$(TOP_DIR)/target/rollback-complete-route-revalidate.XXXXXXXXXXXX" )"; \
	inspection_body="$$( timeout 10s mktemp "$(TOP_DIR)/target/rollback-complete-route-inspection.XXXXXXXXXXXX" )"; \
	trap 'timeout 10s rm -f "$$listed" "$$production_code" "$$symbol_refs" "$$capture_body" "$$revalidate_body" "$$inspection_body"' EXIT; \
	timeout 300s $(CARGO) test -p forge --lib -- --list | timeout 30s tee "$$listed" >/dev/null; \
	timeout 10s grep -q . "$$listed"; \
	prefix='client::startup_recovery::usr_rollback_complete_route::tests::'; \
	count="$$( timeout 10s awk -v prefix="$$prefix" 'index($$0, prefix) == 1 && $$0 ~ /: test$$/ { count += 1 } END { print count + 0 }' "$$listed" )"; \
	timeout 10s test "$$count" = 11; \
	for name in \
		admission::startup_usr_rollback_complete_route_admits_exact_current_and_historical_joint_absence \
		admission::startup_usr_rollback_complete_route_defers_inexact_phase_plan_and_non_absent_database \
		evidence_races::startup_usr_rollback_complete_route_rejects_reopened_and_cross_root_journals \
		evidence_races::startup_usr_rollback_complete_route_capture_and_final_evidence_races_never_advance \
		evidence_races::startup_usr_rollback_complete_route_refuses_namespace_lookalikes \
		matrix::startup_usr_rollback_complete_route_applied_matrix_persists_exact_rollback_complete \
		matrix::startup_usr_rollback_complete_route_already_satisfied_matrix_persists_exact_rollback_complete \
		storage_reopen::startup_usr_rollback_complete_route_storage_faults_reopen_exact_fresh_db_invalidated_or_rollback_complete \
		storage_reopen::startup_usr_rollback_complete_route_consumes_old_store_and_returns_canonical_reopen \
		restart::startup_usr_rollback_complete_route_source_fault_restart_retries_only_the_completion_route \
		restart::startup_usr_rollback_complete_route_rollback_complete_fault_restart_skips_route_and_invalidation; do \
		timeout 10s grep -Fqx "$$prefix$$name: test" "$$listed"; \
	done; \
	executor=crates/forge/src/client/startup_recovery/usr_rollback_complete_route.rs; \
	authority=crates/forge/src/client/startup_reconciliation/usr_rollback_complete_route_authority.rs; \
	proof=crates/forge/src/client/startup_reconciliation/activation_namespace/rollback_complete_route_proof.rs; \
	candidate_proof=crates/forge/src/client/startup_reconciliation/activation_namespace/candidate_preserve_proof.rs; \
	exact=crates/forge/src/db/state/exact_fresh_transition_removal.rs; \
	reopen=crates/forge/src/client/startup_recovery/canonical_journal_reopen.rs; \
	startup_gate=crates/forge/src/client/startup_gate.rs; \
	recovery_root=crates/forge/src/client/startup_recovery.rs; \
	reconciliation_root=crates/forge/src/client/startup_reconciliation.rs; \
	production_dispatch=crates/forge/src/client/startup_gate/usr_rollback_new_state.rs; \
	namespace_root=crates/forge/src/client/startup_reconciliation/activation_namespace.rs; \
	tests=crates/forge/src/client/startup_recovery/usr_rollback_complete_route/tests.rs; \
	support=crates/forge/src/client/startup_recovery/usr_rollback_complete_route/tests/support.rs; \
	admission=crates/forge/src/client/startup_recovery/usr_rollback_complete_route/tests/admission.rs; \
	races=crates/forge/src/client/startup_recovery/usr_rollback_complete_route/tests/evidence_races.rs; \
	matrix=crates/forge/src/client/startup_recovery/usr_rollback_complete_route/tests/matrix.rs; \
	storage=crates/forge/src/client/startup_recovery/usr_rollback_complete_route/tests/storage_reopen.rs; \
	restart=crates/forge/src/client/startup_recovery/usr_rollback_complete_route/tests/restart.rs; \
	timeout 10s grep -Fqx 'mod usr_rollback_complete_route;' "$$recovery_root"; \
	timeout 10s test "$$( timeout 10s grep -Fc 'mod usr_rollback_complete_route;' "$$recovery_root" )" = 1; \
	timeout 10s grep -Fqx 'mod usr_rollback_complete_route_authority;' "$$reconciliation_root"; \
	timeout 10s test "$$( timeout 10s grep -Fc 'mod usr_rollback_complete_route_authority;' "$$reconciliation_root" )" = 1; \
	timeout 10s grep -Fqx 'mod rollback_complete_route_proof;' "$$namespace_root"; \
	timeout 10s test "$$( timeout 10s grep -Fc 'mod rollback_complete_route_proof;' "$$namespace_root" )" = 1; \
	for module in admission evidence_races matrix restart storage_reopen support; do \
		timeout 10s grep -Fqx "mod $$module;" "$$tests"; \
	done; \
	timeout 10s test "$$( timeout 10s grep -Ec '^mod [a-z_]+;' "$$tests" )" = 6; \
	timeout 10s rg -U -q '^pub\(in crate::client\) fn persist_usr_rollback_complete_route_and_reopen\(\n    journal: TransitionJournalStore,\n    authority: UsrRollbackCompleteRouteAuthority<'\''_>,\n\) -> Result<\(TransitionJournalStore, TransitionRecord\), UsrRollbackCompleteRoutePersistenceError> \{' "$$executor"; \
	timeout 10s grep -Fqx '    UsrRollbackCompleteRoutePersistenceError, persist_usr_rollback_complete_route_and_reopen,' "$$recovery_root"; \
	timeout 10s test "$$( timeout 10s grep -Fc 'persist_usr_rollback_complete_route_and_reopen,' "$$recovery_root" )" = 1; \
	timeout 10s test "$$( timeout 10s grep -Fc 'persist_usr_rollback_complete_route_and_reopen(journal, authority)?' "$$production_dispatch" )" = 1; \
	timeout 10s rg -n -F 'persist_usr_rollback_complete_route_and_reopen' crates/forge/src/client --glob '*.rs' --glob '!**/tests/**' --glob '!**/tests.rs' --glob '!**/*_tests.rs' --glob '!**/*_tests/**' --glob '!**/usr_rollback_complete_route.rs' > "$$symbol_refs"; \
	timeout 10s test "$$( timeout 10s wc -l < "$$symbol_refs" )" = 3; \
	timeout 10s test "$$( timeout 10s grep -Fc "$$recovery_root:" "$$symbol_refs" )" = 1; \
	timeout 10s test "$$( timeout 10s grep -Fc "$$production_dispatch:" "$$symbol_refs" )" = 2; \
	timeout 10s test "$$( timeout 10s grep -Fc 'persist_usr_rollback_complete_route_and_reopen(' "$$executor" )" = 1; \
	timeout 10s grep -Fqx 'pub(in crate::client) enum UsrRollbackCompleteRouteAdmission<'\''reservation> {' "$$authority"; \
	for variant in '    NotApplicable,' '    Deferred,' '    Ready(UsrRollbackCompleteRouteAuthority<'\''reservation>),'; do \
		timeout 10s grep -Fqx "$$variant" "$$authority"; \
	done; \
	timeout 10s test "$$( timeout 10s rg -l '^pub\(in crate::client\) struct UsrRollbackCompleteRouteSeal \{' crates/forge/src/client --glob '*.rs' )" = "$$production_dispatch"; \
	timeout 10s grep -Fq '    UsrRollbackCandidatePreserveSeal, UsrRollbackCompleteRouteSeal, UsrRollbackFreshDbInvalidationRouteSeal,' "$$startup_gate"; \
	timeout 10s grep -Fqx 'pub(in crate::client) struct UsrRollbackCompleteRouteSeal {' "$$production_dispatch"; \
	timeout 10s awk '$$0 == "pub(in crate::client) struct UsrRollbackCompleteRouteSeal {" { state = 1; next } state == 1 && $$0 == "    _private: ()," { field = 1; next } state == 1 && $$0 == "}" { found = field; exit !found } END { exit !found }' "$$production_dispatch"; \
	seal_impl="$$( timeout 10s sed -n '/^impl UsrRollbackCompleteRouteSeal {/,/^}/p' "$$production_dispatch" )"; \
	timeout 10s test "$$( timeout 10s grep -Fc '    fn new() -> Self {' <<<"$$seal_impl" )" = 1; \
	timeout 10s test "$$( timeout 10s grep -Fc '    pub(in crate::client) fn new_for_test() -> Self {' <<<"$$seal_impl" )" = 1; \
	timeout 10s test "$$( timeout 10s grep -Fc 'UsrRollbackCompleteRouteSeal::new();' "$$production_dispatch" )" = 1; \
	timeout 10s test "$$( timeout 10s grep -Fc 'UsrRollbackCompleteRouteAuthority::capture(' "$$production_dispatch" )" = 1; \
	timeout 10s rg -n -F 'UsrRollbackCompleteRouteSeal::new();' crates/forge/src/client --glob '*.rs' --glob '!**/tests/**' --glob '!**/tests.rs' --glob '!**/*_tests.rs' --glob '!**/*_tests/**' > "$$symbol_refs"; \
	timeout 10s test "$$( timeout 10s wc -l < "$$symbol_refs" )" = 1; \
	timeout 10s test "$$( timeout 10s cut -d: -f1 "$$symbol_refs" )" = "$$production_dispatch"; \
	timeout 10s rg -n -F 'UsrRollbackCompleteRouteAuthority::capture(' crates/forge/src/client --glob '*.rs' --glob '!**/tests/**' --glob '!**/tests.rs' --glob '!**/*_tests.rs' --glob '!**/*_tests/**' > "$$symbol_refs"; \
	timeout 10s test "$$( timeout 10s wc -l < "$$symbol_refs" )" = 1; \
	timeout 10s test "$$( timeout 10s cut -d: -f1 "$$symbol_refs" )" = "$$production_dispatch"; \
	if timeout 10s rg -U -n '#\[derive\([^]]*Clone[^]]*\)\]\n(?:#\[[^\n]*\]\n)*(?:pub\([^)]*\)[[:space:]]+)?(?:struct|enum)[[:space:]]+(UsrRollbackCompleteRoute(?:Authority|DatabaseEvidence)|ExactFreshTransitionAbsence)' "$$authority" "$$exact"; then exit 1; else status="$$?"; timeout 10s test "$$status" = 1; fi; \
	if timeout 10s rg -n 'impl Clone for (UsrRollbackCompleteRoute(?:Authority|DatabaseEvidence)|ExactFreshTransitionAbsence)' "$$authority" "$$exact"; then exit 1; else status="$$?"; timeout 10s test "$$status" = 1; fi; \
	timeout 10s grep -Fqx '    context: DatabaseEvidence,' "$$authority"; \
	timeout 10s grep -Fqx '    absence: db::state::ExactFreshTransitionAbsence,' "$$authority"; \
	timeout 10s test "$$( timeout 10s grep -Fc 'inspect_exact_fresh_transition' "$$authority" )" = 2; \
	timeout 10s sed -n '/^fn inspect_current_database(/,/^}/p' "$$authority" > "$$inspection_body"; \
	timeout 10s grep -q . "$$inspection_body"; \
	timeout 10s test "$$( timeout 10s grep -Fc 'inspect_exact_fresh_transition' "$$inspection_body" )" = 2; \
	exact_before_line="$$( timeout 10s grep -nF 'let exact_before = state_db.inspect_exact_fresh_transition' "$$inspection_body" | timeout 10s cut -d: -f1 )"; \
	context_line="$$( timeout 10s grep -nF 'let context = inspect_database(record, state_db, in_flight)?;' "$$inspection_body" | timeout 10s cut -d: -f1 )"; \
	exact_after_line="$$( timeout 10s grep -nF 'let exact_after = state_db.inspect_exact_fresh_transition' "$$inspection_body" | timeout 10s cut -d: -f1 )"; \
	timeout 10s test "$$exact_before_line" -lt "$$context_line"; \
	timeout 10s test "$$context_line" -lt "$$exact_after_line"; \
	timeout 10s grep -Fq 'ExactFreshTransitionObservation::JointlyAbsent(absence)' "$$inspection_body"; \
	if timeout 10s rg -n 'ExactFreshTransitionObservation::Present' "$$inspection_body"; then exit 1; else status="$$?"; timeout 10s test "$$status" = 1; fi; \
	timeout 10s grep -Fq 'ownership: db::state::TransitionOwnership::Missing,' "$$authority"; \
	timeout 10s grep -Fq 'provenance: None,' "$$authority"; \
	timeout 10s grep -Fq 'evidence.absence.state_id() == candidate' "$$authority"; \
	timeout 10s grep -Fq 'evidence.absence.transition_id() == &record.transition_id' "$$authority"; \
	timeout 10s sed -n '/^    pub(in crate::client) fn capture(/,/^    }/p' "$$authority" > "$$capture_body"; \
	timeout 10s grep -q . "$$capture_body"; \
	timeout 10s test "$$( timeout 10s grep -Fc 'journal.has_binding(&journal_binding)' "$$capture_body" )" = 1; \
	timeout 10s test "$$( timeout 10s grep -Fc 'inspect_current_database(record, state_db)?' "$$capture_body" )" = 2; \
	timeout 10s test "$$( timeout 10s grep -Fc 'UsrRollbackCompleteRouteNamespaceInspection::begin' "$$capture_body" )" = 1; \
	timeout 10s test "$$( timeout 10s grep -Fc 'namespace_inspection.finish(installation, journal, record)' "$$capture_body" )" = 1; \
	capture_binding_line="$$( timeout 10s grep -nF 'journal.has_binding(&journal_binding)' "$$capture_body" | timeout 10s cut -d: -f1 )"; \
	capture_installation_line="$$( timeout 10s grep -nF 'installation.revalidate_mutable_namespace()?;' "$$capture_body" | timeout 10s head -n 1 | timeout 10s cut -d: -f1 )"; \
	capture_database_before_line="$$( timeout 10s grep -nF 'let database_before =' "$$capture_body" | timeout 10s cut -d: -f1 )"; \
	capture_namespace_begin_line="$$( timeout 10s grep -nF 'UsrRollbackCompleteRouteNamespaceInspection::begin' "$$capture_body" | timeout 10s cut -d: -f1 )"; \
	capture_namespace_finish_line="$$( timeout 10s grep -nF 'namespace_inspection.finish(installation, journal, record)' "$$capture_body" | timeout 10s cut -d: -f1 )"; \
	capture_database_after_line="$$( timeout 10s grep -nF 'let database_after =' "$$capture_body" | timeout 10s cut -d: -f1 )"; \
	timeout 10s test "$$capture_binding_line" -lt "$$capture_installation_line"; \
	timeout 10s test "$$capture_installation_line" -lt "$$capture_database_before_line"; \
	timeout 10s test "$$capture_database_before_line" -lt "$$capture_namespace_begin_line"; \
	timeout 10s test "$$capture_namespace_begin_line" -lt "$$capture_namespace_finish_line"; \
	timeout 10s test "$$capture_namespace_finish_line" -lt "$$capture_database_after_line"; \
	timeout 10s sed -n '/^    pub(in crate::client) fn revalidate(/,/^    }/p' "$$authority" > "$$revalidate_body"; \
	timeout 10s grep -q . "$$revalidate_body"; \
	timeout 10s test "$$( timeout 10s grep -Fc 'journal.has_binding(&self.journal_binding)' "$$revalidate_body" )" = 1; \
	timeout 10s test "$$( timeout 10s grep -Fc 'inspect_current_database(&self.record, &self.state_db)?' "$$revalidate_body" )" = 2; \
	timeout 10s test "$$( timeout 10s grep -Fc 'require_exact_database(' "$$revalidate_body" )" = 2; \
	timeout 10s test "$$( timeout 10s grep -Fc '.revalidate(&self.installation, journal, &self.record)?;' "$$revalidate_body" )" = 1; \
	binding_line="$$( timeout 10s grep -nF 'journal.has_binding(&self.journal_binding)' "$$revalidate_body" | timeout 10s cut -d: -f1 )"; \
	database_before_line="$$( timeout 10s grep -nF 'let database_before =' "$$revalidate_body" | timeout 10s cut -d: -f1 )"; \
	namespace_line="$$( timeout 10s grep -nF '.revalidate(&self.installation, journal, &self.record)?;' "$$revalidate_body" | timeout 10s cut -d: -f1 )"; \
	database_after_line="$$( timeout 10s grep -nF 'let database_after =' "$$revalidate_body" | timeout 10s cut -d: -f1 )"; \
	timeout 10s test "$$binding_line" -lt "$$database_before_line"; \
	timeout 10s test "$$database_before_line" -lt "$$namespace_line"; \
	timeout 10s test "$$namespace_line" -lt "$$database_after_line"; \
	timeout 10s test "$$( timeout 10s grep -Fc 'rollback_complete_route_plan_is_exact' "$$revalidate_body" )" = 1; \
	for field in \
		'record.operation == Operation::NewState' \
		'record.phase == Phase::FreshDbInvalidated' \
		'record.candidate.id.is_some()' \
		'rollback.previous_archive == RollbackAction::NotRequired' \
		'rollback.candidate.disposition == AbortDisposition::Quarantine' \
		'rollback.boot == BootRollback::NotRequired' \
		'rollback.external_effects_may_remain'; do \
		timeout 10s grep -Fq "$$field" "$$authority"; \
	done; \
	for marker in 'ForwardPhase::UsrExchangeIntent | ForwardPhase::UsrExchanged' 'rollback.usr_exchange,' 'rollback.candidate.action,' 'rollback.fresh_db,'; do \
		timeout 10s grep -Fq "$$marker" "$$authority"; \
	done; \
	timeout 10s grep -Fqx 'pub(in crate::client::startup_reconciliation) struct UsrRollbackCompleteRouteNamespaceProof {' "$$proof"; \
	timeout 10s grep -Fq 'require_exact_new_state_fresh_db_invalidated_topology' "$$proof"; \
	timeout 10s grep -Fq 'record.phase != Phase::FreshDbInvalidated' "$$candidate_proof"; \
	timeout 10s grep -Fq 'UsrRollbackCandidatePreserveTopology::NewStatePreserved' "$$candidate_proof"; \
	timeout 10s test "$$( timeout 10s grep -Fc 'authority.revalidate(&journal)' "$$executor" )" = 2; \
	timeout 10s test "$$( timeout 10s grep -Fc 'source_record.rollback_successor(None)' "$$executor" )" = 1; \
	timeout 10s test "$$( timeout 10s grep -Fc 'successor.phase == Phase::RollbackComplete' "$$executor" )" = 1; \
	timeout 10s test "$$( timeout 10s grep -Fc 'journal.advance(&source_record, &successor)' "$$executor" )" = 1; \
	timeout 10s test "$$( timeout 10s grep -Fc 'before_usr_rollback_complete_route_final_revalidation();' "$$executor" )" = 1; \
	timeout 10s test "$$( timeout 10s grep -Fc 'authority.installation()' "$$executor" )" = 1; \
	timeout 10s test "$$( timeout 10s grep -Fc 'drop(authority);' "$$executor" )" = 5; \
	timeout 10s test "$$( timeout 10s grep -Fc 'drop(journal);' "$$executor" )" = 5; \
	timeout 10s test "$$( timeout 10s grep -Fc 'reopen_canonical_journal(&installation)' "$$executor" )" = 1; \
	first_revalidate_line="$$( timeout 10s grep -nF 'authority.revalidate(&journal)' "$$executor" | timeout 10s head -n 1 | timeout 10s cut -d: -f1 )"; \
	successor_line="$$( timeout 10s grep -nF 'source_record.rollback_successor(None)' "$$executor" | timeout 10s cut -d: -f1 )"; \
	seam_line="$$( timeout 10s grep -nF 'before_usr_rollback_complete_route_final_revalidation();' "$$executor" | timeout 10s cut -d: -f1 )"; \
	final_revalidate_line="$$( timeout 10s grep -nF 'authority.revalidate(&journal)' "$$executor" | timeout 10s tail -n 1 | timeout 10s cut -d: -f1 )"; \
	advance_line="$$( timeout 10s grep -nF 'journal.advance(&source_record, &successor)' "$$executor" | timeout 10s cut -d: -f1 )"; \
	drop_authority_line="$$( timeout 10s grep -nF '    drop(authority);' "$$executor" | timeout 10s tail -n 1 | timeout 10s cut -d: -f1 )"; \
	drop_journal_line="$$( timeout 10s grep -nF '    drop(journal);' "$$executor" | timeout 10s tail -n 1 | timeout 10s cut -d: -f1 )"; \
	reopen_line="$$( timeout 10s grep -nF 'reopen_canonical_journal(&installation)' "$$executor" | timeout 10s cut -d: -f1 )"; \
	timeout 10s test "$$first_revalidate_line" -lt "$$successor_line"; \
	timeout 10s test "$$successor_line" -lt "$$seam_line"; \
	timeout 10s test "$$seam_line" -lt "$$final_revalidate_line"; \
	timeout 10s test "$$final_revalidate_line" -lt "$$advance_line"; \
	timeout 10s test "$$advance_line" -lt "$$drop_authority_line"; \
	timeout 10s test "$$drop_authority_line" -lt "$$drop_journal_line"; \
	timeout 10s test "$$drop_journal_line" -lt "$$reopen_line"; \
	timeout 10s grep -Fqx 'pub(in crate::client) enum DurableUsrRollbackCompleteRouteRecord {' "$$executor"; \
	timeout 10s grep -Fqx '    FreshDbInvalidated,' "$$executor"; \
	timeout 10s grep -Fqx '    RollbackComplete,' "$$executor"; \
	timeout 10s grep -Fqx '                    durable: DurableUsrRollbackCompleteRouteRecord::FreshDbInvalidated,' "$$executor"; \
	timeout 10s grep -Fqx '                    durable: DurableUsrRollbackCompleteRouteRecord::RollbackComplete,' "$$executor"; \
	timeout 10s test "$$( timeout 10s grep -Fc 'Ok((reopened, Some(actual))) if actual == source_record => {' "$$executor" )" = 1; \
	timeout 10s test "$$( timeout 10s grep -Fc 'Ok((reopened, Some(actual))) if actual == successor => {' "$$executor" )" = 1; \
	timeout 10s grep -Fqx '            Ok((reopened, Some(actual))) if actual == successor => Ok((reopened, successor)),' "$$executor"; \
	if timeout 10s rg -n 'retained_mutable_cast_directory|open_in_retained_cast|journal\.load\(' "$$executor"; then exit 1; else status="$$?"; timeout 10s test "$$status" = 1; fi; \
	timeout 10s sed -E 's,//.*$$,,' "$$executor" "$$authority" "$$proof" > "$$production_code"; \
	if timeout 10s rg -n '^[[:space:]]*(loop|while|for)[[:space:]]|=[[:space:]]*(loop|while|for)[[:space:]]' "$$production_code"; then exit 1; else status="$$?"; timeout 10s test "$$status" = 1; fi; \
	if timeout 10s rg -n 'RollbackActionOutcome|UsrRollbackFreshDbInvalidation|persist_usr_rollback_fresh_db_invalidation_and_reopen|fresh_db_invalidation_removal_call_count' "$$production_code"; then exit 1; else status="$$?"; timeout 10s test "$$status" = 1; fi; \
	if timeout 10s rg -n 'retry|forward_successor|rollback_decision|RecoveryDisposition|FinalizeRollback|finalize_rollback|journal\.delete|run_transaction_triggers|run_system_triggers|root_links|archive_previous|rearchive_archived|preserve_failed|cleanup|dispatch' "$$production_code"; then exit 1; else status="$$?"; timeout 10s test "$$status" = 1; fi; \
	if timeout 10s rg -n 'diesel::|SqliteConnection|sql_query|\.execute[[:space:]]*\(|\.transaction[[:space:]]*\(|insert_fresh_metadata|delete_metadata|clear_transition_if_matches|remove_transition_if_matches|remove_exact_fresh_transition|\.add[[:space:]]*\(|\.create[[:space:]]*\(|\.remove[[:space:]]*\(|\.batch_remove[[:space:]]*\(|\.delete[[:space:]]*\(' "$$production_code"; then exit 1; else status="$$?"; timeout 10s test "$$status" = 1; fi; \
	if timeout 10s rg -n 'renameat|std::fs|(^|[^_[:alnum:]])fs::|rename[[:space:]]*\(|unlink(at)?[[:space:]]*\(|linkat[[:space:]]*\(|sync_(all|data)|write_all|set_permissions|chmod|create_dir|remove_(dir|file)|hard_link|symlink|attempt_move|reconcile_move' "$$production_code"; then exit 1; else status="$$?"; timeout 10s test "$$status" = 1; fi; \
	timeout 10s grep -Fq 'for historical in [false, true]' "$$admission"; \
	timeout 10s grep -Fq 'FreshDbOutcome::Applied' "$$matrix"; \
	timeout 10s grep -Fq 'FreshDbOutcome::AlreadySatisfied' "$$matrix"; \
	timeout 10s grep -Fq 'TransitionJournalStore::open_retained(' "$$races"; \
	timeout 10s grep -Fq 'arm_between_usr_rollback_complete_route_database_captures' "$$races"; \
	timeout 10s grep -Fq 'arm_before_usr_rollback_complete_route_fresh_namespace_capture' "$$races"; \
	timeout 10s grep -Fq 'arm_before_usr_rollback_complete_route_final_revalidation' "$$races"; \
	for fault in temporary_sync update_exchange update_first_directory_sync displaced_unlink update_final_directory_sync; do \
		timeout 10s grep -Fq "arm_next_$${fault}_fault" "$$storage"; \
	done; \
	timeout 10s test "$$( timeout 10s grep -Fc 'drop(reservation);' "$$restart" )" = 2; \
	timeout 10s awk '/drop\(reservation\);/ { dropped += 1; awaiting = 1; next } awaiting && /ActiveStateReservation::acquire\(\)\.unwrap\(\);/ { reacquired += 1; awaiting = 0 } END { exit !(dropped == 2 && reacquired == 2 && awaiting == 0) }' "$$restart"; \
	timeout 10s grep -Fq 'DurableUsrRollbackCompleteRouteRecord::FreshDbInvalidated' "$$restart"; \
	timeout 10s grep -Fq 'DurableUsrRollbackCompleteRouteRecord::RollbackComplete' "$$restart"; \
	timeout 10s grep -Fq 'UsrRollbackCompleteRouteAdmission::NotApplicable' "$$restart"; \
	for file in "$$executor" "$$authority" "$$proof" "$$candidate_proof" "$$exact" "$$reopen" "$$startup_gate" "$$recovery_root" "$$reconciliation_root" "$$namespace_root" "$$tests" "$$support" "$$admission" "$$races" "$$matrix" "$$storage" "$$restart" misc/make/startup-rollback-complete-route-tests.mk misc/make/exact-fresh-transition-removal-tests.mk misc/make/startup-recovery-tests.mk Makefile misc/make/help.mk; do \
		timeout 10s test "$$( timeout 10s wc -l < "$$file" )" -le 1000; \
	done; \
	timeout 1200s $(CARGO) test -p forge --lib "$$prefix" -- --test-threads=1
