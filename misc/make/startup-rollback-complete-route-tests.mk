.PHONY: forge-startup-usr-rollback-complete-route-test

forge-startup-usr-rollback-complete-route-test:
	@set -euo pipefail; \
	mkdir -p "$(TOP_DIR)/target"; \
	listed="$$( mktemp "$(TOP_DIR)/target/rollback-complete-route-list.XXXXXXXXXXXX" )"; \
	production_code="$$( mktemp "$(TOP_DIR)/target/rollback-complete-route-code.XXXXXXXXXXXX" )"; \
	trap 'rm -f "$$listed" "$$production_code"' EXIT; \
	$(CARGO) test -p forge --lib -- --list | tee "$$listed" >/dev/null; \
	grep -q . "$$listed"; \
	prefix='client::startup_recovery::usr_rollback_complete_route::tests::'; \
	count="$$( awk -v prefix="$$prefix" 'index($$0, prefix) == 1 && $$0 ~ /: test$$/ { count += 1 } END { print count + 0 }' "$$listed" )"; \
	test "$$count" = 18; \
	for name in \
		admission::startup_usr_rollback_complete_route_admits_exact_current_and_historical_joint_absence \
		admission::startup_usr_rollback_complete_route_defers_inexact_phase_plan_and_non_absent_database \
		evidence_races::startup_usr_rollback_complete_route_rejects_reopened_and_cross_root_journals \
		evidence_races::startup_usr_rollback_complete_route_capture_and_final_evidence_races_never_advance \
		evidence_races::startup_usr_rollback_complete_route_refuses_namespace_lookalikes \
		fresh_reopen::startup_usr_rollback_complete_route_source_durable_fresh_handle_reopen_retries_only_the_route \
		fresh_reopen::startup_usr_rollback_complete_route_successor_durable_fresh_handle_reopen_skips_the_route \
		matrix::startup_usr_rollback_complete_route_applied_matrix_persists_exact_rollback_complete \
		matrix::startup_usr_rollback_complete_route_already_satisfied_matrix_persists_exact_rollback_complete \
		record_binding::startup_usr_rollback_complete_route_bound_advance_same_byte_replacements_never_succeed \
		record_binding::startup_usr_rollback_complete_route_same_byte_successor_replacement_fails_reopened_binding \
		record_binding::startup_usr_rollback_complete_route_same_byte_successor_replacement_fails_same_store_binding \
		restart::startup_usr_rollback_complete_route_source_fault_restart_retries_only_the_completion_route \
		restart::startup_usr_rollback_complete_route_rollback_complete_fault_restart_skips_route_and_invalidation \
		root_abi_races::startup_root_links_complete_route_initial_revalidation_rejects_all_root_abi_mutations \
		root_abi_races::startup_root_links_complete_route_final_revalidation_rejects_all_root_abi_mutations \
		storage_reopen::startup_usr_rollback_complete_route_storage_faults_reopen_exact_fresh_db_invalidated_or_rollback_complete \
		storage_reopen::startup_usr_rollback_complete_route_consumes_old_store_and_returns_canonical_reopen; do \
		grep -Fqx "$$prefix$$name: test" "$$listed"; \
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
	tests=crates/forge/src/client/startup_recovery/usr_rollback_complete_route/tests; \
	candidate_support=crates/forge/src/client/startup_reconciliation/usr_rollback_candidate_preserve_authority/tests/support.rs; \
	dispatch_tests=crates/forge/src/client/startup_gate/usr_rollback_new_state/tests; \
	finalization_authority=crates/forge/src/client/startup_reconciliation/usr_rollback_finalization_authority.rs; \
	root_links_endpoint=crates/forge/src/client/startup_recovery/usr_rollback_resume_route/tests/root_links_route_endpoint.rs; \
	new_state_endpoint=crates/forge/src/client/startup_recovery/usr_rollback_fresh_db_invalidation_route/tests/endpoint.rs; \
	resume_make=misc/make/startup-rollback-resume-route-tests.mk; \
	test "$$( grep -Fc 'mod usr_rollback_complete_route;' "$$recovery_root" )" = 1; \
	test "$$( grep -Fc 'mod usr_rollback_complete_route_authority;' "$$reconciliation_root" )" = 1; \
	test "$$( grep -Fc 'mod rollback_complete_route_proof;' "$$namespace_root" )" = 1; \
	for module in admission evidence_races fresh_reopen matrix record_binding restart root_abi_races storage_reopen support; do \
		grep -Fqx "mod $$module;" "$$tests.rs"; \
	done; \
	test "$$( grep -Ec '^mod [a-z_]+;' "$$tests.rs" )" = 9; \
	grep -Fqx 'pub(in crate::client) struct UsrRollbackCompleteRouteSeal {' "$$production_dispatch"; \
	grep -Fq 'UsrRollbackCompleteRouteSeal' "$$startup_gate"; \
	test "$$( grep -Fc 'UsrRollbackCompleteRouteSeal::new();' "$$production_dispatch" )" = 1; \
	test "$$( grep -Fc 'UsrRollbackCompleteRouteAuthority::capture(' "$$production_dispatch" )" = 1; \
	test "$$( grep -Fc 'persist_usr_rollback_complete_route_and_reopen(journal, authority)?' "$$production_dispatch" )" = 1; \
	capture_call_count="$$( rg -n 'UsrRollbackCompleteRouteAuthority::capture\(' crates/forge/src/client --glob '*.rs' --glob '!**/tests/**' --glob '!**/tests.rs' --glob '!**/*_tests.rs' --glob '!**/*_tests/**' | wc -l )"; \
	test "$$capture_call_count" = 1; \
	persist_call_count="$$( rg -n 'persist_usr_rollback_complete_route_and_reopen\(' crates/forge/src/client --glob '*.rs' --glob '!**/tests/**' --glob '!**/tests.rs' --glob '!**/*_tests.rs' --glob '!**/*_tests/**' | wc -l )"; \
	test "$$persist_call_count" = 2; \
	grep -Fqx "pub(in crate::client) enum UsrRollbackCompleteRouteAdmission<'reservation> {" "$$authority"; \
	for variant in '    NotApplicable,' '    Deferred,' "    Ready(UsrRollbackCompleteRouteAuthority<'reservation>),"; do \
		grep -Fqx "$$variant" "$$authority"; \
	done; \
	if rg -U -n '#\[derive\([^]]*Clone[^]]*\)\]\n(?:#\[[^\n]*\]\n)*(?:pub\([^)]*\)[[:space:]]+)?(?:struct|enum)[[:space:]]+(UsrRollbackCompleteRoute(?:Authority|DatabaseEvidence|NamespaceProof)|ExactFreshTransitionAbsence|TransitionJournalRecordBinding)' "$$authority" "$$proof" "$$exact" crates/forge/src/transition_journal/store/record_binding.rs; then exit 1; else test "$$?" = 1; fi; \
	for field in \
		'record.operation == Operation::NewState' \
		'record.phase == Phase::FreshDbInvalidated' \
		'record.candidate.id.is_some()' \
		'ForwardPhase::UsrExchangeIntent | ForwardPhase::UsrExchanged | ForwardPhase::RootLinksComplete' \
		'rollback.previous_archive == RollbackAction::NotRequired' \
		'rollback.candidate.disposition == AbortDisposition::Quarantine' \
		'rollback.boot == BootRollback::NotRequired' \
		'rollback.external_effects_may_remain'; do \
		grep -Fq "$$field" "$$authority"; \
	done; \
	grep -Fqx '    context: DatabaseEvidence,' "$$authority"; \
	grep -Fqx '    absence: db::state::ExactFreshTransitionAbsence,' "$$authority"; \
	test "$$( grep -Fc 'inspect_exact_fresh_transition' "$$authority" )" = 2; \
	grep -Fq 'ownership: db::state::TransitionOwnership::Missing,' "$$authority"; \
	grep -Fq 'provenance: None,' "$$authority"; \
	if rg -n 'TransitionJournalBinding|journal\.binding\(\)|journal\.has_binding\(|journal\.load\(\)|journal\.advance\(' "$$authority" "$$proof" "$$executor"; then exit 1; else test "$$?" = 1; fi; \
	grep -Fqx '    journal_record_binding: TransitionJournalRecordBinding,' "$$authority"; \
	test "$$( grep -Fc 'journal.record_binding(installation.retained_mutable_cast_directory()?, record)?' "$$authority" )" = 1; \
	test "$$( grep -Fc 'require_journal_record_binding(' "$$authority" )" = 4; \
	binding_helper="$$( sed -n '/^fn require_journal_record_binding(/,/^}/p' "$$authority" )"; \
	grep -Fq '    if !journal.has_record_store_binding(binding) {' <<<"$$binding_helper"; \
	grep -Fq 'journal.has_record_binding(cast, binding, record)?' <<<"$$binding_helper"; \
	capture="$$( sed -n '/pub(in crate::client) fn capture(/,/pub(in crate::client) fn revalidate(/p' "$$authority" )"; \
	capture_binding="$$( grep -nF 'journal.record_binding(installation.retained_mutable_cast_directory()?, record)?' <<<"$$capture" | cut -d: -f1 )"; \
	capture_db_before="$$( grep -nF 'let database_before =' <<<"$$capture" | cut -d: -f1 )"; \
	capture_namespace="$$( grep -nF 'UsrRollbackCompleteRouteNamespaceInspection::begin' <<<"$$capture" | cut -d: -f1 )"; \
	capture_db_after="$$( grep -nF 'let database_after =' <<<"$$capture" | cut -d: -f1 )"; \
	test "$$capture_binding" -lt "$$capture_db_before"; \
	test "$$capture_db_before" -lt "$$capture_namespace"; \
	test "$$capture_namespace" -lt "$$capture_db_after"; \
	revalidate="$$( sed -n '/pub(in crate::client) fn revalidate(/,/pub(in crate::client) fn installation(/p' "$$authority" )"; \
	revalidate_first_binding="$$( grep -nF 'require_journal_record_binding(' <<<"$$revalidate" | head -n 1 | cut -d: -f1 )"; \
	revalidate_db_before="$$( grep -nF 'let database_before =' <<<"$$revalidate" | cut -d: -f1 )"; \
	revalidate_namespace="$$( grep -nF 'self.namespace.revalidate' <<<"$$revalidate" | cut -d: -f1 )"; \
	revalidate_db_after="$$( grep -nF 'let database_after =' <<<"$$revalidate" | cut -d: -f1 )"; \
	revalidate_last_binding="$$( grep -nF 'require_journal_record_binding(' <<<"$$revalidate" | tail -n 1 | cut -d: -f1 )"; \
	test "$$revalidate_first_binding" -lt "$$revalidate_db_before"; \
	test "$$revalidate_db_before" -lt "$$revalidate_namespace"; \
	test "$$revalidate_namespace" -lt "$$revalidate_db_after"; \
	test "$$revalidate_db_after" -lt "$$revalidate_last_binding"; \
	grep -Fq 'require_exact_new_state_fresh_db_invalidated_topology(expected, &before)?' "$$proof"; \
	grep -Fq 'run_before_fresh_namespace_capture();' "$$proof"; \
	grep -Fq 'journal.has_record_binding(cast, journal_record_binding, expected)?' "$$proof"; \
	test "$$( grep -Fc 'authority.revalidate(&journal)' "$$executor" )" = 1; \
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
	if rg -n '^[[:space:]]*(loop|while)[[:space:]]|^[[:space:]]*for[[:space:]].*[[:space:]]in[[:space:]]|diesel::|SqliteConnection|run_(transaction|system)_triggers|journal\.delete|remove_exact_fresh_transition|std::fs|(^|[^[:alnum:]_])fs::|(^|[^[:alnum:]_])nix::|transition_identity|linux_fs|rename(at)?|unlink(at)?|linkat|hard_link|symlink|sync_(all|data)|write_all|set_permissions|chmod|mkdir|create_dir|remove_(dir|file)|attempt_move|reconcile_move|finalize_usr_rollback|dispatch|retry|boot::|synchronize_boot' "$$production_code"; then exit 1; else test "$$?" = 1; fi; \
	grep -Fqx '    pub(super) const ALL: [Self; 2] = [Self::Intent, Self::Exchanged];' "$$candidate_support"; \
	rg -U -q '^    pub\(super\) const THROUGH_ROLLBACK_COMPLETE: \[Self; 3\] = \[\n        Self::Intent,\n        Self::Exchanged,\n        Self::RootLinksComplete,\n    \];' "$$candidate_support"; \
	for file in admission.rs matrix.rs restart.rs storage_reopen.rs record_binding.rs fresh_reopen.rs; do \
		grep -Fq 'Source::THROUGH_ROLLBACK_COMPLETE' "$$tests/$$file"; \
	done; \
	test "$$( grep -Fc 'assert_eq!(cases, 24, "{origin:?}");' "$$tests/matrix.rs" )" = 1; \
	grep -Fq 'assert_eq!(executions, 240);' "$$tests/storage_reopen.rs"; \
	grep -Fq 'assert_eq!(executions, 96);' "$$tests/record_binding.rs"; \
	test "$$( grep -Fc 'assert_eq!(executions, 48);' "$$tests/record_binding.rs" )" = 2; \
	for hook in BeforeBoundAdvancePublish BeforeBoundAdvanceFinalBinding arm_before_usr_rollback_complete_route_successor_binding_revalidation arm_after_usr_rollback_complete_route_successor_binding_check_before_reopen; do \
		grep -Fq "$$hook" "$$tests/record_binding.rs"; \
	done; \
	test "$$( grep -Fc 'assert_eq!(executions, 48);' "$$tests/fresh_reopen.rs" )" = 2; \
	test "$$( grep -Fc 'FreshCompleteRouteHandles::open(retained.path())' "$$tests/fresh_reopen.rs" )" = 2; \
	if rg -n 'Command::new|SIGKILL|process::' "$$tests/fresh_reopen.rs"; then exit 1; else test "$$?" = 1; fi; \
	test "$$( grep -Fc 'assert_eq!(executions, 240);' "$$tests/root_abi_races.rs" )" = 2; \
	for loop in 'for historical in [false, true] {' 'for origin in FreshDbOutcome::ALL {' 'for usr_outcome in [RollbackActionOutcome::Applied, RollbackActionOutcome::AlreadySatisfied] {' 'for candidate_outcome in CandidateResult::ALL {' 'for (name, target) in ROOT_ABI {' 'for mutation in RootAbiMutation::ALL {'; do \
		test "$$( grep -Fc "$$loop" "$$tests/root_abi_races.rs" )" = 2; \
	done; \
	grep -Fq 'assert!(!displaced_directory.path().starts_with(&root));' "$$tests/root_abi_races.rs"; \
	test "$$( grep -Fc 'assert_exact_root_abi_mutation(' "$$tests/root_abi_races.rs" )" = 3; \
	for hook in arm_before_usr_rollback_complete_route_final_revalidation arm_before_usr_rollback_complete_route_fresh_namespace_capture; do \
		grep -Fq "$$hook" "$$tests/root_abi_races.rs"; \
	done; \
	grep -Fq 'for source in CandidateSource::THROUGH_ROLLBACK_COMPLETE {' "$$dispatch_tests/matrix.rs"; \
	grep -Fq 'assert_eq!(cases, 48);' "$$dispatch_tests/matrix.rs"; \
	grep -Fq 'ForwardPhase::UsrExchangeIntent | ForwardPhase::UsrExchanged' "$$finalization_authority"; \
	grep -Fq 'ForwardPhase::RootLinksComplete, 18' "$$finalization_authority"; \
	grep -Fq 'for source in CandidateSource::ALL {' "$$dispatch_tests/matrix.rs"; \
	grep -Fq 'for source in CandidateSource::ALL {' "$$dispatch_tests/terminal_delete_process_kill.rs"; \
	grep -Fq 'for source in CandidateSource::ALL {' "$$dispatch_tests/candidate_move_process_kill.rs"; \
	if rg -n 'THROUGH_ROLLBACK_COMPLETE' "$$dispatch_tests/finalization.rs" "$$dispatch_tests/terminal_delete_process_kill.rs" "$$dispatch_tests/candidate_move_process_kill.rs"; then exit 1; else test "$$?" = 1; fi; \
	grep -Fq 'assert_eq!(rollback_complete.generation, 18, "{case}");' "$$root_links_endpoint"; \
	grep -Fq 'assert_eq!(rollback_complete.generation, 12, "{case}");' "$$root_links_endpoint"; \
	grep -Fq 'assert_eq!(rollback_complete.generation, 14, "{case}");' "$$root_links_endpoint"; \
	grep -Fq 'assert_eq!(complete.generation, 18, "{case}");' "$$new_state_endpoint"; \
	grep -Fq '.expect("exact generation-18 RootLinks NewState terminal must finalize cleanly");' "$$new_state_endpoint"; \
	grep -Fq '.expect("finalized RootLinks NewState endpoint must remain clean");' "$$new_state_endpoint"; \
	grep -Fq 'assert_eq!(rollback_complete.generation, 18, "{case}");' "$$resume_make"; \
	for file in "$$executor" "$$authority" "$$proof" "$$candidate_proof" "$$exact" "$$reopen" "$$startup_gate" "$$recovery_root" "$$reconciliation_root" "$$namespace_root" "$$tests.rs" "$$tests"/*.rs "$$candidate_support" "$$dispatch_tests/matrix.rs" "$$dispatch_tests/finalization.rs" "$$dispatch_tests/terminal_delete_process_kill.rs" "$$dispatch_tests/candidate_move_process_kill.rs" "$$finalization_authority" "$$root_links_endpoint" "$$new_state_endpoint" misc/make/startup-rollback-complete-route-tests.mk "$$resume_make"; do \
		test "$$( wc -l < "$$file" )" -le 1000; \
	done; \
	$(CARGO) test -p forge --lib "$$prefix" -- --test-threads=1
