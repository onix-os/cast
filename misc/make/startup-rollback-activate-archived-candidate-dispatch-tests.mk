.PHONY: forge-startup-usr-rollback-activate-archived-candidate-dispatch-test

forge-startup-usr-rollback-activate-archived-candidate-dispatch-test:
	@set -euo pipefail; \
	timeout 10s mkdir -p "$(TOP_DIR)/target"; \
	listed="$$( timeout 10s mktemp "$(TOP_DIR)/target/activate-archived-candidate-dispatch-list.XXXXXXXXXXXX" )"; \
	trap 'timeout 10s rm -f "$$listed"' EXIT; \
	timeout 300s $(CARGO) test -p forge --lib -- --list | timeout 300s tee "$$listed" >/dev/null; \
	persistence_prefix='client::startup_recovery::usr_rollback_activate_archived_candidate_preserve_persistence::tests::'; \
	startup_prefix='client::startup_gate::usr_rollback_activate_archived::tests::'; \
	timeout 10s test "$$( timeout 10s grep -c "^$$persistence_prefix"'.*: test$$' "$$listed" )" = 14; \
	for name in \
		matrix::startup_archived_candidate_preserve_persistence_applied_matrix_persists_exact_successor \
		matrix::startup_archived_candidate_preserve_persistence_finish_matrix_persists_exact_successor \
		matrix::startup_archived_candidate_preserve_persistence_changes_only_journal_after_durability \
		evidence_races::startup_archived_candidate_preserve_persistence_rejects_reopened_and_cross_root_journals \
		evidence_races::startup_archived_candidate_preserve_persistence_final_evidence_races_fail_before_advance \
		storage_reopen::startup_archived_candidate_preserve_persistence_faults_reopen_exact_source_or_successor \
		storage_reopen::startup_archived_candidate_preserve_persistence_consumes_old_store_and_reopens_success \
		restart::startup_archived_candidate_preserve_source_fault_restart_finishes_without_second_move \
		restart::startup_archived_candidate_preserve_successor_fault_restart_skips_preservation \
		record_binding::startup_archived_candidate_preserve_bound_advance_same_byte_replacements_never_succeed \
		record_binding::startup_archived_candidate_preserve_same_byte_successor_replacement_after_publication_fails_exact_binding \
		record_binding::startup_archived_candidate_preserve_same_byte_successor_replacement_after_same_store_binding_fails_reopened_binding \
		production_dispatch::startup_activate_archived_candidate_preserve_production_leaf_dispatches_all_exact_cases_once \
		production_dispatch::startup_activate_archived_candidate_preserve_production_leaf_rejects_cross_operation_pairing; do \
		timeout 10s grep -Fqx "$$persistence_prefix$$name: test" "$$listed"; \
	done; \
	for name in \
		candidate_admission::startup_activate_archived_candidate_child_handles_only_exact_operation_and_phase \
		candidate_admission::startup_activate_archived_candidate_child_excludes_other_operations_and_phases_without_effects \
		candidate_evidence_races::startup_activate_archived_candidate_dispatch_rejects_every_final_evidence_race \
		candidate_matrix::startup_activate_archived_candidate_dispatch_applied_matrix_moves_once_and_returns_pending \
		candidate_matrix::startup_activate_archived_candidate_dispatch_finish_matrix_never_moves_and_returns_pending \
		candidate_matrix::startup_activate_archived_candidate_dispatch_never_falls_through_to_completion_in_same_entry \
		candidate_move_process_kill::startup_activate_archived_candidate_move_process_kill_recovers_without_second_move \
		candidate_restart::startup_activate_archived_candidate_source_fault_fresh_entry_finishes_without_second_move \
		candidate_restart::startup_activate_archived_candidate_successor_fault_fresh_entry_completes_without_second_move; do \
		timeout 10s grep -Fqx "$$startup_prefix$$name: test" "$$listed"; \
	done; \
	gate=crates/forge/src/client/startup_gate.rs; \
	child=crates/forge/src/client/startup_gate/usr_rollback_activate_archived.rs; \
	reconciliation=crates/forge/src/client/startup_reconciliation.rs; \
	authority=crates/forge/src/client/startup_reconciliation/usr_rollback_candidate_preserve_authority.rs; \
	archived_authority=crates/forge/src/client/startup_reconciliation/usr_rollback_candidate_preserve_authority/archived_effect.rs; \
	archived_persistence_authority=crates/forge/src/client/startup_reconciliation/usr_rollback_candidate_preserve_authority/archived_effect/persistence.rs; \
	production_leaf=crates/forge/src/client/startup_recovery/usr_rollback_candidate_preserve_dispatch.rs; \
	recovery=crates/forge/src/client/startup_recovery.rs; \
	persistence=crates/forge/src/client/startup_recovery/usr_rollback_activate_archived_candidate_preserve_persistence.rs; \
	reopen=crates/forge/src/client/startup_recovery/canonical_journal_reopen.rs; \
	persistence_tests=crates/forge/src/client/startup_recovery/usr_rollback_activate_archived_candidate_preserve_persistence/tests; \
	record_binding="$$persistence_tests/record_binding.rs"; \
	storage="$$persistence_tests/storage_reopen.rs"; \
	restart="$$persistence_tests/restart.rs"; \
	startup_tests=crates/forge/src/client/startup_gate/usr_rollback_activate_archived/tests; \
	process_kill="$$startup_tests/candidate_move_process_kill.rs"; \
	process_kill_boundaries="$$startup_tests/candidate_process_kill_boundaries.rs"; \
	timeout 10s grep -Fqx 'mod usr_rollback_activate_archived;' "$$gate"; \
	if timeout 10s awk 'previous == "#[cfg(test)]" && $$0 == "mod usr_rollback_activate_archived;" { found = 1 } { previous = $$0 } END { exit !found }' "$$gate"; then exit 1; fi; \
	timeout 10s grep -Fq 'record.operation != Operation::ActivateArchived' "$$child"; \
	timeout 10s grep -Fqx '        Phase::CandidatePreserveIntent => {' "$$child"; \
	timeout 10s grep -Fq 'UsrRollbackCandidatePreserveAdmission::Apply(authority)' "$$child"; \
	timeout 10s grep -Fq 'UsrRollbackCandidatePreserveAdmission::Finish(authority)' "$$child"; \
	timeout 10s grep -Fq 'dispatch_usr_rollback_candidate_preserve_and_reopen(journal, record, ready)' "$$child"; \
	candidate_arm="$$( timeout 10s sed -n '/Phase::CandidatePreserveIntent => {/,/Phase::CandidatePreserved => {/p' "$$child" | timeout 10s sed '$$d' )"; \
	if timeout 10s rg -n 'UsrRollbackActivateArchivedCompleteRouteAuthority::capture|persist_usr_rollback_activate_archived_complete_route_and_reopen|Phase::RollbackComplete|finalize_usr_rollback' <<<"$$candidate_arm"; then exit 1; else status="$$?"; timeout 10s test "$$status" = 1; fi; \
	archived_line="$$( timeout 10s grep -nF 'match usr_rollback_activate_archived::dispatch(' "$$gate" | timeout 10s cut -d: -f1 )"; \
	active_line="$$( timeout 10s grep -nF 'match usr_rollback_active_reblit::dispatch(' "$$gate" | timeout 10s cut -d: -f1 )"; \
	new_state_line="$$( timeout 10s grep -nF 'match usr_rollback_new_state::dispatch(' "$$gate" | timeout 10s cut -d: -f1 )"; \
	timeout 10s test "$$archived_line" -lt "$$active_line"; \
	timeout 10s test "$$active_line" -lt "$$new_state_line"; \
	handled="$$( timeout 10s sed -n '/usr_rollback_activate_archived::Dispatch::Handled/,/^                }/p' "$$gate" )"; \
	timeout 10s grep -Fq 'PendingSystemTransition::inspect' <<<"$$handled"; \
	timeout 10s grep -Fq 'return Err(Error::RecoveryPending(pending));' <<<"$$handled"; \
	timeout 10s grep -Fqx 'mod usr_rollback_activate_archived_candidate_preserve_persistence;' "$$recovery"; \
	if timeout 10s awk 'previous == "#[cfg(test)]" && $$0 == "mod usr_rollback_activate_archived_candidate_preserve_persistence;" { found = 1 } { previous = $$0 } END { exit !found }' "$$recovery"; then exit 1; fi; \
	timeout 10s grep -Fq 'UsrRollbackCandidatePreserveApplyEffectSelection::MoveArchived' "$$production_leaf" "$$authority"; \
	timeout 10s grep -Fq 'UsrRollbackCandidatePreserveFinishDurabilitySelection::Archived' "$$production_leaf" "$$authority"; \
	timeout 10s grep -Fq 'persist_usr_rollback_archived_candidate_preserve_and_reopen(journal, durable)' "$$production_leaf"; \
	if timeout 10s rg -n 'RollbackActionOutcome|rollback_successor\(' "$$persistence"; then exit 1; else status="$$?"; timeout 10s test "$$status" = 1; fi; \
	timeout 10s test "$$( timeout 10s grep -Fc 'authority.revalidate(&journal)' "$$persistence" )" = 1; \
	if timeout 10s rg -n 'candidate_preserved_successor\(' "$$persistence" "$$archived_persistence_authority"; then exit 1; else status="$$?"; timeout 10s test "$$status" = 1; fi; \
	timeout 10s grep -Fq '.rollback_successor(Some(outcome))' "$$archived_persistence_authority"; \
	if timeout 10s rg -U -n 'fn[^\(]*\([^\)]*(origin|outcome)[^\)]*\)' "$$persistence" "$$archived_persistence_authority"; then exit 1; else status="$$?"; timeout 10s test "$$status" = 1; fi; \
	if timeout 10s rg -n '\.advance\(' "$$persistence" "$$archived_persistence_authority" "$$reopen"; then exit 1; else status="$$?"; timeout 10s test "$$status" = 1; fi; \
	timeout 10s grep -Fqx '    let advance = match authority.advance_candidate_preserved_record_binding(&journal) {' "$$persistence"; \
	timeout 10s grep -Fq 'pub(in crate::client) fn advance_candidate_preserved_record_binding(' "$$archived_persistence_authority"; \
	timeout 10s test "$$( timeout 10s grep -Fc 'advance_candidate_preserved_record_binding' "$$persistence" )" = 1; \
	timeout 10s test "$$( timeout 10s grep -Fc 'advance_candidate_preserved_record_binding' "$$archived_persistence_authority" )" = 1; \
	timeout 10s test "$$( timeout 10s grep -Fc 'journal.advance_record_binding(cast, self.effect.journal_record_binding, &successor)' "$$archived_persistence_authority" )" = 1; \
	timeout 10s grep -Fqx '            let (successor, successor_binding) = published.into_parts();' "$$persistence"; \
	timeout 10s test "$$( timeout 10s grep -Fc 'published.into_parts()' "$$persistence" )" = 1; \
	first_revalidate_line="$$( timeout 10s grep -nF 'authority.revalidate(&journal)' "$$persistence" | timeout 10s head -n 1 | timeout 10s cut -d: -f1 )"; \
	seam_line="$$( timeout 10s grep -nF '    before_usr_rollback_archived_candidate_preserve_persistence_final_revalidation();' "$$persistence" | timeout 10s cut -d: -f1 )"; \
	clone_line="$$( timeout 10s grep -nF '    let installation = authority.installation().clone();' "$$persistence" | timeout 10s cut -d: -f1 )"; \
	advance_line="$$( timeout 10s grep -nF '    let advance = match authority.advance_candidate_preserved_record_binding(&journal) {' "$$persistence" | timeout 10s cut -d: -f1 )"; \
	same_store_line="$$( timeout 10s grep -nF '            let exact = revalidate_published_archived_candidate_preserved_binding(' "$$persistence" | timeout 10s cut -d: -f1 )"; \
	drop_journal_line="$$( timeout 10s grep -nF '    drop(journal);' "$$persistence" | timeout 10s tail -n 1 | timeout 10s cut -d: -f1 )"; \
	reopen_seam_line="$$( timeout 10s grep -nF '        after_usr_rollback_archived_candidate_preserve_successor_binding_check_before_reopen();' "$$persistence" | timeout 10s cut -d: -f1 )"; \
	reopen_line="$$( timeout 10s grep -nF '        reopen_canonical_journal(&installation).map_err(UsrRollbackArchivedCandidatePreserveReopenError::from);' "$$persistence" | timeout 10s cut -d: -f1 )"; \
	timeout 10s test "$$first_revalidate_line" -lt "$$seam_line"; \
	timeout 10s test "$$seam_line" -lt "$$clone_line"; \
	timeout 10s test "$$clone_line" -lt "$$advance_line"; \
	timeout 10s test "$$advance_line" -lt "$$same_store_line"; \
	timeout 10s test "$$same_store_line" -lt "$$drop_journal_line"; \
	timeout 10s test "$$drop_journal_line" -lt "$$reopen_seam_line"; \
	timeout 10s test "$$reopen_seam_line" -lt "$$reopen_line"; \
	suffix="$$( timeout 10s sed -n '/    let advance = match authority.advance_candidate_preserved_record_binding/,/        reopen_canonical_journal(&installation)/p' "$$persistence" )"; \
	if timeout 10s grep -Fq 'drop(authority)' <<<"$$suffix"; then exit 1; else status="$$?"; timeout 10s test "$$status" = 1; fi; \
	timeout 10s test "$$( timeout 10s grep -Fc 'reopen_canonical_journal(&installation)' <<<"$$suffix" )" = 1; \
	published_branch="$$( timeout 10s sed -n '/        Ok(published) => {/,/        Err(UsrRollbackArchivedCandidatePreserveRecordAdvanceError::Authority(source)) => {/p' "$$persistence" )"; \
	timeout 10s test "$$( timeout 10s grep -Fc '        Ok(published) => {' <<<"$$published_branch" )" = 1; \
	timeout 10s test "$$( timeout 10s grep -Fc 'UsrRollbackArchivedCandidatePreserveAdvanceOutcome::Published {' <<<"$$published_branch" )" = 1; \
	timeout 10s test "$$( timeout 10s grep -Fc 'UsrRollbackArchivedCandidatePreserveAdvanceOutcome::SuccessorBindingFailed {' <<<"$$published_branch" )" = 2; \
	if timeout 10s rg -n '(^|[^[:alnum:]_])return([^[:alnum:]_]|$$)|\?|panic!|unreachable!|\.unwrap\(|\.expect\(' <<<"$$published_branch"; then exit 1; else status="$$?"; timeout 10s test "$$status" = 1; fi; \
	if timeout 10s rg -n 'open_in_retained_cast|journal\.load\(' "$$persistence"; then exit 1; else status="$$?"; timeout 10s test "$$status" = 1; fi; \
	timeout 10s test "$$( timeout 10s grep -Fc '.has_record_binding(cast, successor_binding, successor)' "$$persistence" )" = 1; \
	timeout 10s test "$$( timeout 10s grep -Fc '.has_reopened_record_binding(cast, successor_binding, successor)' "$$persistence" )" = 1; \
	published_binding_helper="$$( timeout 10s sed -n '/^fn revalidate_published_archived_candidate_preserved_binding(/,/^}/p' "$$persistence" )"; \
	timeout 10s test "$$( timeout 10s grep -Fc '.revalidate_mutable_namespace()' <<<"$$published_binding_helper" )" = 2; \
	timeout 10s grep -Fq '.has_record_binding(cast, successor_binding, successor)' <<<"$$published_binding_helper"; \
	reopened_binding_helper="$$( timeout 10s sed -n '/^fn revalidate_reopened_archived_candidate_preserved_binding(/,/^}/p' "$$persistence" )"; \
	timeout 10s test "$$( timeout 10s grep -Fc '.revalidate_mutable_namespace()' <<<"$$reopened_binding_helper" )" = 2; \
	timeout 10s grep -Fq '.has_reopened_record_binding(cast, successor_binding, successor)' <<<"$$reopened_binding_helper"; \
	timeout 10s test "$$( timeout 10s grep -Fc 'reopen_canonical_journal(&installation)' "$$persistence" )" = 1; \
	timeout 10s grep -Fqx '                                durable: DurableUsrRollbackArchivedCandidatePreserveRecord::CandidatePreserved,' "$$persistence"; \
	timeout 10s test "$$( timeout 10s grep -Fc '            Ok((reopened, Some(actual))) if actual == source_record => {' "$$persistence" )" = 2; \
	timeout 10s test "$$( timeout 10s grep -Fc '            Ok((reopened, Some(actual))) if actual == successor => {' "$$persistence" )" = 3; \
	timeout 10s grep -Fqx '                    Ok(true) => Ok((reopened, successor)),' "$$persistence"; \
	timeout 10s grep -Fq 'ArchivedDurabilityOrigin::Applied' "$$archived_authority"; \
	timeout 10s grep -Fq 'ArchivedDurabilityOrigin::AlreadySatisfied' "$$archived_authority"; \
	if timeout 10s rg -n 'pub[^\n]*(ArchivedDurabilityOrigin|origin)' "$$archived_authority" "$$archived_persistence_authority"; then exit 1; else status="$$?"; timeout 10s test "$$status" = 1; fi; \
	production_code="$$( timeout 10s sed -E 's,//.*$$,,' "$$child" "$$persistence" "$$archived_authority" "$$archived_persistence_authority" )"; \
	if timeout 10s rg -n '^[[:space:]]*(loop|while)[[:space:]]|=[[:space:]]*(loop|while)[[:space:]]|retry' <<<"$$production_code"; then exit 1; else status="$$?"; timeout 10s test "$$status" = 1; fi; \
	for race in Database Provenance Journal Installation Namespace Plan; do timeout 10s grep -Fq "EvidenceRace::$$race" "$$persistence_tests/evidence_races.rs" "$$startup_tests/candidate_evidence_races.rs"; done; \
	for fault in temporary_sync update_exchange update_first_directory_sync displaced_unlink update_final_directory_sync; do timeout 10s grep -Fq "$$fault" "$$storage"; done; \
	timeout 10s test "$$( timeout 10s rg -n '^            arm_next_(temporary_sync|update_exchange|update_first_directory_sync|displaced_unlink|update_final_directory_sync)_fault,$$' "$$storage" | timeout 10s wc -l )" = 5; \
	for axis in 'for epoch in Epoch::ALL {' 'for origin in CandidateOrigin::ALL {' 'for source in CandidateSource::ALL {' 'for usr_outcome in [RollbackActionOutcome::Applied, RollbackActionOutcome::AlreadySatisfied] {'; do timeout 10s grep -Fq "$$axis" "$$storage"; done; \
	timeout 10s test "$$( timeout 10s grep -Fc 'assert_eq!(fixture.fixture.database_snapshot(), database_before);' "$$storage" )" = 2; \
	timeout 10s test "$$( timeout 10s grep -Fc 'assert_eq!(non_journal_namespace_snapshot(&fixture), namespace_before);' "$$storage" )" = 2; \
	timeout 10s test "$$( timeout 10s grep -Fc 'assert_eq!(archived_candidate_preserve_move_attempt_count(), effect_count_before);' "$$storage" )" = 2; \
	timeout 10s grep -Fqx 'mod record_binding;' "$$persistence_tests.rs"; \
	for seam in BeforeBoundAdvancePublish BeforeBoundAdvanceFinalBinding; do timeout 10s test "$$( timeout 10s grep -Fc "PublicBindingRevalidationBoundary::$$seam" "$$record_binding" )" = 1; done; \
	timeout 10s test "$$( timeout 10s grep -Fc 'arm_public_binding_revalidation_callback(boundary, hook);' "$$record_binding" )" = 1; \
	timeout 10s test "$$( timeout 10s grep -Fc 'arm_before_usr_rollback_archived_candidate_preserve_successor_binding_revalidation(hook);' "$$record_binding" )" = 1; \
	timeout 10s test "$$( timeout 10s grep -Fc 'arm_after_usr_rollback_archived_candidate_preserve_successor_binding_check_before_reopen(hook);' "$$record_binding" )" = 1; \
	for axis in 'for epoch in Epoch::ALL {' 'for source in CandidateSource::ALL {' 'for origin in CandidateOrigin::ALL {' 'for usr_outcome in [RollbackActionOutcome::Applied, RollbackActionOutcome::AlreadySatisfied] {'; do timeout 10s test "$$( timeout 10s grep -Fc "$$axis" "$$record_binding" )" = 3; done; \
	timeout 10s grep -Fq 'assert_ne!(retained_identity, inode_identity(&canonical));' "$$record_binding"; \
	timeout 10s test "$$( timeout 10s grep -Fc 'assert_unchanged_outside_journal(' "$$record_binding" )" = 4; \
	timeout 10s grep -Fq 'archived_candidate_preserve_move_attempt_count()' "$$record_binding"; \
	timeout 10s grep -Fq 'assert_eq!(names.len(), 2, "bound update left journal residue: {names:?}");' "$$record_binding"; \
	for axis in 'for epoch in Epoch::ALL {' 'for source in CandidateSource::ALL {' 'for usr_outcome in [RollbackActionOutcome::Applied, RollbackActionOutcome::AlreadySatisfied] {'; do timeout 10s test "$$( timeout 10s grep -Fc "$$axis" "$$restart" )" = 2; done; \
	timeout 10s test "$$( timeout 10s rg -n 'for (first_)?origin in CandidateOrigin::ALL \{' "$$restart" | timeout 10s wc -l )" = 2; \
	timeout 10s test "$$( timeout 10s grep -Fc 'drop(reservation);' "$$restart" )" = 4; \
	timeout 10s test "$$( timeout 10s grep -Fc 'ActiveStateReservation::acquire().unwrap();' "$$restart" )" = 4; \
	timeout 10s grep -Fq 'expected_post_events(&fixture)' "$$restart"; \
	timeout 10s grep -Fqx 'mod candidate_move_process_kill;' "$$startup_tests/mod.rs"; \
	timeout 10s grep -Fqx 'mod candidate_process_kill_boundaries;' "$$startup_tests/mod.rs"; \
	timeout 10s grep -Fq 'for epoch in Epoch::ALL {' "$$process_kill"; \
	timeout 10s grep -Fq 'for source in CandidateSource::ALL {' "$$process_kill"; \
	timeout 10s grep -Fq 'for boundary in CandidateProcessKillBoundary::ALL {' "$$process_kill"; \
	timeout 10s grep -Fq 'cases, 28,' "$$process_kill"; \
	for boundary in PostMovePreRecapture BeforeCandidateSync BeforeStagingParentSync BeforeTargetParentSync BeforeRootsParentSync BeforeFinalPostCapture BeforeDurablePostRevalidation; do timeout 10s grep -Fq "Self::$$boundary" "$$process_kill_boundaries"; done; \
	for hook in arm_before_archived_candidate_preserve_move_reconciliation_capture arm_before_archived_candidate_preserve_post_candidate_sync arm_before_archived_candidate_preserve_post_staging_parent_sync arm_before_archived_candidate_preserve_post_target_parent_sync arm_before_archived_candidate_preserve_post_roots_parent_sync arm_before_archived_candidate_preserve_post_final_capture arm_before_archived_candidate_preserve_durable_post_revalidation_capture; do timeout 10s grep -Fq "$$hook" "$$process_kill_boundaries"; done; \
	timeout 10s grep -Fq 'kill_after_real_candidate_move' "$$process_kill"; \
	timeout 10s grep -Fq 'nix::libc::kill(nix::libc::getpid(), nix::libc::SIGKILL)' "$$process_kill"; \
	timeout 10s grep -Fq 'crash_status.signal()' "$$process_kill"; \
	timeout 10s grep -Fq 'Some(nix::libc::SIGKILL)' "$$process_kill"; \
	timeout 10s grep -Fq 'Command::new(env::current_exe().unwrap())' "$$process_kill"; \
	timeout 10s grep -Fq 'const CHILD_DEADLINE: Duration = Duration::from_secs(15);' "$$process_kill"; \
	timeout 10s grep -Fq 'let _ = child.kill();' "$$process_kill"; \
	timeout 10s grep -Fq 'let status = child.wait().unwrap();' "$$process_kill"; \
	timeout 10s test "$$( timeout 10s grep -Fc 'CleanSystemStartup::enter(' "$$process_kill" )" = 2; \
	timeout 10s grep -Fq 'Installation::open(&case.root, None)' "$$process_kill"; \
	timeout 10s grep -Fq 'open_state_database(&installation)' "$$process_kill"; \
	timeout 10s grep -Fq 'snapshot_startup_recovery_namespace(&root)' "$$process_kill"; \
	timeout 10s grep -Fq 'snapshot_startup_recovery_namespace(&case.root)' "$$process_kill"; \
	timeout 10s grep -Fq 'RollbackActionOutcome::AlreadySatisfied' "$$process_kill"; \
	timeout 10s grep -Fq 'archived_candidate_preserve_move_attempt_count(), 0' "$$process_kill"; \
	timeout 10s grep -Fq 'same-boot process death only' "$$process_kill"; \
	timeout 10s grep -Fq 'power-loss oracle' "$$process_kill"; \
	timeout 10s grep -Fq 'historical record epoch is not a reboot' "$$process_kill"; \
	if timeout 10s rg -n 'dispatch_usr_rollback_candidate_preserve_and_reopen|persist_usr_rollback|journal\.(advance|delete)|reconcile_move|arm_archived_candidate_preserve_move_fault|arm_next_|StorageFault' "$$process_kill" "$$process_kill_boundaries"; then exit 1; else status="$$?"; timeout 10s test "$$status" = 1; fi; \
	for file in "$$gate" "$$child" "$$reconciliation" "$$authority" "$$archived_authority" "$$archived_persistence_authority" "$$production_leaf" "$$recovery" "$$reopen" "$$persistence" "$$persistence_tests"/*.rs "$$startup_tests"/*.rs misc/make/startup-rollback-activate-archived-candidate-dispatch-tests.mk Makefile misc/make/help.mk; do \
		timeout 10s test "$$( timeout 10s wc -l < "$$file" )" -le 1000; \
	done; \
	timeout 1800s $(CARGO) test -p forge --lib "$$persistence_prefix" -- --test-threads=1; \
	timeout 1800s $(CARGO) test -p forge --lib startup_activate_archived_candidate -- --test-threads=1
