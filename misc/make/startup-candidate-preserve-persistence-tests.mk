.PHONY: forge-startup-usr-rollback-candidate-preserve-persistence-test

forge-startup-usr-rollback-candidate-preserve-persistence-test:
	@set -eu; \
	listed="$$( timeout 300s $(CARGO) test -p forge --lib -- --list )"; \
	timeout 10s grep -q . <<<"$$listed"; \
	prefix='client::startup_recovery::usr_rollback_candidate_preserve_persistence::tests::'; \
	count="$$( timeout 10s grep -c "^$$prefix.*: test$$" <<<"$$listed" )"; \
	timeout 10s test "$$count" = 12; \
	for name in \
		matrix::startup_usr_rollback_candidate_preserve_persistence_applied_matrix_persists_exact_successor \
		matrix::startup_usr_rollback_candidate_preserve_persistence_finish_matrix_persists_exact_successor \
		matrix::startup_usr_rollback_candidate_preserve_persistence_changes_only_the_canonical_journal \
		evidence_races::startup_usr_rollback_candidate_preserve_persistence_rejects_mixed_and_cross_root_journals \
		evidence_races::startup_usr_rollback_candidate_preserve_persistence_final_races_fail_before_advance \
		storage_reopen::startup_usr_rollback_candidate_preserve_persistence_faults_reopen_exact_source_or_successor \
		storage_reopen::startup_usr_rollback_candidate_preserve_persistence_consumes_old_store_and_reopens_exact_success \
		restart::startup_usr_rollback_candidate_preserve_source_fault_restart_finishes_without_second_move \
		restart::startup_usr_rollback_candidate_preserve_successor_fault_restart_skips_preservation \
		record_binding::startup_usr_rollback_candidate_preserve_bound_advance_same_byte_replacements_never_succeed \
		record_binding::startup_usr_rollback_candidate_preserve_same_byte_successor_replacement_after_publication_fails_exact_binding \
		record_binding::startup_usr_rollback_candidate_preserve_same_byte_successor_replacement_after_same_store_binding_fails_reopened_binding; do \
		timeout 10s grep -Fqx "$$prefix$$name: test" <<<"$$listed"; \
	done; \
	executor=crates/forge/src/client/startup_recovery/usr_rollback_candidate_preserve_persistence.rs; \
	authority_parent=crates/forge/src/client/startup_reconciliation/usr_rollback_candidate_preserve_authority/effect_reconciliation/post_move_durability.rs; \
	authority=crates/forge/src/client/startup_reconciliation/usr_rollback_candidate_preserve_authority/effect_reconciliation/post_move_durability/persistence.rs; \
	reopen=crates/forge/src/client/startup_recovery/canonical_journal_reopen.rs; \
	root=crates/forge/src/client/startup_recovery.rs; \
	production_dispatch=crates/forge/src/client/startup_recovery/usr_rollback_candidate_preserve_dispatch.rs; \
	tests=crates/forge/src/client/startup_recovery/usr_rollback_candidate_preserve_persistence/tests.rs; \
	support=crates/forge/src/client/startup_recovery/usr_rollback_candidate_preserve_persistence/tests/support.rs; \
	matrix=crates/forge/src/client/startup_recovery/usr_rollback_candidate_preserve_persistence/tests/matrix.rs; \
	races=crates/forge/src/client/startup_recovery/usr_rollback_candidate_preserve_persistence/tests/evidence_races.rs; \
	storage=crates/forge/src/client/startup_recovery/usr_rollback_candidate_preserve_persistence/tests/storage_reopen.rs; \
	restart=crates/forge/src/client/startup_recovery/usr_rollback_candidate_preserve_persistence/tests/restart.rs; \
	record_binding=crates/forge/src/client/startup_recovery/usr_rollback_candidate_preserve_persistence/tests/record_binding.rs; \
	timeout 10s grep -Fqx 'mod usr_rollback_candidate_preserve_persistence;' "$$root"; \
	timeout 10s test "$$( timeout 10s grep -Fc 'mod usr_rollback_candidate_preserve_persistence;' "$$root" )" = 1; \
	timeout 10s grep -Fqx 'mod persistence;' "$$authority_parent"; \
	timeout 10s rg -U -q '^pub\(in crate::client\) fn persist_usr_rollback_candidate_preserve_and_reopen\(\n    journal: TransitionJournalStore,\n    authority: UsrRollbackNewStateCandidatePreserveDurableEffectAuthority<'"'"'_>,\n\) -> Result<\(TransitionJournalStore, TransitionRecord\), UsrRollbackCandidatePreservePersistenceError> \{' "$$executor"; \
	if timeout 10s rg -n 'RollbackActionOutcome|rollback_successor\(' "$$executor"; then exit 1; fi; \
	timeout 10s test "$$( timeout 10s grep -Fc 'authority.revalidate(&journal)' "$$executor" )" = 1; \
	if timeout 10s rg -n 'candidate_preserved_successor\(' "$$executor" "$$authority"; then exit 1; else status="$$?"; timeout 10s test "$$status" = 1; fi; \
	timeout 10s grep -Fq '.rollback_successor(Some(self.origin))' "$$authority"; \
	if timeout 10s rg -U -n 'fn[^\(]*\([^\)]*(origin|outcome)[^\)]*\)' "$$executor" "$$authority"; then exit 1; fi; \
	if timeout 10s rg -n '\.advance\(' "$$executor" "$$authority" "$$reopen"; then exit 1; else status="$$?"; timeout 10s test "$$status" = 1; fi; \
	timeout 10s grep -Fqx '    let advance = match authority.advance_candidate_preserved_record_binding(&journal) {' "$$executor"; \
	timeout 10s grep -Fq 'pub(in crate::client) fn advance_candidate_preserved_record_binding(' "$$authority"; \
	timeout 10s test "$$( timeout 10s grep -Fc 'advance_candidate_preserved_record_binding' "$$executor" )" = 1; \
	timeout 10s test "$$( timeout 10s grep -Fc 'advance_candidate_preserved_record_binding' "$$authority" )" = 1; \
	timeout 10s test "$$( timeout 10s grep -Fc 'journal.advance_record_binding(cast, self._effect.journal_record_binding, &successor)' "$$authority" )" = 1; \
	timeout 10s grep -Fqx '            let (successor, successor_binding) = published.into_parts();' "$$executor"; \
	timeout 10s test "$$( timeout 10s grep -Fc 'published.into_parts()' "$$executor" )" = 1; \
	timeout 10s test "$$( timeout 10s grep -Fc 'persist_usr_rollback_candidate_preserve_and_reopen(' "$$executor" )" = 1; \
	timeout 10s grep -Fqx '    UsrRollbackCandidatePreservePersistenceError, persist_usr_rollback_candidate_preserve_and_reopen,' "$$root"; \
	timeout 10s test "$$( timeout 10s grep -Fc 'persist_usr_rollback_candidate_preserve_and_reopen,' "$$root" )" = 1; \
	timeout 10s test "$$( timeout 10s grep -Fc 'persist_usr_rollback_candidate_preserve_and_reopen(journal, durable)' "$$production_dispatch" )" = 1; \
	production_references="$$( timeout 10s rg -n -F 'persist_usr_rollback_candidate_preserve_and_reopen' crates/forge/src/client --glob '*.rs' --glob '!**/tests/**' --glob '!**/tests.rs' --glob '!**/*_tests.rs' --glob '!**/*_tests/**' --glob '!**/usr_rollback_candidate_preserve_persistence.rs' )"; \
	timeout 10s test "$$( timeout 10s grep -c . <<<"$$production_references" )" = 3; \
	timeout 10s test "$$( timeout 10s grep -Fc "$$root:" <<<"$$production_references" )" = 1; \
	timeout 10s test "$$( timeout 10s grep -Fc "$$production_dispatch:" <<<"$$production_references" )" = 2; \
	first_revalidate_line="$$( timeout 10s grep -nF 'authority.revalidate(&journal)' "$$executor" | timeout 10s head -n 1 | timeout 10s cut -d: -f1 )"; \
	seam_line="$$( timeout 10s grep -nF '    before_usr_rollback_candidate_preserve_persistence_final_revalidation();' "$$executor" | timeout 10s cut -d: -f1 )"; \
	clone_line="$$( timeout 10s grep -nF '    let installation = authority.installation().clone();' "$$executor" | timeout 10s cut -d: -f1 )"; \
	advance_line="$$( timeout 10s grep -nF '    let advance = match authority.advance_candidate_preserved_record_binding(&journal) {' "$$executor" | timeout 10s cut -d: -f1 )"; \
	same_store_line="$$( timeout 10s grep -nF '            let exact = revalidate_published_candidate_preserved_binding(' "$$executor" | timeout 10s cut -d: -f1 )"; \
	drop_journal_line="$$( timeout 10s grep -nF '    drop(journal);' "$$executor" | timeout 10s tail -n 1 | timeout 10s cut -d: -f1 )"; \
	reopen_seam_line="$$( timeout 10s grep -nF '        after_usr_rollback_candidate_preserve_successor_binding_check_before_reopen();' "$$executor" | timeout 10s cut -d: -f1 )"; \
	reopen_line="$$( timeout 10s grep -nF '    let reopened = reopen_canonical_journal(&installation).map_err(UsrRollbackCandidatePreserveReopenError::from);' "$$executor" | timeout 10s cut -d: -f1 )"; \
	timeout 10s test "$$first_revalidate_line" -lt "$$seam_line"; \
	timeout 10s test "$$seam_line" -lt "$$clone_line"; \
	timeout 10s test "$$clone_line" -lt "$$advance_line"; \
	timeout 10s test "$$advance_line" -lt "$$same_store_line"; \
	timeout 10s test "$$same_store_line" -lt "$$drop_journal_line"; \
	timeout 10s test "$$drop_journal_line" -lt "$$reopen_seam_line"; \
	timeout 10s test "$$reopen_seam_line" -lt "$$reopen_line"; \
	timeout 10s test "$$drop_journal_line" -lt "$$reopen_line"; \
	suffix="$$( timeout 10s sed -n '/    let advance = match authority.advance_candidate_preserved_record_binding/,/    let reopened = reopen_canonical_journal/p' "$$executor" )"; \
	if timeout 10s grep -Fq 'drop(authority)' <<<"$$suffix"; then exit 1; else status="$$?"; timeout 10s test "$$status" = 1; fi; \
	timeout 10s test "$$( timeout 10s grep -Fc 'reopen_canonical_journal(&installation)' <<<"$$suffix" )" = 1; \
	published_branch="$$( timeout 10s sed -n '/        Ok(published) => {/,/        Err(UsrRollbackCandidatePreserveRecordAdvanceError::Authority(source)) => {/p' "$$executor" )"; \
	timeout 10s test "$$( timeout 10s grep -Fc '        Ok(published) => {' <<<"$$published_branch" )" = 1; \
	timeout 10s test "$$( timeout 10s grep -Fc 'UsrRollbackCandidatePreserveAdvanceOutcome::Published {' <<<"$$published_branch" )" = 1; \
	timeout 10s test "$$( timeout 10s grep -Fc 'UsrRollbackCandidatePreserveAdvanceOutcome::SuccessorBindingFailed {' <<<"$$published_branch" )" = 2; \
	if timeout 10s rg -n '(^|[^[:alnum:]_])return([^[:alnum:]_]|$$)|\?|panic!|unreachable!|\.unwrap\(|\.expect\(' <<<"$$published_branch"; then exit 1; else status="$$?"; timeout 10s test "$$status" = 1; fi; \
	if timeout 10s rg -n 'open_in_retained_cast|journal\.load\(' "$$executor"; then exit 1; else status="$$?"; timeout 10s test "$$status" = 1; fi; \
	timeout 10s test "$$( timeout 10s grep -Fc '.has_record_binding(cast, successor_binding, successor)' "$$executor" )" = 1; \
	timeout 10s test "$$( timeout 10s grep -Fc '.has_reopened_record_binding(cast, successor_binding, successor)' "$$executor" )" = 1; \
	published_binding_helper="$$( timeout 10s sed -n '/^fn revalidate_published_candidate_preserved_binding(/,/^}/p' "$$executor" )"; \
	timeout 10s test "$$( timeout 10s grep -Fc '.revalidate_mutable_namespace()' <<<"$$published_binding_helper" )" = 2; \
	timeout 10s grep -Fq '.has_record_binding(cast, successor_binding, successor)' <<<"$$published_binding_helper"; \
	reopened_binding_helper="$$( timeout 10s sed -n '/^fn revalidate_reopened_candidate_preserved_binding(/,/^}/p' "$$executor" )"; \
	timeout 10s test "$$( timeout 10s grep -Fc '.revalidate_mutable_namespace()' <<<"$$reopened_binding_helper" )" = 2; \
	timeout 10s grep -Fq '.has_reopened_record_binding(cast, successor_binding, successor)' <<<"$$reopened_binding_helper"; \
	timeout 10s grep -Fqx '                    durable: DurableUsrRollbackCandidatePreserveRecord::Source,' "$$executor"; \
	timeout 10s grep -Fqx '                    durable: DurableUsrRollbackCandidatePreserveRecord::CandidatePreserved,' "$$executor"; \
	timeout 10s test "$$( timeout 10s grep -Fc '            Ok((reopened, Some(actual))) if actual == source_record => {' "$$executor" )" = 2; \
	timeout 10s test "$$( timeout 10s grep -Fc '            Ok((reopened, Some(actual))) if actual == successor => {' "$$executor" )" = 3; \
	timeout 10s grep -Fqx '                    Ok(true) => Ok((reopened, successor)),' "$$executor"; \
	timeout 10s grep -Fqx '            CanonicalJournalReopenError::Installation(source) => Self::Installation(source),' "$$executor"; \
	timeout 10s grep -Fqx '            CanonicalJournalReopenError::Journal(source) => Self::Journal(source),' "$$executor"; \
	production_code="$$( timeout 10s sed -E 's,//.*$$,,' "$$executor" "$$authority" )"; \
	if timeout 10s rg -n 'renameat|std::fs::rename[[:space:]]*\(|(^|[^_[:alnum:]])fs::rename[[:space:]]*\(|attempt_move|reconcile_move|move_attempt|mkdir|create_dir|set_permissions|chmod|unlink|sync_all|sync_data' <<<"$$production_code"; then exit 1; fi; \
	if timeout 10s rg -n '^[[:space:]]*(loop|while|for)[[:space:]]|=[[:space:]]*(loop|while|for)[[:space:]]|retry' <<<"$$production_code"; then exit 1; fi; \
	if timeout 10s rg -n 'clear_transition_if_matches|remove_transition_if_matches|insert_fresh_metadata|delete_metadata|invalidation|invalidate|run_transaction_triggers|run_system_triggers|root_links|archive_previous|rearchive_archived|preserve_failed|remove_exact_archived|cleanup|\.add\(|\.remove\(|\.batch_remove\(|\.execute\(|\.transaction\(|\.delete\(' <<<"$$production_code"; then exit 1; fi; \
	if timeout 10s rg -n 'startup_gate|dispatch' "$$executor"; then exit 1; fi; \
	timeout 10s test "$$( timeout 10s grep -Fc 'for source in Source::ALL {' "$$matrix" )" = 1; \
	timeout 10s test "$$( timeout 10s grep -Fc 'for historical in [false, true] {' "$$matrix" )" = 1; \
	timeout 10s test "$$( timeout 10s rg -n '^            arm_next_(temporary_sync|update_exchange|update_first_directory_sync|displaced_unlink|update_final_directory_sync)_fault,$$' "$$storage" | timeout 10s wc -l )" = 5; \
	timeout 10s test "$$( timeout 10s grep -Fc 'for historical in [false, true] {' "$$storage" )" = 2; \
	timeout 10s test "$$( timeout 10s grep -Fc 'reset_new_state_candidate_preserve_move_attempt_count();' "$$storage" )" = 2; \
	timeout 10s test "$$( timeout 10s grep -Fc 'assert_eq!(non_journal_namespace_snapshot(&fixture), namespace_before);' "$$storage" )" = 2; \
	timeout 10s test "$$( timeout 10s grep -Fc 'assert_eq!(fixture.fixture.database_snapshot(), database_before);' "$$storage" )" = 2; \
	timeout 10s test "$$( timeout 10s grep -Fc 'assert_eq!(new_state_candidate_preserve_move_attempt_count(), effect_count_before);' "$$storage" )" = 2; \
	timeout 10s grep -Fqx 'mod record_binding;' "$$tests"; \
	for seam in BeforeBoundAdvancePublish BeforeBoundAdvanceFinalBinding; do timeout 10s test "$$( timeout 10s grep -Fc "PublicBindingRevalidationBoundary::$$seam" "$$record_binding" )" = 1; done; \
	timeout 10s test "$$( timeout 10s grep -Fc 'arm_public_binding_revalidation_callback(boundary, hook);' "$$record_binding" )" = 1; \
	timeout 10s test "$$( timeout 10s grep -Fc 'arm_before_usr_rollback_candidate_preserve_successor_binding_revalidation(hook);' "$$record_binding" )" = 1; \
	timeout 10s test "$$( timeout 10s grep -Fc 'arm_after_usr_rollback_candidate_preserve_successor_binding_check_before_reopen(hook);' "$$record_binding" )" = 1; \
	for axis in 'for historical in [false, true] {' 'for source in Source::ALL {' 'for origin in CandidateOrigin::ALL {' 'for usr_outcome in [RollbackActionOutcome::Applied, RollbackActionOutcome::AlreadySatisfied] {'; do timeout 10s test "$$( timeout 10s grep -Fc "$$axis" "$$record_binding" )" = 3; done; \
	timeout 10s grep -Fq 'assert_ne!(retained_identity, inode_identity(&canonical));' "$$record_binding"; \
	timeout 10s test "$$( timeout 10s grep -Fc 'assert_unchanged_outside_journal(' "$$record_binding" )" = 4; \
	timeout 10s grep -Fq 'new_state_candidate_preserve_move_attempt_count()' "$$record_binding"; \
	timeout 10s grep -Fq 'assert_eq!(names.len(), 2, "bound update left journal residue: {names:?}");' "$$record_binding"; \
	for race in Database Provenance Journal Installation Namespace Plan; do timeout 10s grep -Fq "EvidenceRace::$$race" "$$races"; done; \
	timeout 10s grep -Fq 'arm_before_new_state_candidate_preserve_durable_post_revalidation_capture' "$$races"; \
	timeout 10s grep -Fq 'arm_before_usr_rollback_candidate_preserve_durable_trailing_evidence' "$$races"; \
	timeout 10s test "$$( timeout 10s grep -Fc 'drop(reservation);' "$$restart" )" = 2; \
	timeout 10s test "$$( timeout 10s grep -Fc 'ActiveStateReservation::acquire().unwrap();' "$$restart" )" = 4; \
	timeout 10s grep -Fq 'expected_post_events(&fixture)' "$$restart"; \
	for file in "$$executor" "$$authority_parent" "$$authority" "$$reopen" "$$root" "$$tests" "$$support" "$$matrix" "$$races" "$$storage" "$$restart" "$$record_binding" misc/make/startup-candidate-preserve-persistence-tests.mk Makefile; do \
		timeout 10s test "$$( timeout 10s wc -l < "$$file" )" -le 1000; \
	done; \
	timeout 1200s $(CARGO) test -p forge --lib "$$prefix" -- --test-threads=1
