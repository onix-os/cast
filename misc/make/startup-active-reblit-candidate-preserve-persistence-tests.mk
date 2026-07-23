.PHONY: forge-startup-usr-rollback-active-reblit-candidate-preserve-persistence-test

forge-startup-usr-rollback-active-reblit-candidate-preserve-persistence-test:
	@set -euo pipefail; \
	listed="$$( timeout 300s $(CARGO) test -p forge --lib -- --list )"; \
	timeout 10s grep -q . <<<"$$listed"; \
	prefix='client::startup_recovery::usr_rollback_active_reblit_candidate_preserve_persistence::tests::'; \
	count="$$( timeout 10s grep -c "^$$prefix.*: test$$" <<<"$$listed" )"; \
	timeout 10s test "$$count" = 14; \
	for name in \
		matrix::startup_active_reblit_candidate_preserve_persistence_applied_matrix_persists_exact_successor \
		matrix::startup_active_reblit_candidate_preserve_persistence_finish_matrix_persists_exact_successor \
		matrix::startup_active_reblit_candidate_preserve_persistence_changes_only_the_canonical_journal \
		evidence_races::startup_active_reblit_candidate_preserve_persistence_rejects_mixed_and_cross_root_journals \
		evidence_races::startup_active_reblit_candidate_preserve_persistence_final_races_fail_before_advance \
		storage_reopen::startup_active_reblit_candidate_preserve_persistence_faults_reopen_exact_source_or_successor \
		storage_reopen::startup_active_reblit_candidate_preserve_persistence_consumes_old_store_and_reopens_exact_success \
		restart::startup_active_reblit_candidate_preserve_persistence_source_fault_restart_finishes_without_second_exchange \
		restart::startup_active_reblit_candidate_preserve_persistence_successor_fault_restart_skips_preservation \
		record_binding::startup_active_reblit_candidate_preserve_bound_advance_same_byte_replacements_never_succeed \
		record_binding::startup_active_reblit_candidate_preserve_same_byte_successor_replacement_after_publication_fails_exact_binding \
		record_binding::startup_active_reblit_candidate_preserve_same_byte_successor_replacement_after_same_store_binding_fails_reopened_binding \
		production_dispatch::startup_active_reblit_candidate_preserve_production_leaf_dispatches_applied_and_finish_exactly_once \
		production_dispatch::startup_active_reblit_candidate_preserve_production_leaf_source_fault_restarts_finish_without_second_exchange; do \
		timeout 10s grep -Fqx "$$prefix$$name: test" <<<"$$listed"; \
	done; \
	executor=crates/forge/src/client/startup_recovery/usr_rollback_active_reblit_candidate_preserve_persistence.rs; \
	recovery_root=crates/forge/src/client/startup_recovery.rs; \
	production_dispatch=crates/forge/src/client/startup_recovery/usr_rollback_candidate_preserve_dispatch.rs; \
	authority_root=crates/forge/src/client/startup_reconciliation/usr_rollback_candidate_preserve_authority.rs; \
	authority_active=crates/forge/src/client/startup_reconciliation/usr_rollback_candidate_preserve_authority/active_reblit_effect.rs; \
	authority_post=crates/forge/src/client/startup_reconciliation/usr_rollback_candidate_preserve_authority/active_reblit_effect/post_exchange_durability.rs; \
	authority_persistence=crates/forge/src/client/startup_reconciliation/usr_rollback_candidate_preserve_authority/active_reblit_effect/post_exchange_durability/persistence.rs; \
	journal_record_binding=crates/forge/src/transition_journal/store/record_binding.rs; \
	proof=crates/forge/src/client/startup_reconciliation/activation_namespace/candidate_preserve_proof.rs; \
	capture=crates/forge/src/client/startup_reconciliation/activation_namespace/capture/mod.rs; \
	namespace=crates/forge/src/client/startup_reconciliation/activation_namespace/capture/active_reblit_candidate_preserve.rs; \
	namespace_post=crates/forge/src/client/startup_reconciliation/activation_namespace/capture/active_reblit_candidate_preserve/post_exchange_durability.rs; \
	reconciliation_root=crates/forge/src/client/startup_reconciliation.rs; \
	tests=crates/forge/src/client/startup_recovery/usr_rollback_active_reblit_candidate_preserve_persistence/tests.rs; \
	support=crates/forge/src/client/startup_recovery/usr_rollback_active_reblit_candidate_preserve_persistence/tests/support.rs; \
	matrix=crates/forge/src/client/startup_recovery/usr_rollback_active_reblit_candidate_preserve_persistence/tests/matrix.rs; \
	races=crates/forge/src/client/startup_recovery/usr_rollback_active_reblit_candidate_preserve_persistence/tests/evidence_races.rs; \
	storage=crates/forge/src/client/startup_recovery/usr_rollback_active_reblit_candidate_preserve_persistence/tests/storage_reopen.rs; \
	restart=crates/forge/src/client/startup_recovery/usr_rollback_active_reblit_candidate_preserve_persistence/tests/restart.rs; \
	record_binding=crates/forge/src/client/startup_recovery/usr_rollback_active_reblit_candidate_preserve_persistence/tests/record_binding.rs; \
	production_tests=crates/forge/src/client/startup_recovery/usr_rollback_active_reblit_candidate_preserve_persistence/tests/production_dispatch.rs; \
	timeout 10s grep -Fqx 'mod usr_rollback_active_reblit_candidate_preserve_persistence;' "$$recovery_root"; \
	if timeout 10s awk 'previous == "#[cfg(test)]" && $$0 == "mod usr_rollback_active_reblit_candidate_preserve_persistence;" { found = 1 } { previous = $$0 } END { exit !found }' "$$recovery_root"; then exit 1; else status="$$?"; timeout 10s test "$$status" = 1; fi; \
	for pair in "$$authority_root:mod active_reblit_effect;" "$$proof:mod active_reblit_effect;" "$$capture:mod active_reblit_candidate_preserve;"; do \
		file="$${pair%%:*}"; declaration="$${pair#*:}"; \
		timeout 10s grep -Fqx "$$declaration" "$$file"; \
		if timeout 10s awk -v declaration="$$declaration" 'previous == "#[cfg(test)]" && $$0 == declaration { found = 1 } { previous = $$0 } END { exit !found }' "$$file"; then exit 1; else status="$$?"; timeout 10s test "$$status" = 1; fi; \
	done; \
	timeout 10s grep -Fqx 'mod persistence;' "$$authority_post"; \
	timeout 10s rg -U -q '^pub\(in crate::client\) fn persist_usr_rollback_active_reblit_candidate_preserve_and_reopen\(\n    journal: TransitionJournalStore,\n    authority: UsrRollbackActiveReblitCandidatePreserveDurableEffectAuthority<'"'"'_>,\n\) -> Result<\(TransitionJournalStore, TransitionRecord\), UsrRollbackActiveReblitCandidatePreservePersistenceError> \{' "$$executor"; \
	if timeout 10s rg -n 'RollbackActionOutcome|rollback_successor\(' "$$executor"; then exit 1; else status="$$?"; timeout 10s test "$$status" = 1; fi; \
	timeout 10s test "$$( timeout 10s grep -Fc 'authority.revalidate(&journal)' "$$executor" )" = 1; \
	if timeout 10s rg -n 'candidate_preserved_successor\(' "$$executor" "$$authority_persistence"; then exit 1; else status="$$?"; timeout 10s test "$$status" = 1; fi; \
	timeout 10s grep -Fq '.rollback_successor(Some(outcome))' "$$authority_persistence"; \
	if timeout 10s rg -U -n 'fn[^\(]*\([^\)]*(origin|outcome)[^\)]*\)' "$$executor" "$$authority_persistence"; then exit 1; else status="$$?"; timeout 10s test "$$status" = 1; fi; \
	if timeout 10s rg -n '\.advance\(' "$$executor" "$$authority_persistence"; then exit 1; else status="$$?"; timeout 10s test "$$status" = 1; fi; \
	timeout 10s grep -Fqx '    let advance = match authority.advance_candidate_preserved_record_binding(&journal) {' "$$executor"; \
	timeout 10s grep -Fq 'pub(in crate::client) fn advance_candidate_preserved_record_binding(' "$$authority_persistence"; \
	timeout 10s test "$$( timeout 10s grep -Fc 'advance_candidate_preserved_record_binding' "$$executor" )" = 1; \
	timeout 10s test "$$( timeout 10s grep -Fc 'advance_candidate_preserved_record_binding' "$$authority_persistence" )" = 1; \
	timeout 10s test "$$( timeout 10s grep -Fc 'journal.advance_record_binding(cast, self._effect.journal_record_binding, &successor)' "$$authority_persistence" )" = 1; \
	if rg -U -n '#\[derive\([^]]*Clone[^]]*\)\]\n(?:#\[[^\n]*\]\n)*pub\(crate\) struct TransitionJournalRecordBinding' "$$journal_record_binding"; then exit 1; else status="$$?"; test "$$status" = 1; fi; \
	if rg -n 'impl[[:space:]]+Clone[[:space:]]+for[[:space:]]+TransitionJournalRecordBinding' "$$journal_record_binding"; then exit 1; else status="$$?"; test "$$status" = 1; fi; \
	timeout 10s grep -Fqx '            let (successor, successor_binding) = published.into_parts();' "$$executor"; \
	timeout 10s test "$$( timeout 10s grep -Fc 'published.into_parts()' "$$executor" )" = 1; \
	first_revalidate="$$( timeout 10s grep -nF 'authority.revalidate(&journal)' "$$executor" | timeout 10s head -n 1 | timeout 10s cut -d: -f1 )"; \
	seam_line="$$( timeout 10s grep -nF '    before_usr_rollback_active_reblit_candidate_preserve_persistence_final_revalidation();' "$$executor" | timeout 10s cut -d: -f1 )"; \
	clone_line="$$( timeout 10s grep -nF '    let installation = authority.installation().clone();' "$$executor" | timeout 10s cut -d: -f1 )"; \
	advance_line="$$( timeout 10s grep -nF '    let advance = match authority.advance_candidate_preserved_record_binding(&journal) {' "$$executor" | timeout 10s cut -d: -f1 )"; \
	same_store_line="$$( timeout 10s grep -nF '            let exact = revalidate_published_active_reblit_candidate_preserved_binding(' "$$executor" | timeout 10s cut -d: -f1 )"; \
	drop_journal="$$( timeout 10s grep -nF '    drop(journal);' "$$executor" | timeout 10s tail -n 1 | timeout 10s cut -d: -f1 )"; \
	reopen_seam_line="$$( timeout 10s grep -nF '        after_usr_rollback_active_reblit_candidate_preserve_successor_binding_check_before_reopen();' "$$executor" | timeout 10s cut -d: -f1 )"; \
	reopen_line="$$( timeout 10s grep -nF '        reopen_canonical_journal(&installation).map_err(UsrRollbackActiveReblitCandidatePreserveReopenError::from);' "$$executor" | timeout 10s cut -d: -f1 )"; \
	timeout 10s test "$$first_revalidate" -lt "$$seam_line"; \
	timeout 10s test "$$seam_line" -lt "$$clone_line"; \
	timeout 10s test "$$clone_line" -lt "$$advance_line"; \
	timeout 10s test "$$advance_line" -lt "$$same_store_line"; \
	timeout 10s test "$$same_store_line" -lt "$$drop_journal"; \
	timeout 10s test "$$drop_journal" -lt "$$reopen_seam_line"; \
	timeout 10s test "$$reopen_seam_line" -lt "$$reopen_line"; \
	suffix="$$( timeout 10s sed -n '/    let advance = match authority.advance_candidate_preserved_record_binding/,/        reopen_canonical_journal(&installation)/p' "$$executor" )"; \
	if timeout 10s grep -Fq 'drop(authority)' <<<"$$suffix"; then exit 1; else status="$$?"; timeout 10s test "$$status" = 1; fi; \
	timeout 10s test "$$( timeout 10s grep -Fc 'reopen_canonical_journal(&installation)' <<<"$$suffix" )" = 1; \
	published_branch="$$( timeout 10s sed -n '/        Ok(published) => {/,/        Err(UsrRollbackActiveReblitCandidatePreserveRecordAdvanceError::Authority(source)) => {/p' "$$executor" )"; \
	timeout 10s test "$$( timeout 10s grep -Fc '        Ok(published) => {' <<<"$$published_branch" )" = 1; \
	timeout 10s test "$$( timeout 10s grep -Fc 'UsrRollbackActiveReblitCandidatePreserveAdvanceOutcome::Published {' <<<"$$published_branch" )" = 1; \
	timeout 10s test "$$( timeout 10s grep -Fc 'UsrRollbackActiveReblitCandidatePreserveAdvanceOutcome::SuccessorBindingFailed {' <<<"$$published_branch" )" = 2; \
	if timeout 10s rg -n '(^|[^[:alnum:]_])return([^[:alnum:]_]|$$)|\?|panic!|unreachable!|\.unwrap\(|\.expect\(' <<<"$$published_branch"; then exit 1; else status="$$?"; timeout 10s test "$$status" = 1; fi; \
	if timeout 10s rg -n 'open_in_retained_cast|journal\.load\(' "$$executor"; then exit 1; else status="$$?"; timeout 10s test "$$status" = 1; fi; \
	timeout 10s test "$$( timeout 10s grep -Fc '.has_record_binding(cast, successor_binding, successor)' "$$executor" )" = 1; \
	timeout 10s test "$$( timeout 10s grep -Fc '.has_reopened_record_binding(cast, successor_binding, successor)' "$$executor" )" = 1; \
	published_binding_helper="$$( timeout 10s sed -n '/^fn revalidate_published_active_reblit_candidate_preserved_binding(/,/^}/p' "$$executor" )"; \
	timeout 10s test "$$( timeout 10s grep -Fc '.revalidate_mutable_namespace()' <<<"$$published_binding_helper" )" = 2; \
	timeout 10s grep -Fq '.has_record_binding(cast, successor_binding, successor)' <<<"$$published_binding_helper"; \
	reopened_binding_helper="$$( timeout 10s sed -n '/^fn revalidate_reopened_active_reblit_candidate_preserved_binding(/,/^}/p' "$$executor" )"; \
	timeout 10s test "$$( timeout 10s grep -Fc '.revalidate_mutable_namespace()' <<<"$$reopened_binding_helper" )" = 2; \
	timeout 10s grep -Fq '.has_reopened_record_binding(cast, successor_binding, successor)' <<<"$$reopened_binding_helper"; \
	timeout 10s test "$$( timeout 10s grep -Fc 'reopen_canonical_journal(&installation)' "$$executor" )" = 1; \
	timeout 10s grep -Fqx '                                durable: DurableUsrRollbackActiveReblitCandidatePreserveRecord::CandidatePreserved,' "$$executor"; \
	timeout 10s test "$$( timeout 10s grep -Fc '            Ok((reopened, Some(actual))) if actual == source_record => {' "$$executor" )" = 2; \
	timeout 10s test "$$( timeout 10s grep -Fc '            Ok((reopened, Some(actual))) if actual == successor => {' "$$executor" )" = 3; \
	timeout 10s grep -Fqx '                    Ok(true) => Ok((reopened, successor)),' "$$executor"; \
	timeout 10s test "$$drop_journal" -lt "$$reopen_line"; \
	timeout 10s grep -Fqx '                    durable: DurableUsrRollbackActiveReblitCandidatePreserveRecord::Source,' "$$executor"; \
	timeout 10s grep -Fqx '                    durable: DurableUsrRollbackActiveReblitCandidatePreserveRecord::CandidatePreserved,' "$$executor"; \
	timeout 10s grep -Fq 'run_before_durable_post_revalidation_capture();' "$$namespace_post"; \
	timeout 10s grep -Fq 'run_before_persistence_durable_trailing_evidence();' "$$authority_persistence"; \
	persistence_code="$$( timeout 10s sed -E 's,//.*$$,,' "$$executor" "$$authority_persistence" )"; \
	if timeout 10s rg -n 'renameat|exchange_once|std::fs::rename[[:space:]]*\(|(^|[^_[:alnum:]])fs::rename[[:space:]]*\(|mkdir|create_dir|set_permissions|chmod|unlink|remove_dir|remove_file|sync_all|sync_data' <<<"$$persistence_code"; then exit 1; else status="$$?"; timeout 10s test "$$status" = 1; fi; \
	if timeout 10s rg -n '^[[:space:]]*(loop|while|for)[[:space:]]|=[[:space:]]*(loop|while|for)[[:space:]]|retry' <<<"$$persistence_code"; then exit 1; else status="$$?"; timeout 10s test "$$status" = 1; fi; \
	if timeout 10s rg -n 'clear_transition_if_matches|remove_transition_if_matches|insert_fresh_metadata|delete_metadata|run_transaction_triggers|run_system_triggers|cleanup|archive_previous|rearchive_archived|preserve_failed|\.execute\(|\.transaction\(|\.delete\(' <<<"$$persistence_code"; then exit 1; else status="$$?"; timeout 10s test "$$status" = 1; fi; \
	if timeout 10s rg -n 'pub\([^)]*\)[[:space:]]+fn[[:space:]]+.*(descriptor|raw_fd|wrapper_index|target_name)|AsRawFd|RawFd' "$$executor" "$$authority_persistence"; then exit 1; else status="$$?"; timeout 10s test "$$status" = 1; fi; \
	production_refs="$$( timeout 10s rg -n -F 'persist_usr_rollback_active_reblit_candidate_preserve_and_reopen' crates/forge/src/client --glob '*.rs' --glob '!**/tests/**' --glob '!**/tests.rs' --glob '!**/usr_rollback_active_reblit_candidate_preserve_persistence.rs' )"; \
	timeout 10s test "$$( timeout 10s grep -c . <<<"$$production_refs" )" = 3; \
	timeout 10s test "$$( timeout 10s grep -Fc "$$recovery_root:" <<<"$$production_refs" )" = 1; \
	timeout 10s test "$$( timeout 10s grep -Fc "$$production_dispatch:" <<<"$$production_refs" )" = 2; \
	timeout 10s grep -Fq 'persist_usr_rollback_active_reblit_candidate_preserve_and_reopen(journal, durable)' "$$production_dispatch"; \
	timeout 10s grep -Fq 'UsrRollbackCandidatePreserveApplyEffectSelection::ExchangeActiveReblit' "$$production_dispatch"; \
	timeout 10s grep -Fq 'UsrRollbackCandidatePreserveFinishDurabilitySelection::ActiveReblit' "$$production_dispatch"; \
	timeout 10s grep -Fq 'UsrRollbackCandidatePreserveApplyEffectSelection::Unsupported' "$$production_dispatch"; \
	timeout 10s grep -Fq 'UsrRollbackCandidatePreserveDispatchError::ActiveReblitPersistence' "$$production_tests"; \
	timeout 10s test "$$( timeout 10s rg -n '^#\[test\]$$' "$$matrix" "$$races" "$$storage" "$$restart" "$$record_binding" "$$production_tests" | timeout 10s wc -l )" = 14; \
	source_axis="$$( timeout 10s sed -n '/^    pub(super) const ALL: \[Self; 4\] = \[/,/^    \];/p' "$$support" )"; \
	timeout 10s grep -Fqx '    pub(super) const ALL: [Self; 4] = [' <<<"$$source_axis"; \
	timeout 10s test "$$( timeout 10s grep -Fc '        Self::' <<<"$$source_axis" )" = 4; \
	for source in Intent Exchanged RootLinksComplete BootSyncStarted; do timeout 10s grep -Fqx "        Self::$$source," <<<"$$source_axis"; done; \
	timeout 10s grep -Fqx '        (Epoch::Current, Source::RootLinksComplete) => CandidatePreserveFixture::new(' "$$support"; \
	timeout 10s grep -Fqx '        (Epoch::Historical, Source::RootLinksComplete) => CandidatePreserveFixture::historical(' "$$support"; \
	timeout 10s test "$$( timeout 10s grep -Fc '    assert_eq!(cases, 16);' "$$matrix" )" = 1; \
	timeout 10s grep -Fqx '    exercise_success_matrix(CandidateOrigin::Applied);' "$$matrix"; \
	timeout 10s grep -Fqx '    exercise_success_matrix(CandidateOrigin::AlreadySatisfied);' "$$matrix"; \
	timeout 10s test "$$( timeout 10s grep -Fc '    assert_eq!(exercised, 32);' "$$production_tests" )" = 1; \
	for race in Database Provenance Journal Installation Namespace Plan; do timeout 10s grep -Fq "EvidenceRace::$$race" "$$races"; done; \
	for fault in temporary_sync update_exchange update_first_directory_sync displaced_unlink update_final_directory_sync; do timeout 10s grep -Fq "$$fault" "$$storage"; done; \
	timeout 10s test "$$( timeout 10s rg -n '^            arm_next_(temporary_sync|update_exchange|update_first_directory_sync|displaced_unlink|update_final_directory_sync)_fault,$$' "$$storage" | timeout 10s wc -l )" = 5; \
	for axis in 'for epoch in Epoch::ALL {' 'for origin in CandidateOrigin::ALL {' 'for source in Source::ALL {' 'for usr_outcome in [RollbackActionOutcome::Applied, RollbackActionOutcome::AlreadySatisfied] {'; do timeout 10s grep -Fq "$$axis" "$$storage"; done; \
	timeout 10s test "$$( timeout 10s grep -Fc 'assert_eq!(fixture.fixture.database_snapshot(), database_before);' "$$storage" )" = 2; \
	timeout 10s test "$$( timeout 10s grep -Fc 'assert_eq!(non_journal_namespace_snapshot(&fixture), namespace_before);' "$$storage" )" = 2; \
	timeout 10s test "$$( timeout 10s grep -Fc 'assert_eq!(active_reblit_candidate_preserve_exchange_attempt_count(), effect_count_before);' "$$storage" )" = 2; \
	timeout 10s test "$$( timeout 10s grep -Fc '    assert_eq!(exercised, 160);' "$$storage" )" = 1; \
	timeout 10s grep -Fqx 'mod record_binding;' "$$tests"; \
	for seam in BeforeBoundAdvancePublish BeforeBoundAdvanceFinalBinding; do timeout 10s test "$$( timeout 10s grep -Fc "PublicBindingRevalidationBoundary::$$seam" "$$record_binding" )" = 1; done; \
	timeout 10s test "$$( timeout 10s grep -Fc 'arm_public_binding_revalidation_callback(boundary, hook);' "$$record_binding" )" = 1; \
	timeout 10s test "$$( timeout 10s grep -Fc 'arm_before_usr_rollback_active_reblit_candidate_preserve_successor_binding_revalidation(hook);' "$$record_binding" )" = 1; \
	timeout 10s test "$$( timeout 10s grep -Fc 'arm_after_usr_rollback_active_reblit_candidate_preserve_successor_binding_check_before_reopen(hook);' "$$record_binding" )" = 1; \
	for axis in 'for epoch in Epoch::ALL {' 'for source in Source::ALL {' 'for origin in CandidateOrigin::ALL {' 'for usr_outcome in [RollbackActionOutcome::Applied, RollbackActionOutcome::AlreadySatisfied] {'; do timeout 10s test "$$( timeout 10s grep -Fc "$$axis" "$$record_binding" )" = 3; done; \
	timeout 10s grep -Fq 'assert_ne!(retained_identity, inode_identity(&canonical));' "$$record_binding"; \
	timeout 10s test "$$( timeout 10s grep -Fc 'assert_unchanged_outside_journal(' "$$record_binding" )" = 4; \
	timeout 10s grep -Fq 'active_reblit_candidate_preserve_exchange_attempt_count()' "$$record_binding"; \
	timeout 10s grep -Fq 'assert_eq!(names.len(), 2, "bound update left journal residue: {names:?}");' "$$record_binding"; \
	timeout 10s test "$$( timeout 10s grep -Fc '    assert_eq!(exercised, 64);' "$$record_binding" )" = 1; \
	timeout 10s test "$$( timeout 10s grep -Fc '    assert_eq!(exercised, 32);' "$$record_binding" )" = 2; \
	for axis in 'for epoch in Epoch::ALL {' 'for source in Source::ALL {' 'for usr_outcome in [RollbackActionOutcome::Applied, RollbackActionOutcome::AlreadySatisfied] {'; do timeout 10s test "$$( timeout 10s grep -Fc "$$axis" "$$restart" )" = 2; done; \
	timeout 10s test "$$( timeout 10s rg -n 'for (first_)?origin in CandidateOrigin::ALL \{' "$$restart" | timeout 10s wc -l )" = 2; \
	timeout 10s test "$$( timeout 10s grep -Fc 'drop(reservation);' "$$restart" )" = 4; \
	timeout 10s test "$$( timeout 10s grep -Fc 'ActiveStateReservation::acquire().unwrap();' "$$restart" )" = 4; \
	timeout 10s test "$$( timeout 10s grep -Fc '    assert_eq!(exercised, 32);' "$$restart" )" = 2; \
	timeout 10s grep -Fq 'expected_post_events(&fixture)' "$$restart"; \
	for file in "$$executor" "$$recovery_root" "$$production_dispatch" "$$authority_root" "$$authority_active" "$$authority_post" "$$authority_persistence" "$$journal_record_binding" "$$proof" "$$capture" "$$namespace" "$$namespace_post" "$$reconciliation_root" "$$tests" "$$support" "$$matrix" "$$races" "$$storage" "$$restart" "$$record_binding" "$$production_tests" misc/make/startup-active-reblit-candidate-preserve-persistence-tests.mk Makefile; do \
		timeout 10s test "$$( timeout 10s wc -l < "$$file" )" -le 1000; \
	done; \
	timeout 1800s $(CARGO) test -p forge --lib "$$prefix" -- --test-threads=1
