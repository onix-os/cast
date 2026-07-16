forge-transition-journal-coordinator-test:
	@set -eu; \
	listed="$$( timeout 300s $(CARGO) test -p forge --lib -- --list )"; \
	timeout 10s test -n "$$listed"; \
	count="$$( timeout 10s grep -c '^transition_identity::journal_coordinator::tests::journal_coordinator_.*: test$$' <<<"$$listed" )"; \
	timeout 10s test "$$count" = 76; \
	for test in \
		transition_identity::journal_coordinator::tests::journal_coordinator_new_state_reaches_candidate_prepared_through_exact_generations \
		transition_identity::journal_coordinator::tests::journal_coordinator_new_state_previous_origins_and_options_are_exact \
		transition_identity::journal_coordinator::tests::journal_coordinator_archived_activation_reaches_candidate_prepared_without_allocation_phases \
		transition_identity::journal_coordinator::tests::journal_coordinator_active_reblit_reaches_candidate_prepared_without_allocation_phases \
		transition_identity::journal_coordinator::tests::journal_coordinator_creation_captures_exact_epoch_tokens_and_runtime_tree_witnesses \
		transition_identity::journal_coordinator::tests::journal_coordinator_quarantine_name_is_fixed_transition_token_evidence \
		transition_identity::journal_coordinator::tests::journal_coordinator_candidate_state_authority_cannot_be_reinterpreted_between_operations \
		transition_identity::journal_coordinator::tests::journal_coordinator_active_reblit_prejournal_authority_preserves_residue_and_name_substitution \
		transition_identity::journal_coordinator::tests::journal_coordinator_wrong_operation_or_phase_is_rejected_without_record_change \
		transition_identity::journal_coordinator::tests::journal_coordinator_fresh_allocation_effect_observes_durable_intent_before_database_commit \
		transition_identity::journal_coordinator::tests::journal_coordinator_allocation_finish_rejects_missing_cleared_foreign_and_wrong_state_evidence \
		transition_identity::journal_coordinator::tests::journal_coordinator_database_commit_and_completion_share_exact_transition_correlation \
		transition_identity::journal_coordinator::tests::journal_coordinator_post_commit_journal_failure_preserves_matching_database_evidence \
		transition_identity::journal_coordinator::tests::journal_coordinator_candidate_prepare_effect_order_and_failure_preserve_exact_evidence \
		transition_identity::journal_coordinator::tests::journal_coordinator_active_reblit_state_id_publication_failures_preserve_started_evidence \
		transition_identity::journal_coordinator::tests::journal_coordinator_active_reblit_state_id_appearance_before_prepare_intent_blocks_advance \
		transition_identity::journal_coordinator::tests::journal_coordinator_existing_candidate_database_removal_blocks_journal_creation \
		transition_identity::journal_coordinator::tests::journal_coordinator_distinct_previous_database_removal_blocks_journal_creation \
		transition_identity::journal_coordinator::tests::journal_coordinator_transaction_triggers_complete_exact_new_state_and_active_reblit_generations \
		transition_identity::journal_coordinator::tests::journal_coordinator_archived_transaction_triggers_are_rejected_without_effect \
		transition_identity::journal_coordinator::tests::journal_coordinator_transaction_trigger_effect_error_runs_once_and_preserves_started \
		transition_identity::journal_coordinator::tests::journal_coordinator_transaction_trigger_intent_faults_leave_old_or_successor_without_effect \
		transition_identity::journal_coordinator::tests::journal_coordinator_transaction_trigger_completion_faults_leave_started_or_complete_after_one_effect \
		transition_identity::journal_coordinator::tests::journal_coordinator_transaction_trigger_preflight_failure_runs_no_effect \
		transition_identity::journal_coordinator::tests::journal_coordinator_transaction_trigger_post_effect_failure_preserves_started \
		transition_identity::journal_coordinator::tests::journal_coordinator_transaction_trigger_post_effect_database_changes_are_blocked \
		transition_identity::journal_coordinator::tests::journal_coordinator_transaction_trigger_post_effect_previous_database_removal_is_blocked \
		transition_identity::journal_coordinator::tests::journal_coordinator_transaction_trigger_global_database_audit_blocks_foreign_rows \
		transition_identity::journal_coordinator::tests::journal_coordinator_transaction_trigger_state_id_and_public_name_substitution_are_blocked \
		transition_identity::journal_coordinator::tests::journal_coordinator_transaction_trigger_failure_releases_journal_while_error_lives \
		transition_identity::journal_coordinator::tests::journal_coordinator_metadata_proof_is_owned_for_every_operation_and_uses_exact_os_info \
		transition_identity::journal_coordinator::tests::journal_coordinator_archived_metadata_proof_rejects_independent_expectation_mismatch_without_mutation \
		transition_identity::journal_coordinator::tests::journal_coordinator_candidate_prepare_rejects_same_byte_foreign_candidate_before_metadata_or_state_id \
		transition_identity::journal_coordinator::tests::journal_coordinator_metadata_substitution_before_trigger_intent_runs_no_effect \
		transition_identity::journal_coordinator::tests::journal_coordinator_metadata_substitution_during_trigger_effect_stops_before_completion \
		transition_identity::journal_coordinator::tests::journal_coordinator_metadata_publication_failure_releases_authorities_while_error_lives \
		transition_identity::journal_coordinator::tests::journal_coordinator_new_state_provenance_commit_faults_precede_every_canonical_output \
		transition_identity::journal_coordinator::tests::journal_coordinator_first_and_second_metadata_publication_faults_retain_provenance \
		transition_identity::journal_coordinator::tests::journal_coordinator_candidate_prepared_journal_faults_retain_complete_provenance_evidence \
		transition_identity::journal_coordinator::tests::journal_coordinator_existing_candidates_require_exact_nonlegacy_provenance_before_publication \
		transition_identity::journal_coordinator::tests::journal_coordinator_archived_verification_sandwich_detects_provenance_removal_without_mutation \
		transition_identity::journal_coordinator::tests::journal_coordinator_provenance_is_revalidated_before_trigger_and_exchange_intents \
		transition_identity::journal_coordinator::tests::journal_coordinator_usr_exchange_intent_has_exact_phase_and_generation_for_every_operation \
		transition_identity::journal_coordinator::tests::journal_coordinator_usr_exchange_intent_performs_no_exchange_or_root_link_publication \
		transition_identity::journal_coordinator::tests::journal_coordinator_usr_exchange_intent_revalidates_all_retained_evidence_before_advance \
		transition_identity::journal_coordinator::tests::journal_coordinator_usr_exchange_intent_reseals_candidate_before_advance \
		transition_identity::journal_coordinator::tests::journal_coordinator_usr_exchange_intent_faults_leave_exact_predecessor_or_intent \
		transition_identity::journal_coordinator::tests::journal_coordinator_usr_exchange_intent_failure_releases_journal_while_error_lives \
		transition_identity::journal_coordinator::tests::journal_coordinator_usr_exchange_effect_applies_once_for_every_operation_without_root_links \
		transition_identity::journal_coordinator::tests::journal_coordinator_active_reblit_exchange_preserves_exact_parked_two_link_slot_and_reservation \
		transition_identity::journal_coordinator::tests::journal_coordinator_active_reblit_slot_and_state_substitution_stop_before_exchange \
		transition_identity::journal_coordinator::tests::journal_coordinator_usr_exchange_effect_raw_result_matrix_never_retries \
		transition_identity::journal_coordinator::tests::journal_coordinator_usr_exchange_effect_durability_faults_are_applied_without_reverse_or_retry \
		transition_identity::journal_coordinator::tests::journal_coordinator_usr_exchange_effect_reconciles_foreign_post_syscall_layout_as_ambiguous \
		transition_identity::journal_coordinator::tests::journal_coordinator_usr_exchange_effect_repeats_full_proof_immediately_before_syscall \
		transition_identity::journal_coordinator::tests::journal_coordinator_usr_exchange_effect_post_apply_metadata_substitution_is_fail_stop \
		transition_identity::journal_coordinator::tests::journal_coordinator_usr_exchange_effect_post_apply_authority_failures_remain_at_intent \
		transition_identity::journal_coordinator::tests::journal_coordinator_usr_exchange_effect_completion_faults_leave_intent_or_exchanged_after_one_call \
		transition_identity::journal_coordinator::tests::journal_coordinator_usr_exchange_authority_is_writer_first_and_never_waits_behind_journal \
		transition_identity::journal_coordinator::tests::journal_coordinator_new_state_synthesized_empty_exchange_applies_once_and_retains_empty_previous \
		transition_identity::journal_coordinator::tests::journal_coordinator_usr_exchange_identity_handoff_fails_bounded_when_contender_wins_journal_gap \
		transition_identity::journal_coordinator::tests::journal_coordinator_active_reblit_reservation_keeps_wrong_wrapper_mode_untouched \
		transition_identity::journal_coordinator::tests::journal_coordinator_active_reblit_reservation_preserves_typed_coordinator_evidence \
		transition_identity::journal_coordinator::tests::journal_coordinator_active_reblit_reservation_handles_one_link_and_parks_two_link_previous \
		transition_identity::journal_coordinator::tests::journal_coordinator_active_reblit_reservation_reports_ambiguous_replacement_stage \
		transition_identity::journal_coordinator::tests::journal_coordinator_active_reblit_reservation_reports_durable_final_checkpoint_failure \
		transition_identity::journal_coordinator::tests::journal_coordinator_active_reblit_reservation_retries_one_durability_unproven_fault \
		transition_identity::journal_coordinator::tests::journal_coordinator_active_reblit_reservation_reports_applied_slot_after_durable_replacement \
		transition_identity::journal_coordinator::tests::journal_coordinator_active_reblit_reservation_preserves_foreign_name_exhaustion \
		transition_identity::journal_coordinator::tests::journal_coordinator_active_reblit_tamper_before_started_runs_no_effect \
		transition_identity::journal_coordinator::tests::journal_coordinator_active_reblit_parked_slot_substitution_before_started_runs_no_effect \
		transition_identity::journal_coordinator::tests::journal_coordinator_active_reblit_full_state_snapshot_change_before_started_runs_no_effect \
		transition_identity::journal_coordinator::tests::journal_coordinator_active_reblit_tamper_after_started_stops_before_callback \
		transition_identity::journal_coordinator::tests::journal_coordinator_active_reblit_tamper_during_effect_preserves_started \
		transition_identity::journal_coordinator::tests::journal_coordinator_active_reblit_tamper_before_exchange_intent_preserves_trigger_complete \
		transition_identity::journal_coordinator::tests::journal_coordinator_active_reblit_reservation_survives_intent_and_exchange_direction_flip; do \
		timeout 10s grep -Fqx "$$test: test" <<<"$$listed"; \
	done; \
	trigger_contract="crates/forge/src/transition_identity/journal_coordinator/transaction_triggers.rs"; \
	usr_exchange_contract="crates/forge/src/transition_identity/journal_coordinator/usr_exchange_intent.rs"; \
	usr_exchange_effect="crates/forge/src/transition_identity/journal_coordinator/usr_exchange_effect.rs"; \
	usr_exchange_authority="crates/forge/src/client/journal_usr_exchange_authority.rs"; \
	prepare_contract="crates/forge/src/transition_identity/journal_coordinator/candidate_preparation.rs"; \
	coordinator_contract="crates/forge/src/transition_identity/journal_coordinator/mod.rs"; \
	authority_contract="crates/forge/src/transition_identity/candidate_state_authority.rs"; \
	tree_lifecycle="crates/forge/src/transition_identity/tree_lifecycle.rs"; \
	timeout 10s grep -Fqx 'mod candidate_state_authority;' crates/forge/src/transition_identity.rs; \
	if timeout 10s grep -Fqx 'pub(crate) mod candidate_state_authority;' crates/forge/src/transition_identity.rs; then \
		timeout 10s printf '%s\n' 'candidate state authority module visibility widened' >&2; exit 1; \
	else \
		status="$$?"; timeout 10s test "$$status" = 1; \
	fi; \
	for variant in '    UnknownIdAbsent,' '    KnownIdAbsent(state::Id),' '    ExistingId(state_tree_metadata::RetainedTreeStateId),'; do \
		timeout 10s grep -Fqx "$$variant" "$$authority_contract"; \
	done; \
	timeout 10s grep -Fq 'pub(crate) fn prepare_active_reblit_candidate(' "$$tree_lifecycle"; \
	timeout 10s grep -Fq 'pub(crate) fn prepare_retained_active_reblit_candidate(' "$$tree_lifecycle"; \
	if timeout 10s grep -nF 'candidate_state: Option<state::Id>' "$$tree_lifecycle"; then \
		timeout 10s printf '%s\n' 'candidate preparation collapsed three-way state authority back into Option' >&2; exit 1; \
	else \
		status="$$?"; timeout 10s test "$$status" = 1; \
	fi; \
	if timeout 10s grep -nF 'match parts.candidate_id' "$$coordinator_contract"; then \
		timeout 10s printf '%s\n' 'coordinator again treats logical candidate ID presence as filesystem publication' >&2; exit 1; \
	else \
		status="$$?"; timeout 10s test "$$status" = 1; \
	fi; \
	if timeout 10s grep -nF 'self.record.operation != Operation::NewState' "$$prepare_contract"; then \
		timeout 10s printf '%s\n' 'candidate proof again conflates ActiveReblit with existing archived state ID' >&2; exit 1; \
	else \
		status="$$?"; timeout 10s test "$$status" = 1; \
	fi; \
	if timeout 10s grep -RInF 'publish_new' crates/forge/src/transition_identity; then \
		timeout 10s printf '%s\n' 'state-ID publisher regained NewState-only semantics' >&2; exit 1; \
	else \
		status="$$?"; timeout 10s test "$$status" = 1; \
	fi; \
	timeout 10s grep -Fqx '            Operation::NewState | Operation::ActiveReblit => {' "$$prepare_contract"; \
	timeout 10s grep -Fq 'RetainedTreeStateId::publish_absent' "$$prepare_contract"; \
	if timeout 10s grep -RInE 'prepare_(retained_)?active_reblit_candidate' crates/forge/src/client; then \
		timeout 10s printf '%s\n' 'known-ID/absent candidate authority gained a live callsite before startup recovery exists' >&2; exit 1; \
	else \
		status="$$?"; timeout 10s test "$$status" = 1; \
	fi; \
	timeout 10s grep -Fqx "pub(super) struct StatefulTransactionTriggerAuthority<'authority> {" "$$trigger_contract"; \
	timeout 10s grep -Fqx 'pub(super) enum StatefulTransactionTriggerFailure<E>' "$$trigger_contract"; \
	timeout 10s grep -Fqx 'pub(crate) enum PreparedStatefulTransitionCoordinator {' "$$prepare_contract"; \
	for variant in \
		'    NewStateTriggers(PreparedTransactionTriggerCoordinator),' \
		'    ActiveReblitReservation(PreparedActiveReblitReservationCoordinator),' \
		'    Archived(PreparedArchivedTransitionCoordinator),'; do \
		timeout 10s grep -Fqx "$$variant" "$$prepare_contract"; \
	done; \
	timeout 10s grep -Fqx 'pub(crate) struct PreparedActiveReblitReservationCoordinator {' "$$prepare_contract"; \
	if timeout 10s grep -nF 'PreparedActiveReblitReservationCoordinator' "$$trigger_contract"; then \
		timeout 10s printf '%s\n' 'non-ready ActiveReblit authority acquired a trigger runner' >&2; exit 1; \
	else \
		status="$$?"; timeout 10s test "$$status" = 1; \
	fi; \
	timeout 10s grep -Fqx 'pub(crate) struct PreparedTransactionTriggerCoordinator {' "$$prepare_contract"; \
	timeout 10s grep -Fqx 'pub(crate) struct PreparedArchivedTransitionCoordinator {' "$$prepare_contract"; \
	timeout 10s awk '/PreparedTransactionTriggerCoordinator \{$$/ && $$0 !~ /(struct|impl) PreparedTransactionTriggerCoordinator/ { count++ } END { exit count == 2 ? 0 : 1 }' \
		crates/forge/src/transition_identity/journal_coordinator/*.rs; \
	timeout 10s test "$$( timeout 10s grep -RFl 'ActiveReblitReservationSeal { _private: () }' \
		crates/forge/src/transition_identity/journal_coordinator --include='*.rs' )" = \
		'crates/forge/src/transition_identity/journal_coordinator/active_reblit_reservation.rs'; \
	timeout 10s test "$$( timeout 10s grep -Fc 'ActiveReblitReservationSeal { _private: () }' \
		crates/forge/src/transition_identity/journal_coordinator/active_reblit_reservation.rs )" = 3; \
	timeout 10s test "$$( timeout 10s grep -Fc '    pub(super) metadata: CandidateMetadataProof,' "$$prepare_contract" )" = 4; \
	timeout 10s test "$$( timeout 10s grep -Fc '    pub(super) provenance: db::state::MetadataProvenance,' "$$prepare_contract" )" = 4; \
	timeout 10s test "$$( timeout 10s grep -Fc '    pub(super) readiness: TransactionTriggerReadiness,' "$$prepare_contract" )" = 2; \
	timeout 10s grep -Fqx '    pub(super) readiness: UsrExchangeReadiness,' "$$usr_exchange_contract"; \
	timeout 10s grep -Fqx '    readiness: UsrExchangeReadiness,' "$$usr_exchange_effect"; \
	timeout 10s grep -Fq 'UsrExchangeReadiness::TransactionTriggers(readiness)' "$$usr_exchange_contract"; \
	timeout 10s test "$$( timeout 10s grep -Fc 'readiness.require_staged(&coordinator.identity)' "$$trigger_contract" )" = 2; \
	timeout 10s test "$$( timeout 10s grep -Fc 'readiness.require_staged(&coordinator.identity)' "$$usr_exchange_contract" )" = 2; \
	timeout 10s test "$$( timeout 10s grep -Fc 'readiness.require_live(&coordinator.identity)' "$$usr_exchange_effect" )" = 2; \
	timeout 10s grep -Fqx 'impl PreparedTransactionTriggerCoordinator {' "$$trigger_contract"; \
	timeout 10s grep -Fqx '    pub(super) fn run_transaction_triggers<E, F>(' "$$trigger_contract"; \
	if timeout 10s grep -nF 'Option<CandidateMetadataProof>' "$$prepare_contract" "$$trigger_contract"; then \
		timeout 10s printf '%s\n' 'proof-bearing coordinator authority became optional' >&2; exit 1; \
	else \
		status="$$?"; timeout 10s test "$$status" = 1; \
	fi; \
	runner_signature="$$( timeout 10s sed -n '/pub(super) fn run_transaction_triggers/,/) -> Result/p' "$$trigger_contract" )"; \
	if timeout 10s grep -Fq 'CandidateMetadataProof' <<<"$$runner_signature"; then \
		timeout 10s printf '%s\n' 'transaction-trigger runner accepts a caller-supplied metadata proof' >&2; exit 1; \
	fi; \
	if timeout 10s grep -nF 'PreparedArchivedTransitionCoordinator' "$$trigger_contract"; then \
		timeout 10s printf '%s\n' 'archived activation acquired transaction-trigger authority' >&2; exit 1; \
	else \
		status="$$?"; timeout 10s test "$$status" = 1; \
	fi; \
	if widened="$$( timeout 10s grep -nE \
		'pub\(crate\).*(StatefulTransactionTriggerAuthority|StatefulTransactionTriggerFailure|run_transaction_triggers)' \
		crates/forge/src/transition_identity/journal_coordinator/mod.rs "$$prepare_contract" "$$trigger_contract" )"; then \
		timeout 10s printf '%s\n' 'unwired transaction-trigger authority was widened before metadata-aware live integration:' "$$widened" >&2; \
		exit 1; \
	else \
		status=$$?; \
		timeout 10s test "$$status" = 1; \
	fi; \
	timeout 10s grep -Fqx 'mod usr_exchange_intent;' "$$coordinator_contract"; \
	timeout 10s grep -Fqx 'pub(crate) struct UsrExchangeIntentCoordinator {' "$$usr_exchange_contract"; \
	timeout 10s grep -Fqx 'pub(super) enum UsrExchangeIntentFailure {' "$$usr_exchange_contract"; \
	timeout 10s test "$$( timeout 10s grep -Fc '    pub(super) fn begin_usr_exchange_intent(' "$$usr_exchange_contract" )" = 2; \
	timeout 10s grep -Fqx '    pub(super) coordinator: StatefulTransitionCoordinator,' "$$usr_exchange_contract"; \
	timeout 10s grep -Fqx '    pub(super) metadata: CandidateMetadataProof,' "$$usr_exchange_contract"; \
	timeout 10s grep -Fqx '    pub(super) provenance: db::state::MetadataProvenance,' "$$usr_exchange_contract"; \
	if timeout 10s grep -nF 'Option<CandidateMetadataProof>' "$$usr_exchange_contract"; then \
		timeout 10s printf '%s\n' '/usr exchange-intent authority made its metadata proof optional' >&2; exit 1; \
	else \
		status="$$?"; timeout 10s test "$$status" = 1; \
	fi; \
	if timeout 10s grep -nE 'renameat2|exchange_forward|create_root_links|symlinkat|unlinkat' "$$usr_exchange_contract"; then \
		timeout 10s printf '%s\n' 'intent-only /usr exchange boundary acquired a namespace mutation primitive' >&2; exit 1; \
	else \
		status="$$?"; timeout 10s test "$$status" = 1; \
	fi; \
	if timeout 10s grep -nF 'pub(crate) fn begin_usr_exchange_intent' "$$usr_exchange_contract"; then \
		timeout 10s printf '%s\n' '/usr exchange-intent transition widened before live recovery exists' >&2; exit 1; \
	else \
		status="$$?"; timeout 10s test "$$status" = 1; \
	fi; \
	timeout 10s grep -Fqx 'mod usr_exchange_effect;' "$$coordinator_contract"; \
	timeout 10s grep -Fqx 'pub(crate) struct UsrExchangedCoordinator {' "$$usr_exchange_effect"; \
	timeout 10s grep -Fqx '    coordinator: StatefulTransitionCoordinator,' "$$usr_exchange_effect"; \
	timeout 10s grep -Fqx '    metadata: CandidateMetadataProof,' "$$usr_exchange_effect"; \
	timeout 10s grep -Fqx '    provenance: db::state::MetadataProvenance,' "$$usr_exchange_effect"; \
	timeout 10s grep -Fqx '    authority: AppliedJournalUsrExchangeAuthority,' "$$usr_exchange_effect"; \
	timeout 10s grep -Fqx '    pub(super) fn execute_usr_exchange(' "$$usr_exchange_effect"; \
	timeout 10s test "$$( timeout 10s grep -Fc '.exchange_forward_with_journal(installation, &seal, &|| {' "$$usr_exchange_effect" )" = 1; \
	timeout 10s test "$$( timeout 10s grep -Fc 'renameat2_exchange_once(' "$$tree_lifecycle" )" = 1; \
	timeout 10s test "$$( timeout 10s grep -Fc 'ExchangeJournalGuard::LegacyNoJournal' "$$tree_lifecycle" )" -ge 4; \
	if timeout 10s grep -nE 'Option<(CandidateMetadataProof|db::state::MetadataProvenance|AppliedJournalUsrExchangeAuthority|RootAbiPreflight)>' \
		"$$usr_exchange_effect" "$$usr_exchange_authority"; then \
		timeout 10s printf '%s\n' '/usr exchange completion made mandatory proof authority optional' >&2; exit 1; \
	else \
		status="$$?"; timeout 10s test "$$status" = 1; \
	fi; \
	if timeout 10s grep -nE 'renameat2|exchange_reverse|create_root_links|symlinkat|unlinkat|remove_(dir|file)' "$$usr_exchange_effect"; then \
		timeout 10s printf '%s\n' 'coordinator exchange effect acquired retry, reverse, cleanup, or root-link primitives' >&2; exit 1; \
	else \
		status="$$?"; timeout 10s test "$$status" = 1; \
	fi; \
	effect_failure="$$( timeout 10s sed -n '/enum UsrExchangeEffectFailure/,/^}/p' "$$usr_exchange_effect" )"; \
	if timeout 10s grep -E '^[[:space:]]*(coordinator|authority|metadata|provenance):' <<<"$$effect_failure"; then \
		timeout 10s printf '%s\n' '/usr exchange failure retained a reusable coordinator or authority' >&2; exit 1; \
	fi; \
	timeout 10s grep -Fqx 'pub(crate) struct JournalUsrExchangeAuthorityPreflight {' "$$usr_exchange_authority"; \
	timeout 10s grep -Fqx 'pub(crate) struct JournalUsrExchangePreparationSeal {' "$$usr_exchange_authority"; \
	timeout 10s grep -Fqx 'pub(crate) struct JournalUsrExchangeAuthority {' "$$usr_exchange_authority"; \
	timeout 10s grep -Fqx 'pub(crate) struct AppliedJournalUsrExchangeAuthority {' "$$usr_exchange_authority"; \
	timeout 10s test "$$( timeout 10s grep -Fc '    root_abi: RootAbiPreflight,' "$$usr_exchange_authority" )" = 3; \
	timeout 10s grep -Fq 'TransitionJournalStore::try_open_in_retained_cast' "$$usr_exchange_authority"; \
	timeout 10s grep -Fq 'JournalAcquisition::CoordinatorNonblocking' "$$tree_lifecycle"; \
	timeout 10s grep -Fq 'Self::LegacyBlocking => TransitionJournalStore::open_in_retained_cast' "$$tree_lifecycle"; \
	timeout 10s grep -Fq 'Self::CoordinatorNonblocking(seal)' "$$tree_lifecycle"; \
	if timeout 10s grep -nE 'exchange_reverse|create_root_links|renameat2|symlinkat|unlinkat' "$$usr_exchange_authority"; then \
		timeout 10s printf '%s\n' 'client exchange authority acquired namespace mutation primitives' >&2; exit 1; \
	else \
		status="$$?"; timeout 10s test "$$status" = 1; \
	fi; \
	if callsites="$$( timeout 10s grep -RInE \
		'begin_transition|begin_fresh_allocation|transition_id_for_allocation|finish_fresh_allocation|begin_candidate_prepare|finish_candidate_prepare|reserve_for_transaction_triggers|run_transaction_triggers|begin_usr_exchange_intent|execute_usr_exchange' \
		--include='*.rs' --exclude-dir=journal_coordinator crates/forge/src )"; then \
		timeout 10s printf '%s\n' 'journal coordinator has a live callsite outside its contract module:' "$$callsites" >&2; \
		exit 1; \
	else \
		status=$$?; \
		timeout 10s test "$$status" = 1; \
	fi; \
	timeout 900s $(CARGO) test -p forge --lib \
		"transition_identity::journal_coordinator::tests::journal_coordinator_" \
		-- --test-threads=1
