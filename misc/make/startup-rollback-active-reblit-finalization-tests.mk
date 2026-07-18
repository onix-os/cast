.PHONY: forge-startup-usr-rollback-active-reblit-finalization-test

forge-startup-usr-rollback-active-reblit-finalization-test:
	@set -euo pipefail; \
	timeout 10s mkdir -p "$(TOP_DIR)/target"; \
	listed="$$( timeout 10s mktemp "$(TOP_DIR)/target/active-reblit-finalization-list.XXXXXXXXXXXX" )"; \
	refs="$$( timeout 10s mktemp "$(TOP_DIR)/target/active-reblit-finalization-refs.XXXXXXXXXXXX" )"; \
	executor_code="$$( timeout 10s mktemp "$(TOP_DIR)/target/active-reblit-finalization-code.XXXXXXXXXXXX" )"; \
	trap 'timeout 10s rm -f "$$listed" "$$refs" "$$executor_code"' EXIT; \
	timeout 300s $(CARGO) test -p forge --lib -- --list | timeout 30s tee "$$listed" >/dev/null; \
	timeout 10s grep -q . "$$listed"; \
	gate_prefix='client::startup_gate::usr_rollback_active_reblit::tests::finalization_'; \
	timeout 10s test "$$( timeout 10s grep -c "^$$gate_prefix.*: test$$" "$$listed" )" = 17; \
	for name in \
		authority_binding::startup_active_reblit_finalization_authority_covers_both_indices_in_both_epochs_and_rejects_wrong_bindings \
		authority_binding::startup_active_reblit_finalization_authority_refuses_terminal_candidate_without_exact_previous_identity \
		boundaries::startup_active_reblit_finalization_keeps_candidate_preserved_and_terminal_deletion_on_separate_entries \
		boundaries::startup_active_reblit_finalization_keeps_new_state_and_archived_in_their_own_routes \
		boundaries::startup_active_reblit_finalization_rejects_a_valid_terminal_lookalike_plan_and_wrong_topology \
		evidence_races::startup_active_reblit_finalization_refuses_wrong_database_and_provenance_evidence \
		evidence_races::startup_active_reblit_finalization_rejects_capture_and_final_pre_evidence_races \
		evidence_races::startup_active_reblit_finalization_rejects_database_and_provenance_changes_at_final_pre_and_after_delete \
		lock_handoff::startup_active_reblit_finalization_hands_the_same_lock_into_clean_startup_until_proof_drop \
		matrix::startup_active_reblit_finalization_covers_all_sixteen_exact_terminal_cases_and_both_wrapper_indices \
		process_kill::startup_active_reblit_finalization_process_kills_restart_cleanly \
		public_binding_races::startup_active_reblit_finalization_rejects_hidden_entry_set_substitution \
		public_binding_races::startup_active_reblit_finalization_rejects_public_directory_and_lock_substitution \
		public_binding_races::startup_active_reblit_finalization_rejects_source_recreation_after_delete_and_after_absence_proof \
		restart::startup_active_reblit_finalization_restarts_from_observed_absence_with_fresh_handles \
		restart::startup_active_reblit_finalization_restarts_from_retained_terminal_source_with_fresh_handles \
		storage_faults::startup_active_reblit_finalization_classifies_both_terminal_delete_faults_and_converges; do \
		timeout 10s grep -Fqx "$$gate_prefix$$name: test" "$$listed"; \
	done; \
	executor_prefix='client::startup_recovery::usr_rollback_active_reblit_finalization::tests::reconcile_delete::'; \
	timeout 10s test "$$( timeout 10s grep -c "^$$executor_prefix.*: test$$" "$$listed" )" = 4; \
	for name in \
		active_reblit_finalization_delete_error_preserves_ambiguous_verification \
		active_reblit_finalization_false_delete_report_classifies_authenticated_absence \
		active_reblit_finalization_false_delete_report_classifies_exact_source \
		active_reblit_finalization_false_delete_report_rejects_an_unexpected_record; do \
		timeout 10s grep -Fqx "$$executor_prefix$$name: test" "$$listed"; \
	done; \
	authority=crates/forge/src/client/startup_reconciliation/usr_rollback_active_reblit_finalization_authority.rs; \
	proof=crates/forge/src/client/startup_reconciliation/activation_namespace/active_reblit_finalization_proof.rs; \
	topology=crates/forge/src/client/startup_reconciliation/activation_namespace/candidate_preserve_proof.rs; \
	executor=crates/forge/src/client/startup_recovery/usr_rollback_active_reblit_finalization.rs; \
	orchestrator=crates/forge/src/client/startup_gate/usr_rollback_active_reblit.rs; \
	startup_gate=crates/forge/src/client/startup_gate.rs; \
	recovery_root=crates/forge/src/client/startup_recovery.rs; \
	reconciliation_root=crates/forge/src/client/startup_reconciliation.rs; \
	namespace_root=crates/forge/src/client/startup_reconciliation/activation_namespace.rs; \
	gate_tests=crates/forge/src/client/startup_gate/usr_rollback_active_reblit/tests; \
	process_kill="$$gate_tests/finalization_process_kill.rs"; \
	executor_tests=crates/forge/src/client/startup_recovery/usr_rollback_active_reblit_finalization/tests; \
	timeout 10s grep -Fqx 'mod usr_rollback_active_reblit_finalization_authority;' "$$reconciliation_root"; \
	timeout 10s grep -Fqx 'mod active_reblit_finalization_proof;' "$$namespace_root"; \
	timeout 10s grep -Fqx 'mod usr_rollback_active_reblit_finalization;' "$$recovery_root"; \
	for symbol in UsrRollbackActiveReblitFinalizationAdmission UsrRollbackActiveReblitFinalizationAuthority UsrRollbackActiveReblitFinalizationAuthorityError; do \
		timeout 10s grep -Fq "$$symbol" "$$reconciliation_root"; \
	done; \
	for symbol in DurableUsrRollbackActiveReblitFinalizationRecord UsrRollbackActiveReblitFinalizationError UsrRollbackActiveReblitFinalizationVerificationError finalize_usr_rollback_active_reblit; do \
		timeout 10s grep -Fq "$$symbol" "$$recovery_root"; \
	done; \
	for module in finalization_authority_binding finalization_boundaries finalization_evidence_races finalization_lock_handoff finalization_matrix finalization_process_kill finalization_public_binding_races finalization_restart finalization_storage_faults; do \
		timeout 10s grep -Fqx "mod $$module;" "$$gate_tests/mod.rs"; \
	done; \
	for module in reconcile_delete support; do timeout 10s grep -Fqx "mod $$module;" "$$executor_tests/mod.rs"; done; \
	timeout 10s grep -Fqx 'pub(in crate::client) struct UsrRollbackActiveReblitFinalizationSeal {' "$$orchestrator"; \
	seal_impl="$$( timeout 10s sed -n '/^impl UsrRollbackActiveReblitFinalizationSeal {/,/^}/p' "$$orchestrator" )"; \
	timeout 10s test "$$( timeout 10s grep -Fc '    fn new() -> Self {' <<<"$$seal_impl" )" = 1; \
	timeout 10s test "$$( timeout 10s grep -Fc '    pub(in crate::client) fn new_for_test() -> Self {' <<<"$$seal_impl" )" = 1; \
	timeout 10s awk '$$0 == "    #[cfg(test)]" { gated = 1; next } gated && $$0 == "    pub(in crate::client) fn new_for_test() -> Self {" { found++; gated = 0; next } gated { gated = 0 } END { exit found != 1 }' <<<"$$seal_impl"; \
	timeout 10s grep -Fq '        Self::new()' <<<"$$seal_impl"; \
	timeout 10s rg -U -q '^pub\(in crate::client\) use usr_rollback_active_reblit::\{\n    UsrRollbackActiveReblitBootRepairRequiredSeal, UsrRollbackActiveReblitCompleteRouteSeal,\n    UsrRollbackActiveReblitFinalizationSeal,\n\};' "$$startup_gate"; \
	if timeout 10s awk 'previous == "#[cfg(test)]" && $$0 == "pub(in crate::client) use usr_rollback_active_reblit::{" { found = 1 } { previous = $$0 } END { exit !found }' "$$startup_gate"; then exit 1; else status="$$?"; timeout 10s test "$$status" = 1; fi; \
	timeout 10s grep -Fqx 'pub(in crate::client) enum UsrRollbackActiveReblitFinalizationAdmission<'\''reservation> {' "$$authority"; \
	timeout 10s grep -Fqx 'pub(in crate::client) struct UsrRollbackActiveReblitFinalizationAuthority<'\''reservation> {' "$$authority"; \
	timeout 10s grep -Fq '    journal_binding: TransitionJournalBinding,' "$$authority"; \
	timeout 10s grep -Fq '    installation: Installation,' "$$authority"; \
	timeout 10s grep -Fq '    state_db: db::state::Database,' "$$authority"; \
	timeout 10s grep -Fq '    record: TransitionRecord,' "$$authority"; \
	timeout 10s grep -Fq '    database: UsrRollbackActiveReblitFinalizationDatabaseEvidence,' "$$authority"; \
	timeout 10s grep -Fq '    namespace: UsrRollbackActiveReblitFinalizationNamespaceProof,' "$$authority"; \
	timeout 10s grep -Fq '    _active_state_reservation: &'\''reservation ActiveStateReservation,' "$$authority"; \
	timeout 10s grep -Fq '_startup_gate_seal: &UsrRollbackActiveReblitFinalizationSeal,' "$$authority"; \
	timeout 10s grep -Fq 'let journal_binding = journal.binding();' "$$authority"; \
	timeout 10s grep -Fq 'if !journal.has_binding(&journal_binding)' "$$authority"; \
	timeout 10s grep -Fq 'if !journal.has_binding(&self.journal_binding)' "$$authority"; \
	timeout 10s grep -Fq 'let retained_state_db = state_db.clone();' "$$authority"; \
	timeout 10s grep -Fq 'debug_assert!(retained_state_db.same_instance(state_db));' "$$authority"; \
	timeout 10s grep -Fq 'record.operation != Operation::ActiveReblit || record.phase != Phase::RollbackComplete' "$$authority"; \
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
	timeout 10s grep -Fq 'existing.ownership == db::state::TransitionOwnership::Cleared' "$$authority"; \
	timeout 10s grep -Fq 'existing.state == candidate' "$$authority"; \
	timeout 10s grep -Fq 'provenance: Some(_),' "$$authority"; \
	timeout 10s grep -Fq 'previous: None,' "$$authority"; \
	capture="$$( timeout 10s sed -n '/pub(in crate::client) fn capture(/,/pub(in crate::client) fn revalidate(/p' "$$authority" )"; \
	binding_line="$$( timeout 10s grep -nF 'let journal_binding = journal.binding();' <<<"$$capture" | timeout 10s cut -d: -f1 )"; \
	database_before_line="$$( timeout 10s grep -nF 'let database_before = match inspect_current_database(record, state_db)? {' <<<"$$capture" | timeout 10s cut -d: -f1 )"; \
	namespace_begin_line="$$( timeout 10s grep -nF 'match UsrRollbackActiveReblitFinalizationNamespaceInspection::begin(installation, journal, record)' <<<"$$capture" | timeout 10s cut -d: -f1 )"; \
	namespace_finish_line="$$( timeout 10s grep -nF 'let namespace = match namespace_inspection.finish(installation, journal, record) {' <<<"$$capture" | timeout 10s cut -d: -f1 )"; \
	database_after_line="$$( timeout 10s grep -nF 'let database_after = match inspect_current_database(record, state_db)? {' <<<"$$capture" | timeout 10s cut -d: -f1 )"; \
	timeout 10s test "$$binding_line" -lt "$$database_before_line"; \
	timeout 10s test "$$database_before_line" -lt "$$namespace_begin_line"; \
	timeout 10s test "$$namespace_begin_line" -lt "$$namespace_finish_line"; \
	timeout 10s test "$$namespace_finish_line" -lt "$$database_after_line"; \
	if timeout 10s rg -U -n '#\[derive\([^]]*Clone[^]]*\)\]\n(?:#\[[^\n]*\]\n)*(?:pub\([^)]*\)[[:space:]]+)?(?:struct|enum)[[:space:]]+(UsrRollbackActiveReblitFinalization(?:Authority|DatabaseEvidence|NamespaceProof))' "$$authority" "$$proof"; then exit 1; else status="$$?"; timeout 10s test "$$status" = 1; fi; \
	if timeout 10s rg -n 'impl Clone for UsrRollbackActiveReblitFinalization(?:Authority|DatabaseEvidence|NamespaceProof)' "$$authority" "$$proof"; then exit 1; else status="$$?"; timeout 10s test "$$status" = 1; fi; \
	if timeout 10s rg -n '\.delete\(|dispatch_usr_|persist_usr_|fn new\(\) -> Self' "$$authority" "$$proof"; then exit 1; else status="$$?"; timeout 10s test "$$status" = 1; fi; \
	if timeout 10s rg -n 'UsrRollbackFinalization(?:Authority|NamespaceProof)|UsrRollbackActiveReblitCompleteRoute(?:Authority|NamespaceProof)' "$$authority" "$$proof"; then exit 1; else status="$$?"; timeout 10s test "$$status" = 1; fi; \
	timeout 10s rg -n -F 'UsrRollbackActiveReblitFinalizationAuthority::capture(' crates/forge/src/client --glob '*.rs' --glob '!**/tests/**' --glob '!**/tests.rs' --glob '!**/*_tests.rs' --glob '!**/*_tests/**' --glob '!**/usr_rollback_active_reblit_finalization_authority.rs' > "$$refs"; \
	timeout 10s test "$$( timeout 10s wc -l < "$$refs" )" = 1; \
	timeout 10s test "$$( timeout 10s cut -d: -f1 "$$refs" )" = "$$orchestrator"; \
	timeout 10s grep -Fqx '        Phase::RollbackComplete => {' "$$orchestrator"; \
	timeout 10s test "$$( timeout 10s grep -Fc 'UsrRollbackActiveReblitFinalizationAuthority::capture(' "$$orchestrator" )" = 1; \
	timeout 10s test "$$( timeout 10s grep -Fc 'finalize_usr_rollback_active_reblit(journal, authority)?' "$$orchestrator" )" = 1; \
	timeout 10s grep -Fq 'Ok(Dispatch::Finalized { journal })' "$$orchestrator"; \
	candidate_line="$$( timeout 10s grep -nF 'Phase::CandidatePreserveIntent => {' "$$orchestrator" | timeout 10s cut -d: -f1 )"; \
	complete_line="$$( timeout 10s grep -nF 'Phase::CandidatePreserved => {' "$$orchestrator" | timeout 10s cut -d: -f1 )"; \
	finalize_line="$$( timeout 10s grep -nF 'Phase::RollbackComplete => {' "$$orchestrator" | timeout 10s cut -d: -f1 )"; \
	timeout 10s test "$$candidate_line" -lt "$$complete_line"; \
	timeout 10s test "$$complete_line" -lt "$$finalize_line"; \
	timeout 10s grep -Fq 'usr_rollback_active_reblit::Dispatch::Finalized { journal } => {' "$$startup_gate"; \
	timeout 10s grep -Fq 'return Self::admit_clean_after_terminal_finalization(installation, state_db, journal);' "$$startup_gate"; \
	timeout 10s grep -Fq 'return Self::admit_clean(installation, state_db, journal, in_flight);' "$$startup_gate"; \
	timeout 10s rg -U -q '^pub\(in crate::client\) fn finalize_usr_rollback_active_reblit\(\n    journal: TransitionJournalStore,\n    authority: UsrRollbackActiveReblitFinalizationAuthority<'\''_>,\n\) -> Result<TransitionJournalStore, UsrRollbackActiveReblitFinalizationError> \{' "$$executor"; \
	timeout 10s test "$$( timeout 10s grep -Fc '.revalidate(&journal)' "$$executor" )" = 2; \
	timeout 10s test "$$( timeout 10s grep -Fc 'journal.delete_revalidated_retained_cast(cast, &source_record)' "$$executor" )" = 1; \
	timeout 10s test "$$( timeout 10s grep -Fc 'journal.load_revalidated_retained_cast(cast)' "$$executor" )" = 1; \
	timeout 10s test "$$( timeout 10s grep -Fc 'require_exact_public_source(&installation, &journal, &source_record)' "$$executor" )" = 2; \
	timeout 10s grep -Fq 'authority.revalidate_after_journal_delete(journal)?' "$$executor"; \
	timeout 10s grep -Fq 'Ok(DurableUsrRollbackActiveReblitFinalizationRecord::Absent) => Ok(journal),' "$$executor"; \
	timeout 10s grep -Fq 'Ok(true) => match durable {' "$$executor"; \
	timeout 10s grep -Fq 'Ok(false) => match durable {' "$$executor"; \
	timeout 10s grep -Fq 'Err(source) => match durable {' "$$executor"; \
	timeout 10s grep -Fq 'Ok(DurableUsrRollbackActiveReblitFinalizationRecord::RollbackComplete)' "$$executor"; \
	timeout 10s grep -Fq 'DeleteReportedFalse { durable }' "$$executor"; \
	timeout 10s grep -Fq 'DeleteReportedFalseAndVerification { source }' "$$executor"; \
	timeout 10s grep -Fq 'Delete { durable, source }' "$$executor"; \
	timeout 10s grep -Fq 'DeleteAndVerification {' "$$executor"; \
	timeout 10s test "$$( timeout 10s grep -Fc 'classify_exact_or_absent(' "$$executor" )" = 3; \
	timeout 10s grep -Fq 'require_exact_active_reblit_rollback_complete_topology' "$$proof" "$$topology"; \
	timeout 10s grep -Fq '    before: NamespaceSnapshot,' "$$proof"; \
	timeout 10s grep -Fq '    after: NamespaceSnapshot,' "$$proof"; \
	inspection_struct="$$( timeout 10s sed -n '/struct UsrRollbackActiveReblitFinalizationNamespaceInspection {/,/^}/p' "$$proof" )"; \
	proof_struct="$$( timeout 10s sed -n '/struct UsrRollbackActiveReblitFinalizationNamespaceProof {/,/^}/p' "$$proof" )"; \
	timeout 10s test "$$( timeout 10s grep -Fc '    wrapper_index: usize,' <<<"$$inspection_struct" )" = 1; \
	timeout 10s test "$$( timeout 10s grep -Fc '    wrapper_index: usize,' <<<"$$proof_struct" )" = 1; \
	timeout 10s grep -Fq 'require_matching_fingerprints(&self.before, &self.after)?;' "$$proof"; \
	timeout 10s grep -Fq 'let fresh = capture_snapshot(installation, expected)?;' "$$proof"; \
	timeout 10s grep -Fq 'require_matching_fingerprints(&self.before, &fresh)?;' "$$proof"; \
	timeout 10s grep -Fq 'require_exact_wrapper_index(expected, &fresh, self.wrapper_index)?;' "$$proof"; \
	timeout 10s grep -Fq 'pub(in crate::client::startup_reconciliation) fn revalidate_after_journal_delete(' "$$proof"; \
	timeout 10s test "$$( timeout 10s grep -Fc 'require_exact_public_journal_absence(installation, journal)?;' "$$proof" )" = 2; \
	if timeout 10s rg -n 'canonical_journal_reopen|reopen_canonical_journal|finalize_usr_rollback_active_reblit_and_reopen' "$$executor" "$$executor_tests"; then exit 1; else status="$$?"; timeout 10s test "$$status" = 1; fi; \
	if timeout 10s rg -n 'journal\.delete[[:space:]]*\(|\.delete[[:space:]]*\(|TransitionJournalStore::(?:open|try_open)' "$$executor"; then exit 1; else status="$$?"; timeout 10s test "$$status" = 1; fi; \
	timeout 10s sed -E 's,//.*$$,,' "$$executor" > "$$executor_code"; \
	if timeout 10s rg -n '^[[:space:]]*(loop|while|for)[[:space:]]|=[[:space:]]*(loop|while|for)[[:space:]]|retry|cleanup' "$$executor_code"; then exit 1; else status="$$?"; timeout 10s test "$$status" = 1; fi; \
	if timeout 10s rg -n 'diesel::|SqliteConnection|sql_query|\.execute[[:space:]]*\(|\.transaction[[:space:]]*\(|\.advance[[:space:]]*\(|run_(transaction|system)_triggers|root_links|archive_previous|clear_transition_if_matches|remove_transition_if_matches|remove_exact_|\.add[[:space:]]*\(|\.create[[:space:]]*\(|\.remove[[:space:]]*\(|\.batch_remove[[:space:]]*\(' "$$executor_code"; then exit 1; else status="$$?"; timeout 10s test "$$status" = 1; fi; \
	if timeout 10s rg -n 'transition_identity|linux_fs|std::fs|nix::|renameat|unlinkat|linkat|sync_(all|data)|write_all|set_permissions|chmod|mkdir|create_dir|remove_(dir|file)|hard_link|symlink' "$$executor_code"; then exit 1; else status="$$?"; timeout 10s test "$$status" = 1; fi; \
	for hook in arm_between_usr_rollback_active_reblit_finalization_database_captures arm_before_usr_rollback_active_reblit_finalization_fresh_namespace_capture arm_before_usr_rollback_active_reblit_finalization_final_revalidation arm_after_usr_rollback_active_reblit_finalization_delete arm_before_usr_rollback_active_reblit_finalization_final_durable_inspection; do \
		timeout 10s rg -q "$$hook" "$$authority" "$$proof" "$$executor" "$$reconciliation_root" "$$recovery_root"; \
	done; \
	for fault in arm_next_delete_canonical_unlink_fault arm_next_delete_directory_sync_fault assert_delete_canonical_unlink_fault_consumed assert_delete_directory_sync_fault_consumed; do timeout 10s grep -Fq "$$fault" "$$gate_tests/finalization_storage_faults.rs"; done; \
	for loop in 'for epoch in Epoch::ALL {' 'for source in CandidateSource::ALL {' 'for usr_outcome in USR_OUTCOMES {' 'for candidate_outcome in CandidateOrigin::ALL {'; do timeout 10s grep -Fq "$$loop" "$$gate_tests/finalization_matrix.rs"; done; \
	for cardinality in 'Epoch::ALL.len(), 2' 'CandidateSource::ALL.len(), 2' 'USR_OUTCOMES.len(), 2' 'CandidateOrigin::ALL.len(), 2'; do timeout 10s grep -Fq "$$cardinality" "$$gate_tests/finalization_matrix.rs"; done; \
	timeout 10s grep -Fq 'pub(super) const WRAPPER_INDICES: [usize; 2] = [0, WRAPPER_INDEX];' "$$gate_tests/support.rs"; \
	timeout 10s grep -Fq 'assert_eq!(WRAPPER_INDICES, [0, WRAPPER_INDEX]);' "$$gate_tests/finalization_matrix.rs"; \
	timeout 10s grep -Fq 'assert_eq!(cases, 16);' "$$gate_tests/finalization_matrix.rs"; \
	timeout 10s grep -Fq 'for wrapper_index in WRAPPER_INDICES {' "$$gate_tests/finalization_authority_binding.rs"; \
	timeout 10s grep -Fq 'const ALL: [Self; 3] = [' "$$process_kill"; \
	timeout 10s grep -Fq 'for epoch in Epoch::ALL {' "$$process_kill"; \
	timeout 10s grep -Fq 'for source in CandidateSource::ALL {' "$$process_kill"; \
	timeout 10s grep -Fq 'for boundary in FinalizationKillBoundary::ALL {' "$$process_kill"; \
	timeout 10s grep -Fq '        cases, 12,' "$$process_kill"; \
	timeout 10s test "$$( timeout 10s grep -Fc ') => Self {' "$$process_kill" )" = 4; \
	timeout 10s rg -U -q '\(ProcessEpoch::Current, ProcessSource::Intent\) => Self \{\n[[:space:]]+usr_outcome: RollbackActionOutcome::Applied,\n[[:space:]]+candidate_origin: CandidateOrigin::Applied,\n[[:space:]]+wrapper_index: 0,\n[[:space:]]+\},' "$$process_kill"; \
	timeout 10s rg -U -q '\(ProcessEpoch::Current, ProcessSource::Exchanged\) => Self \{\n[[:space:]]+usr_outcome: RollbackActionOutcome::Applied,\n[[:space:]]+candidate_origin: CandidateOrigin::AlreadySatisfied,\n[[:space:]]+wrapper_index: 13,\n[[:space:]]+\},' "$$process_kill"; \
	timeout 10s rg -U -q '\(ProcessEpoch::Historical, ProcessSource::Intent\) => Self \{\n[[:space:]]+usr_outcome: RollbackActionOutcome::AlreadySatisfied,\n[[:space:]]+candidate_origin: CandidateOrigin::Applied,\n[[:space:]]+wrapper_index: 13,\n[[:space:]]+\},' "$$process_kill"; \
	timeout 10s rg -U -q '\(ProcessEpoch::Historical, ProcessSource::Exchanged\) => Self \{\n[[:space:]]+usr_outcome: RollbackActionOutcome::AlreadySatisfied,\n[[:space:]]+candidate_origin: CandidateOrigin::AlreadySatisfied,\n[[:space:]]+wrapper_index: 0,\n[[:space:]]+\},' "$$process_kill"; \
	timeout 10s grep -Fq 'arm_before_usr_rollback_active_reblit_finalization_final_revalidation(kill_self)' "$$process_kill"; \
	timeout 10s test "$$( timeout 10s grep -Fc 'arm_journal_delete_durability_callback(' "$$process_kill" )" = 2; \
	for boundary in CanonicalUnlinked DeleteDirectorySynced; do timeout 10s grep -Fq "JournalDeleteDurabilityBoundary::$$boundary" "$$process_kill"; done; \
	timeout 10s test "$$( timeout 10s grep -Fc 'CleanSystemStartup::enter(' "$$process_kill" )" = 2; \
	timeout 10s grep -Fq 'let installation = Installation::open(&case.root, None).unwrap();' "$$process_kill"; \
	timeout 10s grep -Fq 'let database = open_state_database(&installation);' "$$process_kill"; \
	timeout 10s grep -Fq 'Command::new(env::current_exe().unwrap())' "$$process_kill"; \
	timeout 10s grep -Fq '.arg(TEST_NAME)' "$$process_kill"; \
	timeout 10s grep -Fq '.arg("--exact")' "$$process_kill"; \
	timeout 10s grep -Fq '.arg("--test-threads=1")' "$$process_kill"; \
	timeout 10s grep -Fq 'Some(nix::libc::SIGKILL)' "$$process_kill"; \
	timeout 10s grep -Fq 'nix::libc::kill(nix::libc::getpid(), nix::libc::SIGKILL)' "$$process_kill"; \
	timeout 10s grep -Fq 'const CHILD_DEADLINE: Duration = Duration::from_secs(15);' "$$process_kill"; \
	timeout 10s grep -Fq 'struct DeadlineChild {' "$$process_kill"; \
	timeout 10s test "$$( timeout 10s grep -Fc '.wait(CHILD_DEADLINE)' "$$process_kill" )" = 2; \
	timeout 10s grep -Fq 'if let Some(status) = self.child.as_mut().unwrap().try_wait().unwrap() {' "$$process_kill"; \
	timeout 10s grep -Fq 'if Instant::now() >= deadline {' "$$process_kill"; \
	timeout 10s grep -Fq 'impl Drop for DeadlineChild {' "$$process_kill"; \
	timeout 10s grep -Fq 'let control = tempfile::tempdir().unwrap();' "$$process_kill"; \
	timeout 10s grep -Fq 'assert_separate_control_path(&root, &control_path);' "$$process_kill"; \
	timeout 10s grep -Fq 'external ActiveReblit process-kill control does not match the terminal case' "$$process_kill"; \
	timeout 10s grep -Fq 'let terminal_bytes = fs::read(canonical_path(&root)).unwrap();' "$$process_kill"; \
	timeout 10s grep -Fq 'install_persistent_database(&mut fixture);' "$$process_kill"; \
	timeout 10s grep -Fq 'let retained_root = release_candidate_handles(fixture);' "$$process_kill"; \
	timeout 10s grep -Fqx 'struct ExistingCandidateDatabase {' "$$process_kill"; \
	timeout 10s grep -Fq 'assert_eq!(record.candidate.id, record.previous.id);' "$$process_kill"; \
	timeout 10s grep -Fq 'assert_eq!(in_flight, None);' "$$process_kill"; \
	timeout 10s grep -Fq 'assert_eq!(ownership, db::state::TransitionOwnership::Cleared);' "$$process_kill"; \
	timeout 10s grep -Fq '.expect("ActiveReblit candidate provenance must remain present");' "$$process_kill"; \
	timeout 10s test "$$( timeout 10s grep -Fc 'ExistingCandidateDatabase::capture(' "$$process_kill" )" = 6; \
	timeout 10s grep -Fq 'let wrapper = active_wrapper_path_at(&fixture, dimensions.wrapper_index);' "$$process_kill"; \
	timeout 10s test "$$( timeout 10s grep -Fc 'assert_wrapper(' "$$process_kill" )" = 7; \
	timeout 10s test "$$( timeout 10s grep -Fc 'snapshot_startup_recovery_namespace(' "$$process_kill" )" = 6; \
	timeout 10s grep -Fqx 'struct PublicJournalIdentity {' "$$process_kill"; \
	timeout 10s grep -Fq 'assert_journal_inventory(root, canonical_present);' "$$process_kill"; \
	timeout 10s grep -Fq 'public_before.assert_same_public_anchors(final_public);' "$$process_kill"; \
	timeout 10s grep -Fq 'StorageError::AcquireLock' "$$process_kill"; \
	timeout 10s grep -Fq 'assert_eq!(reopened.load().unwrap(), None)' "$$process_kill"; \
	timeout 10s grep -Fq 'arm_journal_update_durability_callback(JournalUpdateDurabilityBoundary::TemporaryFullySynced' "$$process_kill"; \
	timeout 10s test "$$( timeout 10s grep -Fc 'assert_no_candidate_effects()' "$$process_kill" )" = 4; \
	timeout 10s grep -Fq 'not a reboot or power-loss oracle' "$$process_kill"; \
	if timeout 10s rg -n 'arm_next_|assert_.*fault_consumed|finalize_usr_rollback_active_reblit[[:space:]]*\(|capture_finalization_ready[[:space:]]*\(|enter_clean_(candidate|fresh_handles)[[:space:]]*\(|FaultPoint|StorageFault' "$$process_kill"; then exit 1; else status="$$?"; timeout 10s test "$$status" = 1; fi; \
	timeout 10s test "$$( timeout 10s grep -Fc 'release_candidate_handles(fixture)' "$$gate_tests/finalization_restart.rs" )" = 2; \
	timeout 10s test "$$( timeout 10s grep -Fc 'assert_fresh_existing_candidate_database(' "$$gate_tests/finalization_restart.rs" )" = 4; \
	for disclaimer in 'fresh process-like' 'do not claim SIGKILL' 'reboot' 'power-loss durability'; do timeout 10s grep -Fq "$$disclaimer" "$$gate_tests/finalization_restart.rs"; done; \
	timeout 10s grep -Fq 'StorageError::AcquireLock' "$$gate_tests/finalization_lock_handoff.rs"; \
	timeout 10s grep -Fq 'OperationKind::Archived' "$$gate_tests/finalization_boundaries.rs"; \
	timeout 10s grep -Fq 'OperationKind::NewState' "$$gate_tests/finalization_boundaries.rs"; \
	timeout 10s grep -Fq 'wrong_candidate.candidate.id = None;' "$$gate_tests/finalization_authority_binding.rs"; \
	for file in "$$authority" "$$proof" "$$topology" "$$executor" "$$orchestrator" "$$startup_gate" "$$recovery_root" "$$reconciliation_root" "$$namespace_root" "$$gate_tests"/finalization_*.rs "$$gate_tests/support.rs" "$$executor_tests"/*.rs misc/make/startup-rollback-active-reblit-finalization-tests.mk misc/make/help.mk Makefile; do \
		timeout 10s test "$$( timeout 10s wc -l < "$$file" )" -le 1000; \
	done; \
	timeout 600s $(CARGO) test -p forge --lib active_reblit_finalization -- --test-threads=1
