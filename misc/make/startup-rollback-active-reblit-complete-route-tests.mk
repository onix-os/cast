.PHONY: forge-startup-usr-rollback-active-reblit-complete-route-test

forge-startup-usr-rollback-active-reblit-complete-route-test:
	@set -euo pipefail; \
	mkdir -p "$(TOP_DIR)/target"; \
	listed="$$( mktemp "$(TOP_DIR)/target/active-reblit-complete-route-list.XXXXXXXXXXXX" )"; \
	production_code="$$( mktemp "$(TOP_DIR)/target/active-reblit-complete-route-code.XXXXXXXXXXXX" )"; \
	trap 'rm -f "$$listed" "$$production_code"' EXIT; \
	$(CARGO) test -p forge --lib -- --list | tee "$$listed" >/dev/null; \
	grep -q . "$$listed"; \
	prefix='client::startup_gate::usr_rollback_active_reblit::tests::'; \
	test "$$( grep -Ec "^$$prefix"'complete_(authority_binding|evidence_races|exclusions|matrix|record_binding|restart|storage_faults)::.*: test$$' "$$listed" )" = 11; \
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
		grep -Fqx "$$prefix$$name: test" "$$listed"; \
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
	route_tests="$$tests/complete_authority_binding.rs $$tests/complete_evidence_races.rs $$tests/complete_exclusions.rs $$tests/complete_matrix.rs $$tests/complete_record_binding.rs $$tests/complete_restart.rs $$tests/complete_storage_faults.rs"; \
	candidate_support=crates/forge/src/client/startup_reconciliation/usr_rollback_candidate_preserve_authority/tests/support.rs; \
	root_links_endpoint=crates/forge/src/client/startup_recovery/usr_rollback_resume_route/tests/root_links_route_endpoint.rs; \
	resume_make=misc/make/startup-rollback-resume-route-tests.mk; \
	test "$$( grep -Fc 'mod usr_rollback_active_reblit;' "$$gate" )" = 1; \
	test "$$( grep -Fc 'mod usr_rollback_active_reblit_complete_route_authority;' "$$reconciliation" )" = 1; \
	test "$$( grep -Fc 'mod usr_rollback_active_reblit_complete_route;' "$$recovery" )" = 1; \
	test "$$( grep -Fc 'mod active_reblit_complete_route_proof;' "$$namespace" )" = 1; \
	for module in complete_authority_binding complete_evidence_races complete_exclusions complete_matrix complete_record_binding complete_restart complete_storage_faults; do \
		grep -Fqx "mod $$module;" "$$tests/mod.rs"; \
	done; \
	test "$$( rg -n '^#\[test\]$$' $$route_tests | wc -l )" = 11; \
	grep -Fqx 'pub(in crate::client) struct UsrRollbackActiveReblitCompleteRouteSeal {' "$$orchestrator"; \
	grep -Fq 'pub(in crate::client) fn new_for_test() -> Self {' "$$orchestrator"; \
	test "$$( grep -Fc 'UsrRollbackActiveReblitCompleteRouteSeal::new();' "$$orchestrator" )" = 1; \
	test "$$( grep -Fc 'UsrRollbackActiveReblitCompleteRouteAuthority::capture(' "$$orchestrator" )" = 1; \
	test "$$( grep -Fc 'persist_usr_rollback_active_reblit_complete_route_and_reopen(journal, authority)?' "$$orchestrator" )" = 1; \
	capture_call_count="$$( rg -n 'UsrRollbackActiveReblitCompleteRouteAuthority::capture\(' crates/forge/src/client --glob '*.rs' --glob '!**/tests/**' --glob '!**/*_tests.rs' --glob '!**/*_tests/**' | wc -l )"; \
	test "$$capture_call_count" = 1; \
	persist_call_count="$$( rg -n 'persist_usr_rollback_active_reblit_complete_route_and_reopen\(' crates/forge/src/client --glob '*.rs' --glob '!**/tests/**' --glob '!**/*_tests.rs' --glob '!**/*_tests/**' | wc -l )"; \
	test "$$persist_call_count" = 2; \
	grep -Fqx '        Phase::CandidatePreserved => {' "$$orchestrator"; \
	complete_arm="$$( sed -n '/Phase::CandidatePreserved => {/,/Phase::RollbackComplete => {/p' "$$orchestrator" | sed '$$d' )"; \
	grep -Fq 'Ok(Dispatch::Handled { journal, record })' <<<"$$complete_arm"; \
	if rg -n 'Phase::RollbackComplete|finalize_usr_rollback|journal\.delete|run_(transaction|system)_triggers|loop|while|retry' <<<"$$complete_arm"; then exit 1; else status="$$?"; test "$$status" = 1; fi; \
	grep -Fqx "pub(in crate::client) enum UsrRollbackActiveReblitCompleteRouteAdmission<'reservation> {" "$$authority"; \
	for variant in '    NotApplicable,' '    Deferred,' "    Ready(UsrRollbackActiveReblitCompleteRouteAuthority<'reservation>),"; do \
		grep -Fqx "$$variant" "$$authority"; \
	done; \
	if rg -U -n '#\[derive\([^]]*Clone[^]]*\)\]\n(?:#\[[^\n]*\]\n)*(?:pub\([^)]*\)[[:space:]]+)?(?:struct|enum)[[:space:]]+UsrRollbackActiveReblitCompleteRoute(?:Authority|DatabaseEvidence|NamespaceProof)' "$$authority" "$$proof"; then exit 1; else status="$$?"; test "$$status" = 1; fi; \
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
		grep -Fq "$$field" "$$authority"; \
	done; \
	if rg -n 'rollback\.source == ForwardPhase::BootSyncStarted' "$$authority"; then exit 1; else status="$$?"; test "$$status" = 1; fi; \
	grep -Fq 'rollback.source == ForwardPhase::BootSyncStarted' "$$boot_authority"; \
	grep -Fq 'rollback.boot == BootRollback::PendingUnverifiable' "$$boot_authority"; \
	test "$$( grep -Fc 'RollbackAction::Applied | RollbackAction::AlreadySatisfied' "$$authority" )" = 2; \
	grep -Fq 'DatabaseEvidence::ExistingCandidate {' "$$authority"; \
	grep -Fq 'provenance: Some(_),' "$$authority"; \
	grep -Fq 'previous: None,' "$$authority"; \
	grep -Fq 'existing.ownership == db::state::TransitionOwnership::Cleared' "$$authority"; \
	if rg -n 'TransitionJournalBinding|journal\.binding\(\)|journal\.has_binding\(' "$$authority" "$$proof" "$$executor"; then exit 1; else status="$$?"; test "$$status" = 1; fi; \
	grep -Fqx '    journal_record_binding: TransitionJournalRecordBinding,' "$$authority"; \
	test "$$( grep -Fc 'journal.record_binding(installation.retained_mutable_cast_directory()?, record)?' "$$authority" )" = 1; \
	test "$$( grep -Fc 'require_journal_record_binding(' "$$authority" )" = 4; \
	binding_helper="$$( sed -n '/^fn require_journal_record_binding(/,/^}/p' "$$authority" )"; \
	grep -Fq '    if !journal.has_record_store_binding(binding) {' <<<"$$binding_helper"; \
	grep -Fq 'journal.has_record_binding(cast, binding, record)?' <<<"$$binding_helper"; \
	capture="$$( sed -n '/pub(in crate::client) fn capture(/,/pub(in crate::client) fn revalidate(/p' "$$authority" )"; \
	capture_binding="$$( grep -nF 'journal.record_binding(installation.retained_mutable_cast_directory()?, record)?' <<<"$$capture" | cut -d: -f1 )"; \
	capture_db_before="$$( grep -nF 'let database_before =' <<<"$$capture" | cut -d: -f1 )"; \
	capture_namespace="$$( grep -nF 'UsrRollbackActiveReblitCompleteRouteNamespaceInspection::begin' <<<"$$capture" | cut -d: -f1 )"; \
	capture_db_after="$$( grep -nF 'let database_after =' <<<"$$capture" | cut -d: -f1 )"; \
	test "$$capture_binding" -lt "$$capture_db_before"; \
	test "$$capture_db_before" -lt "$$capture_namespace"; \
	test "$$capture_namespace" -lt "$$capture_db_after"; \
	revalidate="$$( sed -n '/pub(in crate::client) fn revalidate(/,/pub(in crate::client) fn installation(/p' "$$authority" )"; \
	revalidate_db_before="$$( grep -nF 'let database_before =' <<<"$$revalidate" | cut -d: -f1 )"; \
	revalidate_namespace="$$( grep -nF 'self.namespace.revalidate' <<<"$$revalidate" | cut -d: -f1 )"; \
	revalidate_db_after="$$( grep -nF 'let database_after =' <<<"$$revalidate" | cut -d: -f1 )"; \
	revalidate_first_binding="$$( grep -nF 'require_journal_record_binding(' <<<"$$revalidate" | head -n 1 | cut -d: -f1 )"; \
	revalidate_last_binding="$$( grep -nF 'require_journal_record_binding(' <<<"$$revalidate" | tail -n 1 | cut -d: -f1 )"; \
	test "$$revalidate_first_binding" -lt "$$revalidate_db_before"; \
	test "$$revalidate_db_before" -lt "$$revalidate_namespace"; \
	test "$$revalidate_namespace" -lt "$$revalidate_db_after"; \
	test "$$revalidate_db_after" -lt "$$revalidate_last_binding"; \
	grep -Fq 'require_exact_active_reblit_candidate_preserved_topology(expected, &before)?' "$$proof"; \
	grep -Fq 'require_exact_wrapper_index(expected, &fresh, self.wrapper_index)?;' "$$proof"; \
	grep -Fq 'run_before_fresh_namespace_capture();' "$$proof"; \
	grep -Fq 'journal.has_record_binding(cast, journal_record_binding, expected)?' "$$proof"; \
	if rg -n 'journal\.load\(\)' "$$proof"; then exit 1; else status="$$?"; test "$$status" = 1; fi; \
	test "$$( grep -Fc 'authority.revalidate(&journal)' "$$executor" )" = 1; \
	test "$$( grep -Fc 'source_record.rollback_successor(None)' "$$executor" )" = 1; \
	if rg -n 'journal\.advance\(' "$$executor"; then exit 1; else status="$$?"; test "$$status" = 1; fi; \
	test "$$( grep -Fc 'authority.advance_record_binding(&journal, &successor)' "$$executor" )" = 1; \
	grep -Fqx '    Published(TransitionJournalRecordBinding),' "$$executor"; \
	test "$$( grep -Fc '.has_record_binding(cast, successor_binding, successor)' "$$executor" )" = 1; \
	test "$$( grep -Fc '.has_reopened_record_binding(cast, successor_binding, successor)' "$$executor" )" = 1; \
	advance_line="$$( grep -nF 'let advance = match authority.advance_record_binding(&journal, &successor) {' "$$executor" | cut -d: -f1 )"; \
	drop_journal="$$( grep -nF '    drop(journal);' "$$executor" | tail -n 1 | cut -d: -f1 )"; \
	reopen_line="$$( grep -nF 'reopen_canonical_journal(&installation)' "$$executor" | cut -d: -f1 )"; \
	test "$$advance_line" -lt "$$drop_journal"; \
	test "$$drop_journal" -lt "$$reopen_line"; \
	sed -E 's,//.*$$,,' "$$authority" "$$proof" "$$executor" > "$$production_code"; \
	if rg -n '^[[:space:]]*(loop|while)[[:space:]]|^[[:space:]]*for[[:space:]].*[[:space:]]in[[:space:]]|=[[:space:]]*(loop|while)[[:space:]]|=[[:space:]]*for[[:space:]].*[[:space:]]in[[:space:]]|diesel::|SqliteConnection|run_(transaction|system)_triggers|journal\.delete|remove_exact_fresh_transition|std::fs|(^|[^[:alnum:]_])fs::|(^|[^[:alnum:]_])nix::|transition_identity|linux_fs|rename(at)?|unlink(at)?|linkat|hard_link|symlink|sync_(all|data)|write_all|set_permissions|chmod|mkdir|create_dir|remove_(dir|file)|attempt_move|reconcile_move|finalize_usr_rollback|dispatch|retry|boot::|synchronize_boot' "$$production_code"; then exit 1; else status="$$?"; test "$$status" = 1; fi; \
	grep -Fqx '    pub(super) const ALL: [Self; 2] = [Self::Intent, Self::Exchanged];' "$$candidate_support"; \
	rg -U -q '^    pub\(super\) const THROUGH_CANDIDATE_PRESERVED: \[Self; 3\] = \[\n        Self::Intent,\n        Self::Exchanged,\n        Self::RootLinksComplete,\n    \];' "$$candidate_support"; \
	for file in complete_matrix.rs complete_storage_faults.rs complete_restart.rs complete_record_binding.rs; do \
		grep -Fq 'CandidateSource::THROUGH_CANDIDATE_PRESERVED' "$$tests/$$file"; \
	done; \
	grep -Fq 'assert_eq!(cases, 24);' "$$tests/complete_matrix.rs"; \
	grep -Fq 'assert_eq!(cases, 120);' "$$tests/complete_storage_faults.rs"; \
	for fault in temporary_sync update_exchange update_first_directory_sync displaced_unlink update_final_directory_sync; do \
		grep -Fq "$$fault" "$$tests/complete_storage_faults.rs"; \
	done; \
	for count in 48 24; do grep -Fq "assert_eq!(cases, $$count);" "$$tests/complete_record_binding.rs"; done; \
	test "$$( grep -Fc 'assert_eq!(cases, 24);' "$$tests/complete_record_binding.rs" )" = 2; \
	for hook in BeforeBoundAdvancePublish BeforeBoundAdvanceFinalBinding arm_before_usr_rollback_active_reblit_complete_route_successor_binding_revalidation arm_after_usr_rollback_active_reblit_complete_route_successor_binding_check_before_reopen; do \
		grep -Fq "$$hook" "$$tests/complete_record_binding.rs"; \
	done; \
	test "$$( grep -Fc 'for candidate_source in CandidateSource::THROUGH_CANDIDATE_PRESERVED {' "$$tests/complete_restart.rs" )" = 2; \
	test "$$( grep -Fc 'FreshCompleteRouteHandles::open(retained.path())' "$$tests/complete_restart.rs" )" = 2; \
	test "$$( grep -Fc 'assert_eq!(cases, 24);' "$$tests/complete_restart.rs" )" = 2; \
	if rg -n 'Command::new|SIGKILL|process::' "$$tests/complete_restart.rs"; then exit 1; else status="$$?"; test "$$status" = 1; fi; \
	grep -Fqx '    const ALL: [Self; 2] = [Self::CaptureSandwich, Self::FinalRevalidation];' "$$tests/complete_evidence_races.rs"; \
	rg -U -q '^const USR_OUTCOMES: \[RollbackActionOutcome; 2\] = \[\n    RollbackActionOutcome::Applied,\n    RollbackActionOutcome::AlreadySatisfied,\n\];' "$$tests/complete_evidence_races.rs"; \
	grep -Fq 'CandidateSource::RootLinksComplete,' "$$tests/complete_evidence_races.rs"; \
	for loop in 'for seam in RootAbiSeam::ALL {' 'for epoch in Epoch::ALL {' 'for usr_outcome in USR_OUTCOMES {' 'for candidate_outcome in CandidateOrigin::ALL {' 'for (name, target) in ROOT_ABI {' 'for mutation in RootAbiMutation::ALL {'; do \
		grep -Fq "$$loop" "$$tests/complete_evidence_races.rs"; \
	done; \
	for cardinality in 'RootAbiSeam::ALL.len(), 2' 'Epoch::ALL.len(), 2' 'USR_OUTCOMES.len(), 2' 'CandidateOrigin::ALL.len(), 2' 'ROOT_ABI.len(), 5' 'RootAbiMutation::ALL.len(), 3'; do \
		grep -Fq "$$cardinality" "$$tests/complete_evidence_races.rs"; \
	done; \
	test "$$( grep -Fc 'let mut seam_cases = 0;' "$$tests/complete_evidence_races.rs" )" = 1; \
	test "$$( grep -Fc 'seam_cases += 1;' "$$tests/complete_evidence_races.rs" )" = 1; \
	test "$$( grep -Fc 'assert_eq!(seam_cases, 120, "{seam:?}");' "$$tests/complete_evidence_races.rs" )" = 1; \
	grep -Fq 'assert!(!displaced_directory.path().starts_with(&root));' "$$tests/complete_evidence_races.rs"; \
	grep -Fq 'assert_exact_root_abi_mutation(' "$$tests/complete_evidence_races.rs"; \
	grep -Fq 'assert_eq!(cases, 240);' "$$tests/complete_evidence_races.rs"; \
	for hook in arm_between_usr_rollback_active_reblit_complete_route_database_captures arm_before_usr_rollback_active_reblit_complete_route_final_revalidation arm_before_usr_rollback_active_reblit_complete_route_fresh_namespace_capture; do \
		grep -Fq "$$hook" "$$tests/complete_evidence_races.rs"; \
	done; \
	grep -Fq 'assert_exact_no_boot_completion_plan' "$$tests/complete_matrix.rs"; \
	grep -Fq 'assert_complete_route_journal_only();' "$$tests/complete_matrix.rs"; \
	grep -Fq 'assert_no_boot_synchronize_attempts();' "$$tests/support.rs"; \
	grep -Fq 'retained_exchange_syscall_count(),' "$$tests/support.rs"; \
	grep -Fq 'ForwardPhase::UsrExchangeIntent | ForwardPhase::UsrExchanged' "$$finalization_authority"; \
	grep -Fq 'ForwardPhase::RootLinksComplete, 14' "$$finalization_authority"; \
	grep -Fq 'for source in CandidateSource::THROUGH_ROLLBACK_COMPLETE {' "$$tests/finalization_matrix.rs"; \
	grep -Fq 'assert_eq!(cases, 24);' "$$tests/finalization_matrix.rs"; \
	grep -Fq 'for source in CandidateSource::ALL {' "$$tests/finalization_process_kill.rs"; \
	grep -Fq 'CandidateSource::RootLinksComplete => {' "$$tests/finalization_process_kill.rs"; \
	grep -Fq 'unreachable!("RootLinksComplete is outside the later process-kill source axis")' "$$tests/finalization_process_kill.rs"; \
	if rg -n 'THROUGH_(CANDIDATE_PRESERVED|ROLLBACK_COMPLETE)' "$$tests/finalization_process_kill.rs"; then exit 1; else status="$$?"; test "$$status" = 1; fi; \
	grep -Fq '                    assert_eq!(candidate_preserved.generation, 13, "{case}");' "$$root_links_endpoint"; \
	grep -Fq '                    assert_eq!(rollback_complete.generation, 14, "{case}");' "$$root_links_endpoint"; \
	grep -Fq 'exact generation-14 RootLinks ActiveReblit terminal must finalize cleanly' "$$root_links_endpoint"; \
	grep -Fq 'finalized RootLinks ActiveReblit endpoint must remain clean' "$$root_links_endpoint"; \
	if rg -n 'pending\(&stable_entry\).*Phase::RollbackComplete|canonical_bytes\(\), complete_bytes' "$$root_links_endpoint"; then exit 1; else status="$$?"; test "$$status" = 1; fi; \
	test "$$( grep -Fc 'assert_eq!(active_reblit_candidate_preserve_exchange_attempt_count(), 1, "{case}");' "$$root_links_endpoint" )" = 3; \
	test "$$( grep -Fc 'CleanSystemStartup::enter(&fixture.system' "$$root_links_endpoint" )" = 6; \
	grep -Fq 'assert_eq!(archived_candidate_preserve_move_attempt_count(), 0, "{case}");' "$$root_links_endpoint"; \
	grep -Fq 'assert_eq!(boot_synchronize_attempt_count(), 0, "{case}");' "$$root_links_endpoint"; \
	grep -Fq 'assert_eq!(retained_exchange_syscall_count(), 1, "{case}");' "$$root_links_endpoint"; \
	grep -Fq 'assert_eq!(root_link_snapshot(&fixture), root_links_before, "{case}");' "$$root_links_endpoint"; \
	grep -Fq 'exact generation-14 RootLinks ActiveReblit terminal must finalize cleanly' "$$resume_make"; \
	for file in "$$gate" "$$orchestrator" "$$reconciliation" "$$recovery" "$$namespace" "$$authority" "$$proof" "$$executor" "$$boot_authority" "$$finalization_authority" $$route_tests "$$tests/support.rs" "$$root_links_endpoint" misc/make/startup-rollback-active-reblit-complete-route-tests.mk "$$resume_make"; do \
		test "$$( wc -l < "$$file" )" -le 1000; \
	done; \
	$(CARGO) test -p forge --lib startup_active_reblit_complete_route -- --test-threads=1
