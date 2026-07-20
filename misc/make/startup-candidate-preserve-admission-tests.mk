.PHONY: forge-startup-usr-rollback-candidate-preserve-admission-test

forge-startup-usr-rollback-candidate-preserve-admission-test:
	@set -eu; \
	listed="$$( timeout 300s $(CARGO) test -p forge --lib -- --list )"; \
	timeout 10s grep -q . <<<"$$listed"; \
	count="$$( timeout 10s grep -Ec '^client::startup_reconciliation::usr_rollback_candidate_preserve_authority::tests::(admission|evidence|post_move_durability|topology_refusal)::.*: test$$' <<<"$$listed" )"; \
	timeout 10s test "$$count" = 30; \
	for test in \
		client::startup_reconciliation::usr_rollback_candidate_preserve_authority::tests::admission::startup_candidate_preserve_admission_splits_every_exact_staged_and_preserved_matrix_case \
		client::startup_reconciliation::usr_rollback_candidate_preserve_authority::tests::admission::startup_candidate_preserve_admission_accepts_new_state_empty_quarantine_prefix \
		client::startup_reconciliation::usr_rollback_candidate_preserve_authority::tests::admission::startup_candidate_preserve_admission_accepts_historical_runtime_evidence \
		client::startup_reconciliation::usr_rollback_candidate_preserve_authority::tests::admission::startup_candidate_preserve_admission_bypasses_other_phases_and_sources \
		client::startup_reconciliation::usr_rollback_candidate_preserve_authority::tests::admission::startup_candidate_preserve_plan_requires_the_exact_operation_matrix \
		client::startup_reconciliation::usr_rollback_candidate_preserve_authority::tests::evidence::startup_candidate_preserve_rejects_a_different_open_journal_binding \
		client::startup_reconciliation::usr_rollback_candidate_preserve_authority::tests::evidence::startup_candidate_preserve_database_and_provenance_changes_invalidate_authority \
		client::startup_reconciliation::usr_rollback_candidate_preserve_authority::tests::evidence::startup_candidate_preserve_namespace_changes_invalidate_authority \
		client::startup_reconciliation::usr_rollback_candidate_preserve_authority::tests::evidence::startup_candidate_preserve_capture_races_defer_without_authority \
		client::startup_reconciliation::usr_rollback_candidate_preserve_authority::tests::evidence::startup_candidate_preserve_fresh_namespace_race_fails_revalidation \
		client::startup_reconciliation::usr_rollback_candidate_preserve_authority::tests::record_binding::startup_candidate_preserve_same_byte_predecessor_replacement_before_effect_never_authorizes_any_operation \
		client::startup_reconciliation::usr_rollback_candidate_preserve_authority::tests::record_binding::startup_candidate_preserve_same_byte_predecessor_replacement_after_physical_effect_never_becomes_success \
		client::startup_reconciliation::usr_rollback_candidate_preserve_authority::tests::record_binding::startup_candidate_preparation_restart_authority_rejects_same_bytes_at_a_successor_inode \
		client::startup_reconciliation::usr_rollback_candidate_preserve_authority::tests::post_move_durability::startup_new_state_post_move_durability_orders_exact_events_for_applied_and_finish_matrices \
		client::startup_reconciliation::usr_rollback_candidate_preserve_authority::tests::post_move_durability::startup_new_state_post_move_durability_faults_stop_at_exact_prefixes_and_fresh_admission_repeats \
		client::startup_reconciliation::usr_rollback_candidate_preserve_authority::tests::post_move_durability::startup_new_state_post_move_durability_rejects_exact_post_races_at_every_barrier \
		client::startup_reconciliation::usr_rollback_candidate_preserve_authority::tests::post_move_durability::startup_new_state_post_move_durability_rejects_evidence_races_and_fresh_admission_reruns \
		client::startup_reconciliation::usr_rollback_candidate_preserve_authority::tests::post_move_durability::startup_new_state_post_move_durability_converges_applied_error_after_apply_and_finish_origins \
		client::startup_reconciliation::usr_rollback_candidate_preserve_authority::tests::post_move_durability::startup_archived_finish_selects_its_separate_durability_authority_without_new_state_events \
		client::startup_reconciliation::usr_rollback_candidate_preserve_authority::tests::topology_refusal::startup_candidate_preserve_refuses_an_occupied_new_state_target \
		client::startup_reconciliation::usr_rollback_candidate_preserve_authority::tests::topology_refusal::startup_candidate_preserve_refuses_every_controlled_non_private_new_state_target_mode \
		client::startup_reconciliation::usr_rollback_candidate_preserve_authority::tests::topology_refusal::startup_candidate_preserve_models_every_restrictive_new_state_target_residue_without_mutation \
		client::startup_reconciliation::usr_rollback_candidate_preserve_authority::tests::topology_refusal::startup_candidate_preserve_keeps_payload_residue_distinct_from_empty_move_ready_evidence \
		client::startup_reconciliation::usr_rollback_candidate_preserve_authority::tests::topology_refusal::startup_candidate_preserve_refuses_unsafe_modes_and_wrong_target_types_without_mutation \
		client::startup_reconciliation::usr_rollback_candidate_preserve_authority::tests::topology_refusal::startup_candidate_preserve_residue_revalidation_rejects_name_inode_mode_and_content_changes \
		client::startup_reconciliation::usr_rollback_candidate_preserve_authority::tests::topology_refusal::startup_candidate_preserve_refuses_missing_wrong_extra_and_transferred_archived_slots \
		client::startup_reconciliation::usr_rollback_candidate_preserve_authority::tests::topology_refusal::startup_candidate_preserve_refuses_missing_duplicate_and_wrong_active_reblit_reservations \
		client::startup_reconciliation::usr_rollback_candidate_preserve_authority::tests::topology_refusal::startup_candidate_preserve_refuses_generic_quarantine_for_active_reblit \
		client::startup_reconciliation::usr_rollback_candidate_preserve_authority::tests::topology_refusal::startup_candidate_preserve_refuses_empty_and_foreign_current_state_wrappers \
		client::startup_reconciliation::usr_rollback_candidate_preserve_authority::tests::topology_refusal::startup_candidate_preserve_refuses_empty_transition_wrapper_for_archived_and_active_reblit \
		client::startup_reconciliation::usr_rollback_candidate_preserve_authority::tests::topology_refusal::startup_candidate_preserve_allows_fingerprint_bound_unrelated_state_wrappers \
		client::startup_reconciliation::usr_rollback_candidate_preserve_authority::tests::topology_refusal::startup_candidate_preserve_refuses_unmodeled_parking_for_new_and_archived_states \
		client::startup_reconciliation::usr_rollback_candidate_preserve_authority::tests::topology_refusal::startup_candidate_preserve_retains_a_nonzero_active_reblit_reservation_index; do \
		timeout 10s grep -Fqx "$$test: test" <<<"$$listed"; \
	done; \
	authority=crates/forge/src/client/startup_reconciliation/usr_rollback_candidate_preserve_authority.rs; \
	proof=crates/forge/src/client/startup_reconciliation/activation_namespace/candidate_preserve_proof.rs; \
	capture=crates/forge/src/client/startup_reconciliation/activation_namespace/capture/mod.rs; \
	model=crates/forge/src/client/startup_reconciliation/activation_namespace/capture/model.rs; \
	projection=crates/forge/src/client/startup_reconciliation/activation_namespace/capture/new_state_candidate_preserve.rs; \
	wrappers=crates/forge/src/client/startup_reconciliation/activation_namespace/capture/wrappers.rs; \
	startup_gate=crates/forge/src/client/startup_gate.rs; \
	new_state_dispatch=crates/forge/src/client/startup_gate/usr_rollback_new_state.rs; \
	archived_dispatch=crates/forge/src/client/startup_gate/usr_rollback_activate_archived.rs; \
	active_reblit_dispatch=crates/forge/src/client/startup_gate/usr_rollback_active_reblit.rs; \
	production_dispatch=crates/forge/src/client/startup_recovery/usr_rollback_candidate_preserve_dispatch.rs; \
	tests=crates/forge/src/client/startup_reconciliation/usr_rollback_candidate_preserve_authority/tests; \
	record_binding_tests="$$tests/record_binding.rs"; \
	effect_cases="$$( timeout 10s sed -n '/^    const ALL: \[Self; 5\] = \[/,/^    \];/p' "$$record_binding_tests" )"; \
	timeout 10s test "$$( timeout 10s grep -Fc '        Self::' <<<"$$effect_cases" )" = 5; \
	for case in CreateTarget NormalizeTarget MoveNewState MoveArchived ExchangeActiveReblit; do timeout 10s grep -Fqx "        Self::$$case," <<<"$$effect_cases"; done; \
	source_axis="$$( timeout 10s sed -n '/^    fn record_sources(self)/,/^    }/p' "$$record_binding_tests" )"; \
	timeout 10s test "$$( timeout 10s grep -Fc '        CandidateSource::ALL' <<<"$$source_axis" )" = 1; \
	timeout 10s grep -Fq '(self == Self::ExchangeActiveReblit).then_some(RecordSourceCase::BootSyncStarted)' <<<"$$source_axis"; \
	timeout 10s grep -Fq 'pub(super) fn active_reblit_boot_sync_started(' "$$tests/support.rs"; \
	timeout 10s grep -Fq 'Fixture::active_reblit_boot_sync_started(BootSyncStartedLayout::Post, historical)' "$$tests/support.rs"; \
	timeout 10s grep -Fq 'super::test_fixture::install_root_abi(&fixture.installation.root);' "$$tests/support.rs"; \
	pre_binding_matrix="$$( timeout 10s sed -n '/^fn startup_candidate_preserve_same_byte_predecessor_replacement_before_effect_never_authorizes_any_operation()/,/^#\[test\]/p' "$$record_binding_tests" | timeout 10s sed '$$d' )"; \
	post_binding_matrix="$$( timeout 10s sed -n '/^fn startup_candidate_preserve_same_byte_predecessor_replacement_after_physical_effect_never_becomes_success()/,/^#\[test\]/p' "$$record_binding_tests" | timeout 10s sed '$$d' )"; \
	restart_binding_matrix="$$( timeout 10s sed -n '/^fn startup_candidate_preparation_restart_authority_rejects_same_bytes_at_a_successor_inode()/,$$p' "$$record_binding_tests" )"; \
	for matrix in "$$pre_binding_matrix" "$$post_binding_matrix"; do \
		timeout 10s test "$$( timeout 10s grep -Fc '    for historical in [false, true] {' <<<"$$matrix" )" = 1; \
		timeout 10s test "$$( timeout 10s grep -Fc '        for outcome in [RollbackActionOutcome::Applied, RollbackActionOutcome::AlreadySatisfied] {' <<<"$$matrix" )" = 1; \
		timeout 10s test "$$( timeout 10s grep -Fc '            for case in EffectCase::ALL {' <<<"$$matrix" )" = 1; \
		timeout 10s test "$$( timeout 10s grep -Fc '                for source in case.record_sources() {' <<<"$$matrix" )" = 1; \
	done; \
	timeout 10s grep -Fq 'assert_eq!(cases, 44, "pre-effect record-binding matrix drifted");' <<<"$$pre_binding_matrix"; \
	timeout 10s grep -Fq 'assert_eq!(cases, 44, "post-effect record-binding matrix drifted");' <<<"$$post_binding_matrix"; \
	timeout 10s test "$$( timeout 10s grep -Fc '    for historical in [false, true] {' <<<"$$restart_binding_matrix" )" = 1; \
	timeout 10s test "$$( timeout 10s grep -Fc '        for source in CandidateSource::ALL {' <<<"$$restart_binding_matrix" )" = 1; \
	timeout 10s test "$$( timeout 10s grep -Fc '            for outcome in [RollbackActionOutcome::Applied, RollbackActionOutcome::AlreadySatisfied] {' <<<"$$restart_binding_matrix" )" = 1; \
	timeout 10s test "$$( timeout 10s grep -Fc '                for case in [EffectCase::CreateTarget, EffectCase::NormalizeTarget] {' <<<"$$restart_binding_matrix" )" = 1; \
	timeout 10s grep -Fq 'assert_eq!(cases, 16, "preparation restart matrix drifted");' <<<"$$restart_binding_matrix"; \
	timeout 10s test "$$( timeout 10s rg -l '^pub\(in crate::client\) struct UsrRollbackCandidatePreserveSeal \{' crates/forge/src/client --glob '*.rs' )" = "$$startup_gate"; \
	timeout 10s grep -Fqx 'pub(in crate::client) struct UsrRollbackCandidatePreserveSeal {' "$$startup_gate"; \
	timeout 10s awk '$$0 == "pub(in crate::client) struct UsrRollbackCandidatePreserveSeal {" { state = 1; next } state == 1 && $$0 == "    _private: ()," { field = 1; next } state == 1 && $$0 == "}" { found = field; exit !found } END { exit !found }' "$$startup_gate"; \
	seal_impl="$$( timeout 10s sed -n '/^impl UsrRollbackCandidatePreserveSeal {/,/^}/p' "$$startup_gate" )"; \
	timeout 10s test "$$( timeout 10s grep -Fc '    fn new() -> Self {' <<<"$$seal_impl" )" = 1; \
	timeout 10s test "$$( timeout 10s grep -Fc '    pub(in crate::client) fn new_for_test() -> Self {' <<<"$$seal_impl" )" = 1; \
	timeout 10s test "$$( timeout 10s grep -Fc 'UsrRollbackCandidatePreserveSeal::new();' "$$new_state_dispatch" )" = 1; \
	timeout 10s test "$$( timeout 10s grep -Fc 'UsrRollbackCandidatePreserveSeal::new();' "$$archived_dispatch" )" = 1; \
	timeout 10s test "$$( timeout 10s grep -Fc 'UsrRollbackCandidatePreserveSeal::new();' "$$active_reblit_dispatch" )" = 1; \
	timeout 10s test "$$( timeout 10s grep -Fc 'UsrRollbackCandidatePreserveAuthority::capture(' "$$new_state_dispatch" )" = 1; \
	timeout 10s test "$$( timeout 10s grep -Fc 'UsrRollbackCandidatePreserveAuthority::capture(' "$$archived_dispatch" )" = 1; \
	timeout 10s test "$$( timeout 10s grep -Fc 'UsrRollbackCandidatePreserveAuthority::capture(' "$$active_reblit_dispatch" )" = 1; \
	production_seal_calls="$$( timeout 10s rg -n -F 'UsrRollbackCandidatePreserveSeal::new();' crates/forge/src/client --glob '*.rs' --glob '!**/tests/**' --glob '!**/tests.rs' --glob '!**/*_tests.rs' --glob '!**/*_tests/**' )"; \
	timeout 10s test "$$( timeout 10s grep -c . <<<"$$production_seal_calls" )" = 3; \
	timeout 10s test "$$( timeout 10s grep -Fc "$$new_state_dispatch:" <<<"$$production_seal_calls" )" = 1; \
	timeout 10s test "$$( timeout 10s grep -Fc "$$archived_dispatch:" <<<"$$production_seal_calls" )" = 1; \
	timeout 10s test "$$( timeout 10s grep -Fc "$$active_reblit_dispatch:" <<<"$$production_seal_calls" )" = 1; \
	production_capture_calls="$$( timeout 10s rg -n -F 'UsrRollbackCandidatePreserveAuthority::capture(' crates/forge/src/client --glob '*.rs' --glob '!**/tests/**' --glob '!**/tests.rs' --glob '!**/*_tests.rs' --glob '!**/*_tests/**' )"; \
	timeout 10s test "$$( timeout 10s grep -c . <<<"$$production_capture_calls" )" = 3; \
	timeout 10s test "$$( timeout 10s grep -Fc "$$new_state_dispatch:" <<<"$$production_capture_calls" )" = 1; \
	timeout 10s test "$$( timeout 10s grep -Fc "$$archived_dispatch:" <<<"$$production_capture_calls" )" = 1; \
	timeout 10s test "$$( timeout 10s grep -Fc "$$active_reblit_dispatch:" <<<"$$production_capture_calls" )" = 1; \
	timeout 10s grep -Fqx '        _startup_gate_seal: &UsrRollbackCandidatePreserveSeal,' "$$authority"; \
	timeout 10s grep -Fqx '    journal_record_binding: TransitionJournalRecordBinding,' "$$authority"; \
	timeout 10s grep -Fqx "    _active_state_reservation: &'reservation ActiveStateReservation," "$$authority"; \
	restart_authority="$$( timeout 10s sed -n "/^pub(in crate::client) struct UsrRollbackCandidatePreserveRestartAuthority<'reservation> {/,/^}/p" "$$authority" )"; \
	timeout 10s grep -Fqx "pub(in crate::client) struct UsrRollbackCandidatePreserveRestartAuthority<'reservation> {" <<<"$$restart_authority"; \
	timeout 10s test "$$( timeout 10s grep -Ec '^    [a-zA-Z_][a-zA-Z0-9_]*: .+,$$' <<<"$$restart_authority" )" = 6; \
	for field in '    installation: Installation,' '    state_db: db::state::Database,' '    record: TransitionRecord,' '    database: DatabaseEvidence,' '    journal_record_binding: TransitionJournalRecordBinding,'; do timeout 10s grep -Fqx "$$field" <<<"$$restart_authority"; done; \
	timeout 10s grep -Fqx "    _active_state_reservation: &'reservation ActiveStateReservation," <<<"$$restart_authority"; \
	if timeout 10s rg -n '^    pub' <<<"$$restart_authority"; then exit 1; else status="$$?"; timeout 10s test "$$status" = 1; fi; \
	restart_declaration="$$( timeout 10s sed -n '/^\/\/\/ Consumed preparation-only authority/,/^}/p' "$$authority" )"; \
	if timeout 10s rg -n '#\[derive\([^]]*(Clone|Copy)' <<<"$$restart_declaration"; then exit 1; else status="$$?"; timeout 10s test "$$status" = 1; fi; \
	if timeout 10s rg -n 'impl (Clone|Copy) for UsrRollbackCandidatePreserveRestartAuthority' "$$authority"; then exit 1; else status="$$?"; timeout 10s test "$$status" = 1; fi; \
	restart_impl="$$( timeout 10s sed -n "/^impl UsrRollbackCandidatePreserveRestartAuthority<'_> {/,/^}/p" "$$authority" )"; \
	timeout 10s test "$$( timeout 10s grep -Fc '    pub(in crate::client) fn ' <<<"$$restart_impl" )" = 1; \
	timeout 10s grep -Fqx '    pub(in crate::client) fn into_exact_source_record(' <<<"$$restart_impl"; \
	timeout 10s test "$$( timeout 10s grep -Fc '        self,' <<<"$$restart_impl" )" = 1; \
	if timeout 10s rg -n '^[[:space:]]*&(mut[[:space:]]+)?self,|pub.*fn.*(retry|effect|lease|reconcile)' <<<"$$restart_impl"; then exit 1; else status="$$?"; timeout 10s test "$$status" = 1; fi; \
	timeout 10s test "$$( timeout 10s grep -Fc 'into_exact_source_record(' "$$authority" )" = 1; \
	restart_dispatch="$$( timeout 10s sed -n "/^fn return_exact_unchanged_source<'reservation>(/,/^}/p" "$$production_dispatch" )"; \
	timeout 10s grep -Fqx "    authority: UsrRollbackCandidatePreserveRestartAuthority<'reservation>," <<<"$$restart_dispatch"; \
	timeout 10s test "$$( timeout 10s grep -Fc '    let actual = authority.into_exact_source_record(&journal)?;' <<<"$$restart_dispatch" )" = 1; \
	timeout 10s test "$$( timeout 10s grep -Fc 'return return_exact_unchanged_source(journal, source_record, authority);' "$$production_dispatch" )" = 2; \
	timeout 10s test "$$( timeout 10s rg -n 'journal\.record_binding\(installation\.retained_mutable_cast_directory\(\)\?, record\)\?' "$$authority" | timeout 10s wc -l )" = 1; \
	timeout 10s test "$$( timeout 10s grep -Fc 'if !journal.has_record_store_binding(binding) {' "$$authority" )" = 1; \
	timeout 10s test "$$( timeout 10s grep -Fc 'journal.has_record_binding(cast, binding, record)' "$$authority" )" = 1; \
	timeout 10s awk '$$0 == "    fn revalidate_kind(" { active = 1; next } active && $$0 == "        self.require_journal_record_binding(journal)?;" { found = 1; exit } active && ($$0 ~ /self\.installation/ || $$0 ~ /inspect_current_database/ || $$0 ~ /self\.namespace/) { exit 1 } END { exit !found }' "$$authority"; \
	binding_line="$$( timeout 10s grep -nF 'journal.record_binding(installation.retained_mutable_cast_directory()?, record)?;' "$$authority" | timeout 10s cut -d: -f1 )"; \
	namespace_line="$$( timeout 10s grep -nF 'match UsrRollbackCandidatePreserveNamespaceInspection::begin(' "$$authority" | timeout 10s cut -d: -f1 )"; \
	database_line="$$( timeout 10s grep -nF '        let database = inspect_database(record, state_db, initial_in_flight)?;' "$$authority" | timeout 10s cut -d: -f1 )"; \
	timeout 10s grep -Fqx '        installation.revalidate_mutable_namespace()?;' <<<"$$( timeout 10s sed -n "$$((binding_line - 2))p" "$$authority" )"; \
	timeout 10s grep -Fqx '        installation.revalidate_mutable_namespace()?;' <<<"$$( timeout 10s sed -n "$$((binding_line + 1))p" "$$authority" )"; \
	timeout 10s test "$$binding_line" -lt "$$namespace_line"; \
	timeout 10s test "$$binding_line" -lt "$$database_line"; \
	if timeout 10s rg -n 'TransitionJournalBinding|journal\.binding\(\)|journal\.has_binding\(|journal\.load\(\)|journal\.advance\(' "$$authority" "$$proof"; then exit 1; else status="$$?"; timeout 10s test "$$status" = 1; fi; \
	timeout 10s grep -Fq 'if record.phase != Phase::CandidatePreserveIntent {' "$$authority"; \
	timeout 10s grep -Fq 'ForwardPhase::UsrExchangeIntent | ForwardPhase::UsrExchanged' "$$authority"; \
	timeout 10s grep -Fq 'RollbackAction::Applied | RollbackAction::AlreadySatisfied' "$$authority"; \
	timeout 10s grep -Fq 'Operation::ActivateArchived => rollback.candidate.disposition == AbortDisposition::Rearchive' "$$authority"; \
	timeout 10s grep -Fq 'UsrRollbackCandidatePreserveAdmission::Apply' "$$authority"; \
	timeout 10s grep -Fq 'UsrRollbackCandidatePreserveAdmission::Finish' "$$authority"; \
	timeout 10s grep -Fq 'NewStateStagedWithEmptyQuarantine' "$$proof"; \
	timeout 10s grep -Fq 'NewStateStagedWithTargetResidue' "$$proof"; \
	timeout 10s grep -Fq 'ArchivedStagedWithCanonicalSlot' "$$proof"; \
	timeout 10s grep -Fq 'ActiveReblitStaged { wrapper_index: usize }' "$$proof"; \
	timeout 10s grep -Fq 'candidate.marker_links() != 2' "$$proof"; \
	timeout 10s grep -Fq 'UnexpectedParkingWrapper' "$$proof"; \
	timeout 10s grep -Fq 'UnexpectedCurrentStateWrapper' "$$proof"; \
	timeout 10s grep -Fq 'self.witness.mode & 0o7777 == 0o700' "$$model"; \
	timeout 10s grep -Fq 'wrapper.has_exact_private_permissions()' "$$proof"; \
	timeout 10s grep -Fq 'if !target.has_exact_private_permissions() {' "$$projection"; \
	timeout 10s grep -Fq 'TargetPermissions' "$$projection"; \
	timeout 10s grep -Fq 'permissions != 0o700' "$$capture"; \
	timeout 10s grep -Fq 'permissions & !0o700 == 0' "$$capture"; \
	timeout 10s grep -Fq 'struct NewStateTargetResidueFingerprint {' "$$model"; \
	timeout 10s grep -Fq 'struct RetainedNewStateTargetResidue {' "$$model"; \
	timeout 10s grep -Fq 'new_state_target_residue: Option<NewStateTargetResidueFingerprint>' "$$model"; \
	timeout 10s grep -Fq 'new_state_target_residue: Option<RetainedNewStateTargetResidue>' "$$model"; \
	timeout 10s grep -Fqx '    pub(in crate::client::startup_reconciliation::activation_namespace) fn has_new_state_target_residue(&self) -> bool {' "$$model"; \
	timeout 10s test "$$( timeout 10s rg -n 'pub\([^)]*\)[[:space:]]+fn[[:space:]]+.*target_residue' "$$model" | timeout 10s wc -l )" = 1; \
	timeout 10s grep -Fq 'new_state_target_residue = Some(RetainedNewStateTargetResidue {' "$$wrappers"; \
	timeout 10s awk '$$0 == "    if let Some(residue) = new_state_target_residue {" { blocks++; active = blocks == 2; next } active && /require_new_state_target_residue_witness\(/ { witness = 1 } active && /revalidate_named_entry\(/ { named = 1 } active && /require_witness\(/ { exact = 1 } active && $$0 == "    }" { found = witness && named && exact; exit !found } END { exit !found }' "$$model"; \
	timeout 10s grep -Fqx 'pub(in crate::client::startup_reconciliation) enum UsrRollbackCandidatePreserveTopology {' "$$proof"; \
	timeout 10s test "$$( timeout 10s rg -n 'pub\(in crate::client::startup_reconciliation\) fn topology\(' "$$authority" | timeout 10s wc -l )" = 2; \
	timeout 10s awk '$$0 == "    #[cfg(test)]" { gated = 1; next } gated && $$0 == "    pub(in crate::client::startup_reconciliation) fn topology(&self) -> UsrRollbackCandidatePreserveTopology {" { count++; gated = 0; next } gated { gated = 0 } END { exit count != 2 }' "$$authority"; \
	if timeout 10s rg -n 'pub\(in crate::client\) (enum|use).*UsrRollbackCandidatePreserveTopology|pub\(in crate::client\) fn topology\(' "$$authority" "$$proof" crates/forge/src/client/startup_reconciliation.rs crates/forge/src/client/startup_reconciliation/activation_namespace.rs; then exit 1; fi; \
	timeout 10s grep -Fq 'startup_candidate_preserve_refuses_missing_wrong_extra_and_transferred_archived_slots' "$$tests/topology_refusal.rs"; \
	timeout 10s grep -Fq 'startup_candidate_preserve_refuses_missing_duplicate_and_wrong_active_reblit_reservations' "$$tests/topology_refusal.rs"; \
	timeout 10s grep -Fq 'startup_candidate_preserve_refuses_empty_and_foreign_current_state_wrappers' "$$tests/topology_refusal.rs"; \
	timeout 10s grep -Fq 'startup_candidate_preserve_refuses_empty_transition_wrapper_for_archived_and_active_reblit' "$$tests/topology_refusal.rs"; \
	timeout 10s grep -Fq 'startup_candidate_preserve_refuses_unmodeled_parking_for_new_and_archived_states' "$$tests/topology_refusal.rs"; \
	timeout 10s grep -Fq 'startup_candidate_preserve_refuses_every_controlled_non_private_new_state_target_mode' "$$tests/topology_refusal.rs"; \
	timeout 10s grep -Fq 'startup_candidate_preserve_models_every_restrictive_new_state_target_residue_without_mutation' "$$tests/topology_refusal.rs"; \
	timeout 10s grep -Fq 'startup_candidate_preserve_keeps_payload_residue_distinct_from_empty_move_ready_evidence' "$$tests/topology_refusal.rs"; \
	timeout 10s grep -Fq 'startup_candidate_preserve_refuses_unsafe_modes_and_wrong_target_types_without_mutation' "$$tests/topology_refusal.rs"; \
	timeout 10s grep -Fq 'startup_candidate_preserve_residue_revalidation_rejects_name_inode_mode_and_content_changes' "$$tests/topology_refusal.rs"; \
	if timeout 10s rg -n 'dispatcher|dispatch_' "$$authority" "$$proof"; then exit 1; fi; \
	if timeout 10s rg -n 'renameat|rename\(|exchange_forward|exchange_reverse|sync_all|sync_data|\.sync\(|\.advance\(|forward_successor|rollback_successor|unlinkat|linkat|create_dir|remove_dir|remove_file|set_permissions|write_all|run_transaction_triggers|run_system_triggers|root_links|archive_previous|rearchive_archived|preserve_failed|remove_exact_archived|add_with_transition|insert_fresh_metadata|delete_metadata_provenance|clear_transition_if_matches|remove_transition_if_matches|\.add\(|\.remove\(|\.batch_remove\(|\.execute\(|\.transaction\(|\.delete\(' "$$authority" "$$proof"; then exit 1; fi; \
	if timeout 10s rg -n 'std::fs::File|fs::File|AsRawFd|RawFd|BorrowedFd|OwnedFd|root_directory\(|retained_staging_parent|PendingSystemTransition|ActivationNamespaceEvidence' "$$authority" "$$proof"; then exit 1; fi; \
	if timeout 10s rg -n 'renameat|rename\(|mkdirat|create_dir|set_permissions|chmod|remove_dir|remove_file|write_all|sync_all|sync_data|\.sync\(|\.advance\(|clear_transition_if_matches|remove_transition_if_matches|insert_fresh_metadata|delete_metadata' "$$capture" "$$model" "$$wrappers" "$$proof"; then exit 1; fi; \
	for file in "$$authority" "$$proof" "$$capture" "$$model" "$$projection" "$$wrappers" "$$startup_gate" "$$new_state_dispatch" "$$archived_dispatch" "$$active_reblit_dispatch" "$$tests.rs" "$$tests/support.rs" "$$tests/admission.rs" "$$tests/evidence.rs" "$$tests/post_move_durability.rs" "$$tests/record_binding.rs" "$$tests/topology_refusal.rs"; do \
		timeout 10s test "$$( timeout 10s wc -l < "$$file" )" -le 1000; \
	done; \
	timeout 1200s $(CARGO) test -p forge --lib \
		'client::startup_reconciliation::usr_rollback_candidate_preserve_authority::tests::' \
		-- --test-threads=1
