.PHONY: forge-startup-usr-rollback-active-reblit-complete-route-test

forge-startup-usr-rollback-active-reblit-complete-route-test:
	@set -euo pipefail; \
	timeout 10s mkdir -p "$(TOP_DIR)/target"; \
	listed="$$( timeout 10s mktemp "$(TOP_DIR)/target/active-reblit-complete-route-list.XXXXXXXXXXXX" )"; \
	production_code="$$( timeout 10s mktemp "$(TOP_DIR)/target/active-reblit-complete-route-code.XXXXXXXXXXXX" )"; \
	trap 'timeout 10s rm -f "$$listed" "$$production_code"' EXIT; \
	timeout 300s $(CARGO) test -p forge --lib -- --list | timeout 300s tee "$$listed" >/dev/null; \
	timeout 10s grep -q . "$$listed"; \
	prefix='client::startup_gate::usr_rollback_active_reblit::tests::'; \
	timeout 10s test "$$( timeout 10s grep -c "^$$prefix"'complete_.*: test$$' "$$listed" )" = 11; \
	for name in \
		complete_authority_binding::startup_active_reblit_complete_route_authority_rejects_reopened_and_cross_root_journal_bindings \
		complete_evidence_races::startup_active_reblit_complete_route_rejects_database_provenance_journal_and_namespace_races \
		complete_evidence_races::startup_active_reblit_complete_route_root_links_rejects_all_root_abi_mutations_at_two_seams \
		complete_exclusions::startup_active_reblit_complete_route_preserves_operation_and_phase_ordering \
		complete_matrix::startup_active_reblit_complete_route_covers_all_twenty_four_exact_candidate_preserved_cases \
		complete_record_binding::startup_active_reblit_complete_route_bound_advance_same_byte_replacements_never_succeed \
		complete_record_binding::startup_active_reblit_complete_route_same_byte_successor_replacement_fails_reopened_binding \
		complete_record_binding::startup_active_reblit_complete_route_same_byte_successor_replacement_fails_same_store_binding \
		complete_restart::startup_active_reblit_complete_route_source_durable_fresh_handle_reopen_retries_only_the_route \
		complete_restart::startup_active_reblit_complete_route_successor_durable_fresh_handle_reopen_skips_the_route \
		complete_storage_faults::startup_active_reblit_complete_route_all_five_journal_faults_reopen_exact_durable_record; do \
		timeout 10s grep -Fqx "$$prefix$$name: test" "$$listed"; \
	done; \
	gate=crates/forge/src/client/startup_gate.rs; \
	orchestrator=crates/forge/src/client/startup_gate/usr_rollback_active_reblit.rs; \
	reconciliation=crates/forge/src/client/startup_reconciliation.rs; \
	recovery=crates/forge/src/client/startup_recovery.rs; \
	namespace=crates/forge/src/client/startup_reconciliation/activation_namespace.rs; \
	authority=crates/forge/src/client/startup_reconciliation/usr_rollback_active_reblit_complete_route_authority.rs; \
	proof=crates/forge/src/client/startup_reconciliation/activation_namespace/active_reblit_complete_route_proof.rs; \
	executor=crates/forge/src/client/startup_recovery/usr_rollback_active_reblit_complete_route.rs; \
	boot_authority=crates/forge/src/client/startup_reconciliation/usr_rollback_active_reblit_boot_repair_required_authority.rs; \
	finalization_authority=crates/forge/src/client/startup_reconciliation/usr_rollback_active_reblit_finalization_authority.rs; \
	tests=crates/forge/src/client/startup_gate/usr_rollback_active_reblit/tests; \
	candidate_support=crates/forge/src/client/startup_reconciliation/usr_rollback_candidate_preserve_authority/tests/support.rs; \
	root_links_endpoint=crates/forge/src/client/startup_recovery/usr_rollback_resume_route/tests/root_links_route_endpoint.rs; \
	resume_make=misc/make/startup-rollback-resume-route-tests.mk; \
	timeout 10s test "$$( timeout 10s grep -Fc 'mod usr_rollback_active_reblit;' "$$gate" )" = 1; \
	timeout 10s test "$$( timeout 10s grep -Fc 'mod usr_rollback_active_reblit_complete_route_authority;' "$$reconciliation" )" = 1; \
	timeout 10s test "$$( timeout 10s grep -Fc 'mod usr_rollback_active_reblit_complete_route;' "$$recovery" )" = 1; \
	timeout 10s test "$$( timeout 10s grep -Fc 'mod active_reblit_complete_route_proof;' "$$namespace" )" = 1; \
	for module in complete_authority_binding complete_evidence_races complete_exclusions complete_matrix complete_record_binding complete_restart complete_storage_faults; do \
		timeout 10s grep -Fqx "mod $$module;" "$$tests/mod.rs"; \
	done; \
	timeout 10s test "$$( timeout 10s rg -n '^#\[test\]$$' "$$tests"/complete_*.rs | timeout 10s wc -l )" = 11; \
	timeout 10s grep -Fqx 'pub(in crate::client) struct UsrRollbackActiveReblitCompleteRouteSeal {' "$$orchestrator"; \
	timeout 10s grep -Fq 'pub(in crate::client) fn new_for_test() -> Self {' "$$orchestrator"; \
	timeout 10s test "$$( timeout 10s grep -Fc 'UsrRollbackActiveReblitCompleteRouteSeal::new();' "$$orchestrator" )" = 1; \
	timeout 10s test "$$( timeout 10s grep -Fc 'UsrRollbackActiveReblitCompleteRouteAuthority::capture(' "$$orchestrator" )" = 1; \
	timeout 10s test "$$( timeout 10s grep -Fc 'persist_usr_rollback_active_reblit_complete_route_and_reopen(journal, authority)?' "$$orchestrator" )" = 1; \
	capture_call_count="$$( timeout 10s rg -n 'UsrRollbackActiveReblitCompleteRouteAuthority::capture\(' crates/forge/src/client --glob '*.rs' --glob '!**/tests/**' --glob '!**/*_tests.rs' --glob '!**/*_tests/**' | timeout 10s wc -l )"; \
	timeout 10s test "$$capture_call_count" = 1; \
	persist_call_count="$$( timeout 10s rg -n 'persist_usr_rollback_active_reblit_complete_route_and_reopen\(' crates/forge/src/client --glob '*.rs' --glob '!**/tests/**' --glob '!**/*_tests.rs' --glob '!**/*_tests/**' | timeout 10s wc -l )"; \
	timeout 10s test "$$persist_call_count" = 2; \
	timeout 10s grep -Fqx '        Phase::CandidatePreserved => {' "$$orchestrator"; \
	complete_arm="$$( timeout 10s sed -n '/Phase::CandidatePreserved => {/,/Phase::RollbackComplete => {/p' "$$orchestrator" | timeout 10s sed '$$d' )"; \
	timeout 10s grep -Fq 'Ok(Dispatch::Handled { journal, record })' <<<"$$complete_arm"; \
	if timeout 10s rg -n 'Phase::RollbackComplete|finalize_usr_rollback|journal\.delete|run_(transaction|system)_triggers|loop|while|retry' <<<"$$complete_arm"; then exit 1; else status="$$?"; timeout 10s test "$$status" = 1; fi; \
	timeout 10s grep -Fqx "pub(in crate::client) enum UsrRollbackActiveReblitCompleteRouteAdmission<'reservation> {" "$$authority"; \
	for variant in '    NotApplicable,' '    Deferred,' "    Ready(UsrRollbackActiveReblitCompleteRouteAuthority<'reservation>),"; do \
		timeout 10s grep -Fqx "$$variant" "$$authority"; \
	done; \
	if timeout 10s rg -U -n '#\[derive\([^]]*Clone[^]]*\)\]\n(?:#\[[^\n]*\]\n)*(?:pub\([^)]*\)[[:space:]]+)?(?:struct|enum)[[:space:]]+UsrRollbackActiveReblitCompleteRoute(?:Authority|DatabaseEvidence|NamespaceProof)' "$$authority" "$$proof"; then exit 1; else status="$$?"; timeout 10s test "$$status" = 1; fi; \
	for field in \
		'record.operation == Operation::ActiveReblit' \
		'record.phase == Phase::CandidatePreserved' \
		'record.candidate.id == record.previous.id' \
		'ForwardPhase::UsrExchangeIntent | ForwardPhase::UsrExchanged | ForwardPhase::RootLinksComplete' \
		'rollback.previous_archive == RollbackAction::NotRequired' \
		'rollback.candidate.disposition == AbortDisposition::Quarantine' \
		'rollback.fresh_db == RollbackAction::NotRequired' \
		'rollback.boot == BootRollback::NotRequired' \
		'rollback.external_effects_may_remain'; do \
		timeout 10s grep -Fq "$$field" "$$authority"; \
	done; \
	if timeout 10s rg -n 'rollback\.source == ForwardPhase::BootSyncStarted' "$$authority"; then exit 1; else status="$$?"; timeout 10s test "$$status" = 1; fi; \
	timeout 10s grep -Fq 'rollback.source == ForwardPhase::BootSyncStarted' "$$boot_authority"; \
	timeout 10s grep -Fq 'rollback.boot == BootRollback::PendingUnverifiable' "$$boot_authority"; \
	timeout 10s test "$$( timeout 10s grep -Fc 'RollbackAction::Applied | RollbackAction::AlreadySatisfied' "$$authority" )" = 2; \
	timeout 10s grep -Fq 'DatabaseEvidence::ExistingCandidate {' "$$authority"; \
	timeout 10s grep -Fq 'provenance: Some(_),' "$$authority"; \
	timeout 10s grep -Fq 'previous: None,' "$$authority"; \
	timeout 10s grep -Fq 'existing.ownership == db::state::TransitionOwnership::Cleared' "$$authority"; \
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
	capture_namespace="$$( timeout 10s grep -nF 'UsrRollbackActiveReblitCompleteRouteNamespaceInspection::begin' <<<"$$capture" | timeout 10s cut -d: -f1 )"; \
	capture_db_after="$$( timeout 10s grep -nF 'let database_after =' <<<"$$capture" | timeout 10s cut -d: -f1 )"; \
	timeout 10s test "$$capture_binding" -lt "$$capture_db_before"; \
	timeout 10s test "$$capture_db_before" -lt "$$capture_namespace"; \
	timeout 10s test "$$capture_namespace" -lt "$$capture_db_after"; \
	revalidate="$$( timeout 10s sed -n '/pub(in crate::client) fn revalidate(/,/pub(in crate::client) fn installation(/p' "$$authority" )"; \
	revalidate_db_before="$$( timeout 10s grep -nF 'let database_before =' <<<"$$revalidate" | timeout 10s cut -d: -f1 )"; \
	revalidate_namespace="$$( timeout 10s grep -nF 'self.namespace.revalidate' <<<"$$revalidate" | timeout 10s cut -d: -f1 )"; \
	revalidate_db_after="$$( timeout 10s grep -nF 'let database_after =' <<<"$$revalidate" | timeout 10s cut -d: -f1 )"; \
	revalidate_first_binding="$$( timeout 10s grep -nF 'require_journal_record_binding(' <<<"$$revalidate" | timeout 10s head -n 1 | timeout 10s cut -d: -f1 )"; \
	revalidate_last_binding="$$( timeout 10s grep -nF 'require_journal_record_binding(' <<<"$$revalidate" | timeout 10s tail -n 1 | timeout 10s cut -d: -f1 )"; \
	timeout 10s test "$$revalidate_first_binding" -lt "$$revalidate_db_before"; \
	timeout 10s test "$$revalidate_db_before" -lt "$$revalidate_namespace"; \
	timeout 10s test "$$revalidate_namespace" -lt "$$revalidate_db_after"; \
	timeout 10s test "$$revalidate_db_after" -lt "$$revalidate_last_binding"; \
	timeout 10s grep -Fq 'require_exact_active_reblit_candidate_preserved_topology(expected, &before)?' "$$proof"; \
	timeout 10s grep -Fq 'require_exact_wrapper_index(expected, &fresh, self.wrapper_index)?;' "$$proof"; \
	timeout 10s grep -Fq 'run_before_fresh_namespace_capture();' "$$proof"; \
	timeout 10s grep -Fq 'journal.has_record_binding(cast, journal_record_binding, expected)?' "$$proof"; \
	if timeout 10s rg -n 'journal\.load\(\)' "$$proof"; then exit 1; else status="$$?"; timeout 10s test "$$status" = 1; fi; \
	timeout 10s test "$$( timeout 10s grep -Fc 'authority.revalidate(&journal)' "$$executor" )" = 1; \
	timeout 10s test "$$( timeout 10s grep -Fc 'source_record.rollback_successor(None)' "$$executor" )" = 1; \
	if timeout 10s rg -n 'journal\.advance\(' "$$executor"; then exit 1; else status="$$?"; timeout 10s test "$$status" = 1; fi; \
	timeout 10s test "$$( timeout 10s grep -Fc 'authority.advance_record_binding(&journal, &successor)' "$$executor" )" = 1; \
	timeout 10s grep -Fqx '    Published(TransitionJournalRecordBinding),' "$$executor"; \
	timeout 10s test "$$( timeout 10s grep -Fc '.has_record_binding(cast, successor_binding, successor)' "$$executor" )" = 1; \
	timeout 10s test "$$( timeout 10s grep -Fc '.has_reopened_record_binding(cast, successor_binding, successor)' "$$executor" )" = 1; \
	advance_line="$$( timeout 10s grep -nF 'let advance = match authority.advance_record_binding(&journal, &successor) {' "$$executor" | timeout 10s cut -d: -f1 )"; \
	drop_journal="$$( timeout 10s grep -nF '    drop(journal);' "$$executor" | timeout 10s tail -n 1 | timeout 10s cut -d: -f1 )"; \
	reopen_line="$$( timeout 10s grep -nF 'reopen_canonical_journal(&installation)' "$$executor" | timeout 10s cut -d: -f1 )"; \
	timeout 10s test "$$advance_line" -lt "$$drop_journal"; \
	timeout 10s test "$$drop_journal" -lt "$$reopen_line"; \
	timeout 10s sed -E 's,//.*$$,,' "$$authority" "$$proof" "$$executor" > "$$production_code"; \
	if timeout 10s rg -n '^[[:space:]]*(loop|while)[[:space:]]|^[[:space:]]*for[[:space:]].*[[:space:]]in[[:space:]]|=[[:space:]]*(loop|while)[[:space:]]|=[[:space:]]*for[[:space:]].*[[:space:]]in[[:space:]]|diesel::|SqliteConnection|run_(transaction|system)_triggers|journal\.delete|remove_exact_fresh_transition|renameat|unlink|mkdir|create_dir|remove_(dir|file)|attempt_move|reconcile_move|finalize_usr_rollback|dispatch|retry|boot::|synchronize_boot' "$$production_code"; then exit 1; else status="$$?"; timeout 10s test "$$status" = 1; fi; \
	timeout 10s grep -Fqx '    pub(super) const ALL: [Self; 2] = [Self::Intent, Self::Exchanged];' "$$candidate_support"; \
	timeout 10s grep -Fqx '    pub(super) const THROUGH_CANDIDATE_PRESERVED: [Self; 3] = [' "$$candidate_support"; \
	for file in complete_matrix.rs complete_storage_faults.rs complete_restart.rs complete_record_binding.rs; do \
		timeout 10s grep -Fq 'CandidateSource::THROUGH_CANDIDATE_PRESERVED' "$$tests/$$file"; \
	done; \
	timeout 10s grep -Fq 'assert_eq!(cases, 24);' "$$tests/complete_matrix.rs"; \
	timeout 10s grep -Fq 'assert_eq!(cases, 120);' "$$tests/complete_storage_faults.rs"; \
	for fault in temporary_sync update_exchange update_first_directory_sync displaced_unlink update_final_directory_sync; do \
		timeout 10s grep -Fq "$$fault" "$$tests/complete_storage_faults.rs"; \
	done; \
	for count in 48 24; do timeout 10s grep -Fq "assert_eq!(cases, $$count);" "$$tests/complete_record_binding.rs"; done; \
	timeout 10s test "$$( timeout 10s grep -Fc 'assert_eq!(cases, 24);' "$$tests/complete_record_binding.rs" )" = 2; \
	for hook in BeforeBoundAdvancePublish BeforeBoundAdvanceFinalBinding arm_before_usr_rollback_active_reblit_complete_route_successor_binding_revalidation arm_after_usr_rollback_active_reblit_complete_route_successor_binding_check_before_reopen; do \
		timeout 10s grep -Fq "$$hook" "$$tests/complete_record_binding.rs"; \
	done; \
	timeout 10s test "$$( timeout 10s grep -Fc 'for candidate_source in CandidateSource::THROUGH_CANDIDATE_PRESERVED {' "$$tests/complete_restart.rs" )" = 2; \
	timeout 10s test "$$( timeout 10s grep -Fc 'FreshCompleteRouteHandles::open(retained.path())' "$$tests/complete_restart.rs" )" = 2; \
	timeout 10s test "$$( timeout 10s grep -Fc 'assert_eq!(cases, 24);' "$$tests/complete_restart.rs" )" = 2; \
	if timeout 10s rg -n 'Command::new|SIGKILL|process::' "$$tests/complete_restart.rs"; then exit 1; else status="$$?"; timeout 10s test "$$status" = 1; fi; \
	timeout 10s grep -Fqx '    const ALL: [Self; 2] = [Self::CaptureSandwich, Self::FinalRevalidation];' "$$tests/complete_evidence_races.rs"; \
	timeout 10s grep -Fq 'CandidateSource::RootLinksComplete,' "$$tests/complete_evidence_races.rs"; \
	timeout 10s grep -Fq 'for (name, target) in ROOT_ABI {' "$$tests/complete_evidence_races.rs"; \
	timeout 10s grep -Fq 'for mutation in RootAbiMutation::ALL {' "$$tests/complete_evidence_races.rs"; \
	timeout 10s grep -Fq 'assert!(!displaced_directory.path().starts_with(&root));' "$$tests/complete_evidence_races.rs"; \
	timeout 10s grep -Fq 'assert_exact_root_abi_mutation(' "$$tests/complete_evidence_races.rs"; \
	timeout 10s grep -Fq 'assert_eq!(cases, 240);' "$$tests/complete_evidence_races.rs"; \
	for hook in arm_between_usr_rollback_active_reblit_complete_route_database_captures arm_before_usr_rollback_active_reblit_complete_route_final_revalidation arm_before_usr_rollback_active_reblit_complete_route_fresh_namespace_capture; do \
		timeout 10s grep -Fq "$$hook" "$$tests/complete_evidence_races.rs"; \
	done; \
	timeout 10s grep -Fq 'assert_exact_no_boot_completion_plan' "$$tests/complete_matrix.rs"; \
	timeout 10s grep -Fq 'assert_complete_route_journal_only();' "$$tests/complete_matrix.rs"; \
	timeout 10s grep -Fq 'assert_no_boot_synchronize_attempts();' "$$tests/support.rs"; \
	timeout 10s grep -Fq 'retained_exchange_syscall_count(),' "$$tests/support.rs"; \
	timeout 10s grep -Fq 'ForwardPhase::UsrExchangeIntent | ForwardPhase::UsrExchanged' "$$finalization_authority"; \
	if timeout 10s rg -n 'RootLinksComplete' "$$finalization_authority"; then exit 1; else status="$$?"; timeout 10s test "$$status" = 1; fi; \
	timeout 10s grep -Fq 'for source in CandidateSource::ALL {' "$$tests/finalization_matrix.rs"; \
	timeout 10s grep -Fq 'for source in CandidateSource::ALL {' "$$tests/finalization_process_kill.rs"; \
	if timeout 10s rg -n 'THROUGH_CANDIDATE_PRESERVED' "$$tests/finalization_matrix.rs" "$$tests/finalization_process_kill.rs"; then exit 1; else status="$$?"; timeout 10s test "$$status" = 1; fi; \
	timeout 10s grep -Fq '                    assert_eq!(candidate_preserved.generation, 13, "{case}");' "$$root_links_endpoint"; \
	timeout 10s grep -Fq '                    assert_eq!(rollback_complete.generation, 14, "{case}");' "$$root_links_endpoint"; \
	timeout 10s grep -Fq '                    assert_eq!(pending(&stable_entry).phase(), Phase::RollbackComplete, "{case}");' "$$root_links_endpoint"; \
	timeout 10s grep -Fq '                    assert_eq!(fixture.canonical_bytes(), complete_bytes, "{case}");' "$$root_links_endpoint"; \
	timeout 10s test "$$( timeout 10s grep -Fc 'assert_eq!(active_reblit_candidate_preserve_exchange_attempt_count(), 1, "{case}");' "$$root_links_endpoint" )" = 2; \
	timeout 10s grep -Fq 'assert_eq!(retained_exchange_syscall_count(), 1, "{case}");' "$$root_links_endpoint"; \
	timeout 10s grep -Fq 'assert_eq!(root_link_snapshot(&fixture), root_links_before, "{case}");' "$$root_links_endpoint"; \
	timeout 10s grep -Fq 'assert_eq!(rollback_complete.generation, 14, "{case}");' "$$resume_make"; \
	for file in "$$gate" "$$orchestrator" "$$reconciliation" "$$recovery" "$$namespace" "$$authority" "$$proof" "$$executor" "$$boot_authority" "$$finalization_authority" "$$tests"/complete_*.rs "$$tests/support.rs" "$$root_links_endpoint" misc/make/startup-rollback-active-reblit-complete-route-tests.mk "$$resume_make"; do \
		timeout 10s test "$$( timeout 10s wc -l < "$$file" )" -le 1000; \
	done; \
	timeout 1800s $(CARGO) test -p forge --lib startup_active_reblit_complete_route -- --test-threads=1
