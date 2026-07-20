.PHONY: forge-startup-usr-rollback-activate-archived-complete-route-test

forge-startup-usr-rollback-activate-archived-complete-route-test:
	@set -euo pipefail; \
	timeout 10s mkdir -p "$(TOP_DIR)/target"; \
	listed="$$( timeout 10s mktemp "$(TOP_DIR)/target/activate-archived-complete-route-list.XXXXXXXXXXXX" )"; \
	production_code="$$( timeout 10s mktemp "$(TOP_DIR)/target/activate-archived-complete-route-code.XXXXXXXXXXXX" )"; \
	trap 'timeout 10s rm -f "$$listed" "$$production_code"' EXIT; \
	timeout 300s $(CARGO) test -p forge --lib -- --list | timeout 300s tee "$$listed" >/dev/null; \
	timeout 10s grep -q . "$$listed"; \
	prefix='client::startup_gate::usr_rollback_activate_archived::tests::'; \
	timeout 10s test "$$( timeout 10s grep -c "^$$prefix"'.*: test$$' "$$listed" )" = 43; \
	for name in \
		authority_binding::startup_activate_archived_complete_route_rejects_reopened_and_cross_root_journal_bindings \
		evidence_races::startup_activate_archived_complete_route_capture_sandwich_rejects_database_provenance_and_namespace_races \
		evidence_races::startup_activate_archived_complete_route_final_revalidation_rejects_database_provenance_journal_and_namespace_races \
		evidence_races::startup_activate_archived_complete_route_root_links_rejects_all_root_abi_mutations_at_three_seams \
		exclusions::startup_activate_archived_complete_route_defers_every_inexact_plan_boundary \
		exclusions::startup_activate_archived_complete_route_refuses_missing_and_extra_archived_topology \
		exclusions::startup_activate_archived_complete_route_rejects_other_operations_and_phases \
		exclusions::startup_activate_archived_complete_route_advances_once_without_same_entry_finalization \
		exclusions::startup_activate_archived_complete_route_production_defers_inexact_plan_and_topology_without_effects \
		exclusions::startup_activate_archived_complete_route_requires_exact_candidate_previous_and_provenance_rows \
		matrix::startup_activate_archived_complete_route_covers_all_twenty_four_exact_candidate_preserved_cases \
		record_binding::startup_activate_archived_complete_route_bound_advance_same_byte_replacements_never_succeed \
		record_binding::startup_activate_archived_complete_route_same_byte_successor_replacement_fails_reopened_binding \
		record_binding::startup_activate_archived_complete_route_same_byte_successor_replacement_fails_same_store_binding \
		restart::startup_activate_archived_complete_route_source_durable_fresh_handle_reopen_retries_only_the_route \
		restart::startup_activate_archived_complete_route_successor_durable_fresh_handle_reopen_skips_the_route \
		storage_faults::startup_activate_archived_complete_route_all_five_journal_faults_reopen_exact_durable_record; do \
		timeout 10s grep -Fqx "$$prefix$$name: test" "$$listed"; \
	done; \
	seal=crates/forge/src/client/startup_gate/usr_rollback_activate_archived.rs; \
	gate=crates/forge/src/client/startup_gate.rs; \
	reconciliation=crates/forge/src/client/startup_reconciliation.rs; \
	recovery=crates/forge/src/client/startup_recovery.rs; \
	namespace=crates/forge/src/client/startup_reconciliation/activation_namespace.rs; \
	authority=crates/forge/src/client/startup_reconciliation/usr_rollback_activate_archived_complete_route_authority.rs; \
	proof=crates/forge/src/client/startup_reconciliation/activation_namespace/activate_archived_complete_route_proof.rs; \
	candidate_proof=crates/forge/src/client/startup_reconciliation/activation_namespace/candidate_preserve_proof.rs; \
	executor=crates/forge/src/client/startup_recovery/usr_rollback_activate_archived_complete_route.rs; \
	tests=crates/forge/src/client/startup_gate/usr_rollback_activate_archived/tests; \
	candidate_support=crates/forge/src/client/startup_reconciliation/usr_rollback_candidate_preserve_authority/tests/support.rs; \
	timeout 10s test "$$( timeout 10s grep -Fc 'mod usr_rollback_activate_archived;' "$$gate" )" = 1; \
	timeout 10s test "$$( timeout 10s grep -Fc 'mod usr_rollback_activate_archived_complete_route_authority;' "$$reconciliation" )" = 1; \
	timeout 10s test "$$( timeout 10s grep -Fc 'mod usr_rollback_activate_archived_complete_route;' "$$recovery" )" = 1; \
	timeout 10s test "$$( timeout 10s grep -Fc 'mod activate_archived_complete_route_proof;' "$$namespace" )" = 1; \
	for module in authority_binding evidence_races exclusions matrix record_binding restart storage_faults support; do \
		timeout 10s grep -Fqx "mod $$module;" "$$tests/mod.rs"; \
	done; \
	timeout 10s grep -Fqx 'pub(in crate::client) struct UsrRollbackActivateArchivedCompleteRouteSeal {' "$$seal"; \
	timeout 10s grep -Fqx '    fn new() -> Self {' "$$seal"; \
	timeout 10s grep -Fq '#[cfg(test)]' "$$seal"; \
	timeout 10s grep -Fq 'pub(in crate::client) fn new_for_test() -> Self {' "$$seal"; \
	timeout 10s grep -Fqx '        Self::new()' "$$seal"; \
	timeout 10s test "$$( timeout 10s grep -Fc 'UsrRollbackActivateArchivedCompleteRouteSeal::new();' "$$seal" )" = 1; \
	timeout 10s test "$$( timeout 10s grep -Fc 'UsrRollbackActivateArchivedCompleteRouteAuthority::capture(' "$$seal" )" = 1; \
	timeout 10s test "$$( timeout 10s grep -Fc 'persist_usr_rollback_activate_archived_complete_route_and_reopen(journal, authority)?' "$$seal" )" = 1; \
	capture_call_count="$$( timeout 10s rg -n 'UsrRollbackActivateArchivedCompleteRouteAuthority::capture\(' crates/forge/src/client --glob '*.rs' --glob '!**/tests/**' --glob '!**/*_tests.rs' --glob '!**/*_tests/**' | timeout 10s wc -l )"; \
	timeout 10s test "$$capture_call_count" = 1; \
	persist_call_count="$$( timeout 10s rg -n 'persist_usr_rollback_activate_archived_complete_route_and_reopen\(' crates/forge/src/client --glob '*.rs' --glob '!**/tests/**' --glob '!**/*_tests.rs' --glob '!**/*_tests/**' | timeout 10s wc -l )"; \
	timeout 10s test "$$persist_call_count" = 2; \
	timeout 10s grep -Fqx '    if record.operation != Operation::ActivateArchived {' "$$seal"; \
	timeout 10s grep -Fqx '        Phase::CandidatePreserveIntent => {' "$$seal"; \
	timeout 10s grep -Fqx '        Phase::CandidatePreserved => {' "$$seal"; \
	complete_arm="$$( timeout 10s sed -n '/Phase::CandidatePreserved => {/,/        Phase::RollbackComplete =>/p' "$$seal" | timeout 10s sed '$$d' )"; \
	if timeout 10s rg -n 'Phase::RollbackComplete|finalize_usr_rollback|journal\.delete|run_(transaction|system)_triggers|loop|while|retry' <<<"$$complete_arm"; then exit 1; else status="$$?"; timeout 10s test "$$status" = 1; fi; \
	timeout 10s grep -Fq 'Ok(Dispatch::Handled { journal, record })' <<<"$$complete_arm"; \
	handled="$$( timeout 10s sed -n '/usr_rollback_activate_archived::Dispatch::Handled/,/^                }/p' "$$gate" )"; \
	timeout 10s grep -Fq 'PendingSystemTransition::inspect' <<<"$$handled"; \
	timeout 10s grep -Fq 'return Err(Error::RecoveryPending(pending));' <<<"$$handled"; \
	timeout 10s grep -Fqx "pub(in crate::client) enum UsrRollbackActivateArchivedCompleteRouteAdmission<'reservation> {" "$$authority"; \
	for variant in '    NotApplicable,' '    Deferred,' "    Ready(UsrRollbackActivateArchivedCompleteRouteAuthority<'reservation>),"; do \
		timeout 10s grep -Fqx "$$variant" "$$authority"; \
	done; \
	if timeout 10s rg -U -n '#\[derive\([^]]*Clone[^]]*\)\]\n(?:#\[[^\n]*\]\n)*(?:pub\([^)]*\)[[:space:]]+)?(?:struct|enum)[[:space:]]+UsrRollbackActivateArchivedCompleteRoute(?:Authority|DatabaseEvidence|NamespaceProof)' "$$authority" "$$proof"; then exit 1; else status="$$?"; timeout 10s test "$$status" = 1; fi; \
	for field in \
		'record.operation == Operation::ActivateArchived' \
		'record.phase == Phase::CandidatePreserved' \
		'record.candidate.origin == CandidateOrigin::Archived' \
		'record.previous.origin == PreviousOrigin::ActiveState' \
		'record.candidate.id != record.previous.id' \
		'ForwardPhase::UsrExchangeIntent | ForwardPhase::UsrExchanged | ForwardPhase::RootLinksComplete' \
		'rollback.previous_archive == RollbackAction::NotRequired' \
		'rollback.candidate.disposition == AbortDisposition::Rearchive' \
		'rollback.fresh_db == RollbackAction::NotRequired' \
		'rollback.boot == BootRollback::NotRequired' \
		'!rollback.external_effects_may_remain'; do \
		timeout 10s grep -Fq "$$field" "$$authority"; \
	done; \
	timeout 10s test "$$( timeout 10s grep -Fc 'RollbackAction::Applied | RollbackAction::AlreadySatisfied' "$$authority" )" = 2; \
	timeout 10s grep -Fq 'DatabaseEvidence::ExistingCandidate {' "$$authority"; \
	timeout 10s grep -Fq 'provenance: Some(_),' "$$authority"; \
	timeout 10s grep -Fq 'previous: Some(previous_existing),' "$$authority"; \
	timeout 10s test "$$( timeout 10s grep -Fc 'ownership == db::state::TransitionOwnership::Cleared' "$$authority" )" = 2; \
	timeout 10s test "$$( timeout 10s grep -Fc 'inspect_current_database(record, state_db)?' "$$authority" )" = 2; \
	timeout 10s test "$$( timeout 10s grep -Fc 'inspect_current_database(&self.record, &self.state_db)?' "$$authority" )" = 2; \
	if timeout 10s rg -n 'TransitionJournalBinding|journal\.binding\(\)|journal\.has_binding\(' "$$authority" "$$proof" "$$executor"; then exit 1; else status="$$?"; timeout 10s test "$$status" = 1; fi; \
	timeout 10s grep -Fqx '    journal_record_binding: TransitionJournalRecordBinding,' "$$authority"; \
	timeout 10s test "$$( timeout 10s grep -Fc 'journal.record_binding(installation.retained_mutable_cast_directory()?, record)?' "$$authority" )" = 1; \
	timeout 10s test "$$( timeout 10s grep -Fc 'require_journal_record_binding(' "$$authority" )" = 4; \
	binding_helper="$$( timeout 10s sed -n '/^fn require_journal_record_binding(/,/^}/p' "$$authority" )"; \
	timeout 10s grep -Fq '    if !journal.has_record_store_binding(binding) {' <<<"$$binding_helper"; \
	timeout 10s grep -Fq 'journal.has_record_binding(cast, binding, record)?' <<<"$$binding_helper"; \
	capture="$$( timeout 10s sed -n '/pub(in crate::client) fn capture(/,/pub(in crate::client) fn revalidate(/p' "$$authority" )"; \
	capture_binding="$$( timeout 10s grep -nF 'journal.record_binding(installation.retained_mutable_cast_directory()?, record)?' <<<"$$capture" | timeout 10s cut -d: -f1 )"; \
	capture_db_before="$$( timeout 10s grep -nF 'let database_before =' <<<"$$capture" | timeout 10s cut -d: -f1 )"; \
	capture_namespace="$$( timeout 10s grep -nF 'UsrRollbackActivateArchivedCompleteRouteNamespaceInspection::begin' <<<"$$capture" | timeout 10s cut -d: -f1 )"; \
	capture_db_after="$$( timeout 10s grep -nF 'let database_after =' <<<"$$capture" | timeout 10s cut -d: -f1 )"; \
	timeout 10s test "$$capture_binding" -lt "$$capture_db_before"; \
	timeout 10s test "$$capture_db_before" -lt "$$capture_namespace"; \
	timeout 10s test "$$capture_namespace" -lt "$$capture_db_after"; \
	revalidate="$$( timeout 10s sed -n '/pub(in crate::client) fn revalidate(/,/pub(in crate::client) fn installation(/p' "$$authority" )"; \
	revalidate_db_before="$$( timeout 10s grep -nF 'let database_before =' <<<"$$revalidate" | timeout 10s cut -d: -f1 )"; \
	revalidate_namespace="$$( timeout 10s grep -nF 'self.namespace.revalidate' <<<"$$revalidate" | timeout 10s cut -d: -f1 )"; \
	revalidate_db_after="$$( timeout 10s grep -nF 'let database_after =' <<<"$$revalidate" | timeout 10s cut -d: -f1 )"; \
	timeout 10s test "$$revalidate_db_before" -lt "$$revalidate_namespace"; \
	timeout 10s test "$$revalidate_namespace" -lt "$$revalidate_db_after"; \
	revalidate_first_binding="$$( timeout 10s grep -nF 'require_journal_record_binding(' <<<"$$revalidate" | timeout 10s head -n 1 | timeout 10s cut -d: -f1 )"; \
	revalidate_last_binding="$$( timeout 10s grep -nF 'require_journal_record_binding(' <<<"$$revalidate" | timeout 10s tail -n 1 | timeout 10s cut -d: -f1 )"; \
	timeout 10s test "$$revalidate_first_binding" -lt "$$revalidate_db_before"; \
	timeout 10s test "$$revalidate_db_after" -lt "$$revalidate_last_binding"; \
	timeout 10s test "$$( timeout 10s grep -Fc 'require_exact_activate_archived_candidate_preserved_topology' "$$proof" )" = 7; \
	timeout 10s grep -Fq 'run_before_fresh_namespace_capture();' "$$proof"; \
	if timeout 10s rg -n 'wrapper_index|ActiveReblitCompleteRoute|new_state' "$$proof"; then exit 1; else status="$$?"; timeout 10s test "$$status" = 1; fi; \
	if timeout 10s rg -n 'journal\.load\(\)' "$$proof"; then exit 1; else status="$$?"; timeout 10s test "$$status" = 1; fi; \
	timeout 10s test "$$( timeout 10s grep -Fc 'authority.revalidate(&journal)' "$$executor" )" = 1; \
	timeout 10s test "$$( timeout 10s grep -Fc 'source_record.rollback_successor(None)' "$$executor" )" = 1; \
	if timeout 10s rg -n 'journal\.advance\(' "$$executor"; then exit 1; else status="$$?"; timeout 10s test "$$status" = 1; fi; \
	timeout 10s test "$$( timeout 10s grep -Fc 'authority.advance_record_binding(&journal, &successor)' "$$executor" )" = 1; \
	timeout 10s test "$$( timeout 10s grep -Fc 'reopen_canonical_journal(&installation)' "$$executor" )" = 1; \
	advance_line="$$( timeout 10s grep -nF 'let advance = match authority.advance_record_binding(&journal, &successor) {' "$$executor" | timeout 10s cut -d: -f1 )"; \
	drop_journal="$$( timeout 10s grep -nF '    drop(journal);' "$$executor" | timeout 10s tail -n 1 | timeout 10s cut -d: -f1 )"; \
	reopen_line="$$( timeout 10s grep -nF 'reopen_canonical_journal(&installation)' "$$executor" | timeout 10s cut -d: -f1 )"; \
	timeout 10s test "$$advance_line" -lt "$$drop_journal"; \
	timeout 10s test "$$drop_journal" -lt "$$reopen_line"; \
	timeout 10s grep -Fqx '    Published(TransitionJournalRecordBinding),' "$$executor"; \
	timeout 10s test "$$( timeout 10s grep -Fc '.has_record_binding(cast, successor_binding, successor)' "$$executor" )" = 1; \
	timeout 10s test "$$( timeout 10s grep -Fc '.has_reopened_record_binding(cast, successor_binding, successor)' "$$executor" )" = 1; \
	timeout 10s grep -Fq 'arm_after_usr_rollback_activate_archived_complete_route_successor_binding_check_before_reopen' "$$executor"; \
	timeout 10s grep -Fq 'if actual == source_record =>' "$$executor"; \
	timeout 10s grep -Fq 'if actual == successor =>' "$$executor"; \
	timeout 10s sed -E 's,//.*$$,,' "$$authority" "$$proof" "$$executor" > "$$production_code"; \
	if timeout 10s rg -n '^[[:space:]]*(loop|while)[[:space:]]|^[[:space:]]*for[[:space:]].*[[:space:]]in[[:space:]]|=[[:space:]]*(loop|while)[[:space:]]|=[[:space:]]*for[[:space:]].*[[:space:]]in[[:space:]]|diesel::|SqliteConnection|run_(transaction|system)_triggers|journal\.delete|remove_exact_fresh_transition|renameat|unlink|mkdir|create_dir|remove_(dir|file)|attempt_move|reconcile_move|finalize_usr_rollback|dispatch|retry' "$$production_code"; then exit 1; else status="$$?"; timeout 10s test "$$status" = 1; fi; \
	timeout 10s grep -Fq 'for epoch in Epoch::ALL {' "$$tests/matrix.rs"; \
	timeout 10s grep -Fqx '    pub(super) const ALL: [Self; 2] = [Self::Intent, Self::Exchanged];' "$$candidate_support"; \
	timeout 10s grep -Fqx '    pub(super) const THROUGH_CANDIDATE_PRESERVED: [Self; 3] = [' "$$candidate_support"; \
	timeout 10s grep -Fq 'for source in CandidateSource::THROUGH_CANDIDATE_PRESERVED {' "$$tests/matrix.rs"; \
	timeout 10s grep -Fq 'for candidate_outcome in CandidateOutcome::ALL {' "$$tests/matrix.rs"; \
	timeout 10s grep -Fq 'assert_eq!(cases, 24);' "$$tests/matrix.rs"; \
	timeout 10s grep -Fq 'let error = enter_route(&fixture);' "$$tests/matrix.rs"; \
	timeout 10s grep -Fq 'assert_route_pending_audit(&error, &fixture, &expected);' "$$tests/matrix.rs"; \
	timeout 10s grep -Fq 'assert_eq!(retained_exchange_syscall_count(), 0, "{case:?}");' "$$tests/matrix.rs"; \
	timeout 10s grep -Fq 'assert_eq!(candidate_move_count(), 0, "{case:?}");' "$$tests/matrix.rs"; \
	timeout 10s grep -Fq 'assert_eq!(boot_synchronize_attempt_count(), 0, "{case:?}");' "$$tests/matrix.rs"; \
	for fault in temporary_sync update_exchange update_first_directory_sync displaced_unlink update_final_directory_sync; do \
		timeout 10s grep -Fq "$$fault" "$$tests/storage_faults.rs"; \
	done; \
	timeout 10s grep -Fq 'for source in CandidateSource::THROUGH_CANDIDATE_PRESERVED {' "$$tests/storage_faults.rs"; \
	timeout 10s grep -Fq 'assert_eq!(cases, 120);' "$$tests/storage_faults.rs"; \
	for hook in arm_between_usr_rollback_activate_archived_complete_route_database_captures arm_before_usr_rollback_activate_archived_complete_route_fresh_namespace_capture arm_before_usr_rollback_activate_archived_complete_route_final_revalidation; do \
		timeout 10s grep -Fq "$$hook" "$$tests/evidence_races.rs"; \
	done; \
	timeout 10s grep -Fq 'for seam in RootAbiSeam::ALL {' "$$tests/evidence_races.rs"; \
	timeout 10s grep -Fq 'CandidateSource::RootLinksComplete,' "$$tests/evidence_races.rs"; \
	timeout 10s grep -Fq 'for (name, target) in ROOT_ABI {' "$$tests/evidence_races.rs"; \
	timeout 10s grep -Fq 'for mutation in RootAbiMutation::ALL {' "$$tests/evidence_races.rs"; \
	timeout 10s grep -Fq 'assert_eq!(cases, 360);' "$$tests/evidence_races.rs"; \
	for count in 48 24; do \
		timeout 10s grep -Fq "assert_eq!(cases, $$count);" "$$tests/record_binding.rs"; \
	done; \
	timeout 10s test "$$( timeout 10s grep -Fc 'assert_eq!(cases, 24);' "$$tests/record_binding.rs" )" = 2; \
	for hook in BeforeBoundAdvancePublish BeforeBoundAdvanceFinalBinding arm_before_usr_rollback_activate_archived_complete_route_successor_binding_revalidation arm_after_usr_rollback_activate_archived_complete_route_successor_binding_check_before_reopen; do \
		timeout 10s grep -Fq "$$hook" "$$tests/record_binding.rs"; \
	done; \
	timeout 10s grep -Fq 'DurableUsrRollbackActivateArchivedCompleteRouteRecord::CandidatePreserved' "$$tests/restart.rs"; \
	timeout 10s grep -Fq 'DurableUsrRollbackActivateArchivedCompleteRouteRecord::RollbackComplete' "$$tests/restart.rs"; \
	timeout 10s test "$$( timeout 10s grep -Fc 'for source in CandidateSource::THROUGH_CANDIDATE_PRESERVED {' "$$tests/restart.rs" )" = 2; \
	timeout 10s test "$$( timeout 10s grep -Fc 'FreshRouteHandles::open(retained.path())' "$$tests/restart.rs" )" = 2; \
	timeout 10s test "$$( timeout 10s grep -Fc 'assert_eq!(cases, 24);' "$$tests/restart.rs" )" = 2; \
	finalization_authority=crates/forge/src/client/startup_reconciliation/usr_rollback_activate_archived_finalization_authority.rs; \
	finalization_tests="$$tests/finalization_matrix.rs"; \
	timeout 10s grep -Fq 'ForwardPhase::UsrExchangeIntent | ForwardPhase::UsrExchanged' "$$finalization_authority"; \
	if timeout 10s rg -n 'RootLinksComplete' "$$finalization_authority"; then exit 1; else status="$$?"; timeout 10s test "$$status" = 1; fi; \
	timeout 10s grep -Fq 'for source in CandidateSource::ALL {' "$$finalization_tests"; \
	if timeout 10s rg -n 'THROUGH_CANDIDATE_PRESERVED' "$$finalization_tests" "$$tests/finalization_process_kill.rs"; then exit 1; else status="$$?"; timeout 10s test "$$status" = 1; fi; \
	timeout 10s grep -Fq 'unreachable!("RootLinksComplete is outside the later process-kill source axis")' "$$tests/finalization_process_kill.rs"; \
	timeout 10s grep -Fq 'assert_route_pending_audit(&error, &fixture, &expected);' "$$tests/exclusions.rs"; \
	if timeout 10s rg -n 'finalize_usr_rollback|journal\.delete' "$$tests/exclusions.rs"; then exit 1; else status="$$?"; timeout 10s test "$$status" = 1; fi; \
	for file in "$$seal" "$$gate" "$$reconciliation" "$$recovery" "$$namespace" "$$authority" "$$proof" "$$candidate_proof" "$$executor" "$$tests"/*.rs misc/make/startup-rollback-activate-archived-complete-route-tests.mk Makefile misc/make/help.mk; do \
		timeout 10s test "$$( timeout 10s wc -l < "$$file" )" -le 1000; \
	done; \
	timeout 1200s $(CARGO) test -p forge --lib startup_activate_archived_complete_route -- --test-threads=1
