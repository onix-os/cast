.PHONY: forge-startup-usr-rollback-fresh-db-invalidation-persistence-test

forge-startup-usr-rollback-fresh-db-invalidation-persistence-test:
	@set -euo pipefail; \
	timeout 10s mkdir -p "$(TOP_DIR)/target"; \
	listed="$$( timeout 10s mktemp "$(TOP_DIR)/target/fresh-db-invalidation-persistence-list.XXXXXXXXXXXX" )"; \
	production_code="$$( timeout 10s mktemp "$(TOP_DIR)/target/fresh-db-invalidation-persistence-code.XXXXXXXXXXXX" )"; \
	symbol_refs="$$( timeout 10s mktemp "$(TOP_DIR)/target/fresh-db-invalidation-persistence-symbols.XXXXXXXXXXXX" )"; \
	revalidate_body="$$( timeout 10s mktemp "$(TOP_DIR)/target/fresh-db-invalidation-persistence-revalidate.XXXXXXXXXXXX" )"; \
	trap 'timeout 10s rm -f "$$listed" "$$production_code" "$$symbol_refs" "$$revalidate_body"' EXIT; \
	timeout 300s $(CARGO) test -p forge --lib -- --list | timeout 30s tee "$$listed" >/dev/null; \
	timeout 10s grep -q . "$$listed"; \
	prefix='client::startup_recovery::usr_rollback_fresh_db_invalidation_persistence::tests::'; \
	count="$$( timeout 10s awk -v prefix="$$prefix" 'index($$0, prefix) == 1 && $$0 ~ /: test$$/ { count += 1 } END { print count + 0 }' "$$listed" )"; \
	timeout 10s test "$$count" = 9; \
	for name in \
		matrix::startup_fresh_db_invalidation_persistence_applied_matrix_persists_exact_owned_successor \
		matrix::startup_fresh_db_invalidation_persistence_finish_matrix_persists_exact_owned_successor \
		matrix::startup_fresh_db_invalidation_persistence_changes_only_the_canonical_journal \
		races::startup_fresh_db_invalidation_persistence_rejects_reopened_and_cross_root_journals \
		races::startup_fresh_db_invalidation_persistence_final_races_fail_before_advance \
		storage_reopen::startup_fresh_db_invalidation_persistence_faults_reopen_exact_intent_or_invalidated_record \
		storage_reopen::startup_fresh_db_invalidation_persistence_consumes_old_store_and_reopens_exact_success \
		restart::startup_fresh_db_invalidation_persistence_source_fault_restart_uses_zero_removal_finish \
		restart::startup_fresh_db_invalidation_persistence_successor_fault_restart_is_not_applicable; do \
		timeout 10s grep -Fqx "$$prefix$$name: test" "$$listed"; \
	done; \
	executor=crates/forge/src/client/startup_recovery/usr_rollback_fresh_db_invalidation_persistence.rs; \
	persistence_authority=crates/forge/src/client/startup_reconciliation/usr_rollback_fresh_db_invalidation_authority/effect_reconciliation/persistence.rs; \
	effect=crates/forge/src/client/startup_reconciliation/usr_rollback_fresh_db_invalidation_authority/effect_reconciliation.rs; \
	authority=crates/forge/src/client/startup_reconciliation/usr_rollback_fresh_db_invalidation_authority.rs; \
	exact=crates/forge/src/db/state/exact_fresh_transition_removal.rs; \
	reopen=crates/forge/src/client/startup_recovery/canonical_journal_reopen.rs; \
	recovery_root=crates/forge/src/client/startup_recovery.rs; \
	reconciliation_root=crates/forge/src/client/startup_reconciliation.rs; \
	tests=crates/forge/src/client/startup_recovery/usr_rollback_fresh_db_invalidation_persistence/tests.rs; \
	support=crates/forge/src/client/startup_recovery/usr_rollback_fresh_db_invalidation_persistence/tests/support.rs; \
	matrix=crates/forge/src/client/startup_recovery/usr_rollback_fresh_db_invalidation_persistence/tests/matrix.rs; \
	races=crates/forge/src/client/startup_recovery/usr_rollback_fresh_db_invalidation_persistence/tests/races.rs; \
	storage=crates/forge/src/client/startup_recovery/usr_rollback_fresh_db_invalidation_persistence/tests/storage_reopen.rs; \
	restart=crates/forge/src/client/startup_recovery/usr_rollback_fresh_db_invalidation_persistence/tests/restart.rs; \
	timeout 10s grep -Fqx 'mod usr_rollback_fresh_db_invalidation_persistence;' "$$recovery_root"; \
	timeout 10s test "$$( timeout 10s grep -Fc 'mod usr_rollback_fresh_db_invalidation_persistence;' "$$recovery_root" )" = 1; \
	timeout 10s grep -Fqx 'mod persistence;' "$$effect"; \
	timeout 10s test "$$( timeout 10s grep -Fc 'mod persistence;' "$$effect" )" = 1; \
	for module in matrix races restart storage_reopen support; do \
		timeout 10s grep -Fqx "mod $$module;" "$$tests"; \
	done; \
	timeout 10s test "$$( timeout 10s grep -Ec '^mod [a-z_]+;' "$$tests" )" = 5; \
	timeout 10s rg -U -q '^pub\(in crate::client\) fn persist_usr_rollback_fresh_db_invalidation_and_reopen\(\n    journal: TransitionJournalStore,\n    authority: UsrRollbackFreshDbInvalidationEffectAuthority<'\''_>,\n\) -> Result<\(TransitionJournalStore, TransitionRecord\), UsrRollbackFreshDbInvalidationPersistenceError> \{' "$$executor"; \
	timeout 10s grep -Fqx '    persist_usr_rollback_fresh_db_invalidation_and_reopen,' "$$recovery_root"; \
	timeout 10s rg -n -F 'persist_usr_rollback_fresh_db_invalidation_and_reopen' crates/forge/src/client --glob '*.rs' --glob '!**/tests/**' --glob '!**/tests.rs' --glob '!**/usr_rollback_fresh_db_invalidation_persistence.rs' > "$$symbol_refs"; \
	timeout 10s test "$$( timeout 10s wc -l < "$$symbol_refs" )" = 1; \
	timeout 10s test "$$( timeout 10s cut -d: -f1 "$$symbol_refs" )" = "$$recovery_root"; \
	timeout 10s test "$$( timeout 10s cut -d: -f3- "$$symbol_refs" )" = '    persist_usr_rollback_fresh_db_invalidation_and_reopen,'; \
	timeout 10s test "$$( timeout 10s grep -Fc 'persist_usr_rollback_fresh_db_invalidation_and_reopen(' "$$executor" )" = 1; \
	timeout 10s test "$$( timeout 10s grep -Fc 'authority.revalidate(&journal)' "$$executor" )" = 2; \
	timeout 10s test "$$( timeout 10s grep -Fc 'authority.record()' "$$executor" )" = 1; \
	timeout 10s test "$$( timeout 10s grep -Fc 'authority.fresh_db_invalidated_successor()' "$$executor" )" = 1; \
	timeout 10s test "$$( timeout 10s grep -Fc 'before_usr_rollback_fresh_db_invalidation_persistence_final_revalidation();' "$$executor" )" = 1; \
	timeout 10s test "$$( timeout 10s grep -Fc 'authority.installation()' "$$executor" )" = 1; \
	timeout 10s test "$$( timeout 10s grep -Fc 'journal.advance(&source_record, &successor)' "$$executor" )" = 1; \
	timeout 10s test "$$( timeout 10s grep -Fc 'drop(authority);' "$$executor" )" = 5; \
	timeout 10s test "$$( timeout 10s grep -Fc 'drop(journal);' "$$executor" )" = 5; \
	timeout 10s test "$$( timeout 10s grep -Fc 'reopen_canonical_journal(&installation)' "$$executor" )" = 1; \
	timeout 10s grep -Fqx '    let source_record = authority.record().clone();' "$$executor"; \
	timeout 10s grep -Fqx '    let successor = match authority.fresh_db_invalidated_successor() {' "$$executor"; \
	timeout 10s grep -Fqx '        Ok(successor) if successor.phase == Phase::FreshDbInvalidated => successor,' "$$executor"; \
	timeout 10s grep -Fqx '    let installation = authority.installation().clone();' "$$executor"; \
	timeout 10s grep -Fqx '    let advance = journal.advance(&source_record, &successor);' "$$executor"; \
	first_revalidate_line="$$( timeout 10s grep -nF 'authority.revalidate(&journal)' "$$executor" | timeout 10s head -n 1 | timeout 10s cut -d: -f1 )"; \
	source_line="$$( timeout 10s grep -nF '    let source_record = authority.record().clone();' "$$executor" | timeout 10s cut -d: -f1 )"; \
	successor_line="$$( timeout 10s grep -nF '    let successor = match authority.fresh_db_invalidated_successor() {' "$$executor" | timeout 10s cut -d: -f1 )"; \
	seam_line="$$( timeout 10s grep -nF '    before_usr_rollback_fresh_db_invalidation_persistence_final_revalidation();' "$$executor" | timeout 10s cut -d: -f1 )"; \
	final_revalidate_line="$$( timeout 10s grep -nF 'authority.revalidate(&journal)' "$$executor" | timeout 10s tail -n 1 | timeout 10s cut -d: -f1 )"; \
	installation_line="$$( timeout 10s grep -nF '    let installation = authority.installation().clone();' "$$executor" | timeout 10s cut -d: -f1 )"; \
	advance_line="$$( timeout 10s grep -nF '    let advance = journal.advance(&source_record, &successor);' "$$executor" | timeout 10s cut -d: -f1 )"; \
	drop_authority_line="$$( timeout 10s grep -nF '    drop(authority);' "$$executor" | timeout 10s tail -n 1 | timeout 10s cut -d: -f1 )"; \
	drop_journal_line="$$( timeout 10s grep -nF '    drop(journal);' "$$executor" | timeout 10s tail -n 1 | timeout 10s cut -d: -f1 )"; \
	reopen_line="$$( timeout 10s grep -nF 'reopen_canonical_journal(&installation)' "$$executor" | timeout 10s cut -d: -f1 )"; \
	timeout 10s test "$$first_revalidate_line" -lt "$$source_line"; \
	timeout 10s test "$$source_line" -lt "$$successor_line"; \
	timeout 10s test "$$successor_line" -lt "$$seam_line"; \
	timeout 10s test "$$seam_line" -lt "$$final_revalidate_line"; \
	timeout 10s test "$$final_revalidate_line" -lt "$$installation_line"; \
	timeout 10s test "$$installation_line" -lt "$$advance_line"; \
	timeout 10s test "$$advance_line" -lt "$$drop_authority_line"; \
	timeout 10s test "$$drop_authority_line" -lt "$$drop_journal_line"; \
	timeout 10s test "$$drop_journal_line" -lt "$$reopen_line"; \
	timeout 10s grep -Fqx '    origin: RollbackActionOutcome,' "$$effect"; \
	timeout 10s test "$$( timeout 10s grep -Fc 'origin: RollbackActionOutcome::Applied,' "$$effect" )" = 1; \
	timeout 10s test "$$( timeout 10s grep -Fc 'origin: RollbackActionOutcome::AlreadySatisfied,' "$$effect" )" = 1; \
	timeout 10s test "$$( timeout 10s grep -Ec '^    pub\(in crate::client\) fn ' "$$persistence_authority" )" = 4; \
	timeout 10s grep -Fqx '    pub(in crate::client) fn fresh_db_invalidated_successor(&self) -> Result<TransitionRecord, CodecError> {' "$$persistence_authority"; \
	timeout 10s grep -Fqx '        self.record.rollback_successor(Some(self.origin))' "$$persistence_authority"; \
	timeout 10s sed -n '/^    pub(in crate::client) fn revalidate(/,/^    }/p' "$$persistence_authority" > "$$revalidate_body"; \
	timeout 10s grep -q . "$$revalidate_body"; \
	timeout 10s test "$$( timeout 10s grep -Fc 'journal.has_binding(&self.journal_binding)' "$$revalidate_body" )" = 1; \
	timeout 10s test "$$( timeout 10s grep -Fc 'FreshDbInvalidationDatabaseKind::' "$$revalidate_body" )" = 2; \
	timeout 10s test "$$( timeout 10s grep -Fc 'FreshDbInvalidationDatabaseKind::JointlyAbsent' "$$revalidate_body" )" = 2; \
	timeout 10s test "$$( timeout 10s grep -Fc 'inspect_current_database(&self.record, &self.state_db)?' "$$revalidate_body" )" = 2; \
	timeout 10s test "$$( timeout 10s grep -Fc 'require_exact_database(' "$$revalidate_body" )" = 2; \
	timeout 10s test "$$( timeout 10s rg -U --count-matches 'require_exact_database\([[:space:]]*&self\.database,[[:space:]]*inspect_current_database\(&self\.record, &self\.state_db\)\?[[:space:]]*\)' "$$revalidate_body" )" = 2; \
	timeout 10s test "$$( timeout 10s grep -Fc '.revalidate(&self.installation, journal, &self.record)?;' "$$revalidate_body" )" = 1; \
	timeout 10s test "$$( timeout 10s grep -Fc 'fresh_db_invalidation_plan_is_exact(&self.record)' "$$revalidate_body" )" = 1; \
	timeout 10s test "$$( timeout 10s grep -Fc 'self.installation.revalidate_mutable_namespace()?;' "$$revalidate_body" )" = 2; \
	binding_line="$$( timeout 10s grep -nF 'journal.has_binding(&self.journal_binding)' "$$revalidate_body" | timeout 10s cut -d: -f1 )"; \
	joint_absence_line="$$( timeout 10s grep -nF 'FreshDbInvalidationDatabaseKind::JointlyAbsent' "$$revalidate_body" | timeout 10s head -n 1 | timeout 10s cut -d: -f1 )"; \
	first_installation_revalidation_line="$$( timeout 10s grep -nF 'self.installation.revalidate_mutable_namespace()?;' "$$revalidate_body" | timeout 10s head -n 1 | timeout 10s cut -d: -f1 )"; \
	database_before_line="$$( timeout 10s grep -nF 'let database_before =' "$$revalidate_body" | timeout 10s cut -d: -f1 )"; \
	namespace_line="$$( timeout 10s grep -nF '.revalidate(&self.installation, journal, &self.record)?;' "$$revalidate_body" | timeout 10s cut -d: -f1 )"; \
	database_after_line="$$( timeout 10s grep -nF 'let database_after =' "$$revalidate_body" | timeout 10s cut -d: -f1 )"; \
	plan_line="$$( timeout 10s grep -nF 'fresh_db_invalidation_plan_is_exact(&self.record)' "$$revalidate_body" | timeout 10s cut -d: -f1 )"; \
	final_joint_absence_line="$$( timeout 10s grep -nF 'FreshDbInvalidationDatabaseKind::JointlyAbsent' "$$revalidate_body" | timeout 10s tail -n 1 | timeout 10s cut -d: -f1 )"; \
	final_installation_revalidation_line="$$( timeout 10s grep -nF 'self.installation.revalidate_mutable_namespace()?;' "$$revalidate_body" | timeout 10s tail -n 1 | timeout 10s cut -d: -f1 )"; \
	timeout 10s test "$$binding_line" -lt "$$joint_absence_line"; \
	timeout 10s test "$$joint_absence_line" -lt "$$first_installation_revalidation_line"; \
	timeout 10s test "$$first_installation_revalidation_line" -lt "$$database_before_line"; \
	timeout 10s test "$$database_before_line" -lt "$$namespace_line"; \
	timeout 10s test "$$namespace_line" -lt "$$database_after_line"; \
	timeout 10s test "$$database_after_line" -lt "$$plan_line"; \
	timeout 10s test "$$plan_line" -lt "$$final_joint_absence_line"; \
	timeout 10s test "$$final_joint_absence_line" -lt "$$final_installation_revalidation_line"; \
	if timeout 10s rg -n -w -F 'before_database' "$$persistence_authority"; then exit 1; else status="$$?"; timeout 10s test "$$status" = 1; fi; \
	if timeout 10s rg -n 'RollbackActionOutcome|\.rollback_successor\(' "$$executor"; then exit 1; else status="$$?"; timeout 10s test "$$status" = 1; fi; \
	timeout 10s rg -n -w -F 'fresh_db_invalidated_successor' crates/forge/src/client --glob '*.rs' --glob '!**/tests/**' --glob '!**/tests.rs' > "$$symbol_refs"; \
	timeout 10s test "$$( timeout 10s wc -l < "$$symbol_refs" )" = 2; \
	timeout 10s test "$$( timeout 10s grep -Fc "$$executor:" "$$symbol_refs" )" = 1; \
	timeout 10s test "$$( timeout 10s grep -Fc "$$persistence_authority:" "$$symbol_refs" )" = 1; \
	timeout 10s test "$$( timeout 10s rg -n '\.rollback_successor\(' "$$executor" "$$persistence_authority" "$$effect" "$$authority" "$$reopen" | timeout 10s wc -l )" = 1; \
	timeout 10s test "$$( timeout 10s rg -n '\.advance[[:space:]]*\(' "$$executor" "$$persistence_authority" "$$effect" "$$authority" "$$reopen" | timeout 10s wc -l )" = 1; \
	timeout 10s rg -n -w -F 'remove_exact_fresh_transition' crates/forge/src/client --glob '*.rs' --glob '!**/tests/**' --glob '!**/tests.rs' --glob '!**/*_tests.rs' > "$$symbol_refs"; \
	timeout 10s test "$$( timeout 10s wc -l < "$$symbol_refs" )" = 1; \
	timeout 10s test "$$( timeout 10s cut -d: -f1 "$$symbol_refs" )" = "$$effect"; \
	timeout 10s grep -Fq 'state_db.remove_exact_fresh_transition(preimage)' "$$effect"; \
	timeout 10s test "$$( timeout 10s grep -Fc 'remove_exact_fresh_transition(' "$$exact" )" = 1; \
	timeout 10s rg -U -q '^    pub\(crate\) fn remove_exact_fresh_transition\(\n        &self,\n        preimage: ExactFreshTransitionPreimage,\n    \) -> Result<ExactFreshTransitionAbsence, ExactFreshTransitionRemovalError> \{' "$$exact"; \
	if timeout 10s rg -U -n '#\[derive\([^]]*Clone[^]]*\)\]\n(?:#\[[^\n]*\]\n)*(?:pub\([^)]*\)[[:space:]]+)?(?:struct|enum)[[:space:]]+(UsrRollbackFreshDbInvalidation(?:Authority|ApplyAuthority|FinishAuthority|DatabaseEvidence|EffectAuthority))' "$$authority" "$$effect" "$$persistence_authority"; then exit 1; else status="$$?"; timeout 10s test "$$status" = 1; fi; \
	if timeout 10s rg -n 'impl Clone for UsrRollbackFreshDbInvalidation(?:Authority|ApplyAuthority|FinishAuthority|DatabaseEvidence|EffectAuthority)' "$$authority" "$$effect" "$$persistence_authority"; then exit 1; else status="$$?"; timeout 10s test "$$status" = 1; fi; \
	if timeout 10s rg -n 'FreshDbInvalidationPersistenceSeal|FreshDbInvalidation.*Persistence.*Seal' crates/forge/src/client --glob '*.rs' --glob '!**/tests/**' --glob '!**/tests.rs'; then exit 1; else status="$$?"; timeout 10s test "$$status" = 1; fi; \
	if timeout 10s rg -n '[[:alnum:]_]+Seal' "$$executor" "$$persistence_authority"; then exit 1; else status="$$?"; timeout 10s test "$$status" = 1; fi; \
	timeout 10s grep -Fqx 'pub(in crate::client) enum DurableUsrRollbackFreshDbInvalidationRecord {' "$$executor"; \
	timeout 10s grep -Fqx '    FreshDbInvalidationIntent,' "$$executor"; \
	timeout 10s grep -Fqx '    FreshDbInvalidated,' "$$executor"; \
	timeout 10s grep -Fqx '                    durable: DurableUsrRollbackFreshDbInvalidationRecord::FreshDbInvalidationIntent,' "$$executor"; \
	timeout 10s grep -Fqx '                    durable: DurableUsrRollbackFreshDbInvalidationRecord::FreshDbInvalidated,' "$$executor"; \
	timeout 10s test "$$( timeout 10s grep -Fc 'Ok((reopened, Some(actual))) if actual == source_record => {' "$$executor" )" = 1; \
	timeout 10s test "$$( timeout 10s grep -Fc 'Ok((reopened, Some(actual))) if actual == successor => {' "$$executor" )" = 1; \
	timeout 10s grep -Fqx '            Ok((reopened, Some(actual))) if actual == successor => Ok((reopened, successor)),' "$$executor"; \
	if timeout 10s rg -n 'retained_mutable_cast_directory|open_in_retained_cast|journal\.load\(' "$$executor"; then exit 1; else status="$$?"; timeout 10s test "$$status" = 1; fi; \
	timeout 10s sed -E 's,//.*$$,,' "$$executor" "$$persistence_authority" "$$effect" > "$$production_code"; \
	if timeout 10s rg -n '^[[:space:]]*(loop|while|for)[[:space:]]|=[[:space:]]*(loop|while|for)[[:space:]]' "$$production_code"; then exit 1; else status="$$?"; timeout 10s test "$$status" = 1; fi; \
	if timeout 10s rg -n 'retry|forward_successor|clear_transition_if_matches|remove_transition_if_matches|run_transaction_triggers|run_system_triggers|root_links|archive_previous|rearchive_archived|preserve_failed|cleanup|dispatch' "$$production_code"; then exit 1; else status="$$?"; timeout 10s test "$$status" = 1; fi; \
	if timeout 10s rg -n 'diesel::|SqliteConnection|sql_query|\.execute[[:space:]]*\(|\.transaction[[:space:]]*\(|insert_fresh_metadata|delete_metadata|\.add[[:space:]]*\(|\.create[[:space:]]*\(|\.remove[[:space:]]*\(|\.batch_remove[[:space:]]*\(|\.delete[[:space:]]*\(' "$$production_code"; then exit 1; else status="$$?"; timeout 10s test "$$status" = 1; fi; \
	if timeout 10s rg -n 'renameat|std::fs|(^|[^_[:alnum:]])fs::|rename[[:space:]]*\(|unlink(at)?[[:space:]]*\(|linkat[[:space:]]*\(|sync_(all|data)|write_all|set_permissions|chmod|create_dir|remove_(dir|file)|hard_link|symlink|attempt_move|reconcile_move' "$$production_code"; then exit 1; else status="$$?"; timeout 10s test "$$status" = 1; fi; \
	timeout 10s test "$$( timeout 10s grep -Fc 'exercise_success_matrix(FreshDbInvalidationOrigin::' "$$matrix" )" = 2; \
	timeout 10s test "$$( timeout 10s grep -Fc 'for historical in [false, true] {' "$$matrix" )" = 1; \
	timeout 10s test "$$( timeout 10s grep -Fc 'for source in Source::ALL {' "$$matrix" )" = 1; \
	timeout 10s test "$$( timeout 10s grep -Fc 'assert_eq!(database_snapshot(&fixture), database_before' "$$matrix" )" = 2; \
	timeout 10s test "$$( timeout 10s grep -Fc 'assert_eq!(non_journal_namespace_snapshot(&fixture), namespace_before' "$$matrix" )" = 2; \
	timeout 10s test "$$( timeout 10s grep -Fc '    historical: bool,' "$$support" )" = 1; \
	timeout 10s grep -Fq 'FreshDbInvalidationFixture::historical(source, usr_outcome, candidate_outcome, row)' "$$support"; \
	timeout 10s grep -Fq 'TransitionJournalStore::open_retained(' "$$races"; \
	timeout 10s grep -Fq 'arm_before_usr_rollback_fresh_db_invalidation_persistence_final_revalidation(hook);' "$$races"; \
	for race in Database Journal Installation Namespace; do timeout 10s grep -Fq "FinalRace::$$race" "$$races"; done; \
	for fault in temporary_sync update_exchange update_first_directory_sync displaced_unlink update_final_directory_sync; do \
		timeout 10s grep -Fqx "            arm_next_$${fault}_fault," "$$storage"; \
	done; \
	timeout 10s test "$$( timeout 10s rg -n '^            arm_next_(temporary_sync|update_exchange|update_first_directory_sync|displaced_unlink|update_final_directory_sync)_fault,$$' "$$storage" | timeout 10s wc -l )" = 5; \
	timeout 10s grep -Fq 'TransitionJournalStore::try_open_in_retained_cast(' "$$storage"; \
	timeout 10s grep -Fq 'drop(reopened);' "$$storage"; \
	timeout 10s test "$$( timeout 10s grep -Fc 'drop(reservation);' "$$restart" )" = 2; \
	timeout 10s awk '/drop\(reservation\);/ { dropped += 1; awaiting = 1; next } awaiting && /ActiveStateReservation::acquire\(\)\.unwrap\(\);/ { reacquired += 1; awaiting = 0 } END { exit !(dropped == 2 && reacquired == 2 && awaiting == 0) }' "$$restart"; \
	timeout 10s test "$$( timeout 10s grep -Fc 'arm_next_temporary_sync_fault();' "$$restart" )" = 1; \
	timeout 10s test "$$( timeout 10s grep -Fc 'arm_next_update_first_directory_sync_fault();' "$$restart" )" = 1; \
	timeout 10s grep -Fq 'let finish = fixture.capture_finish(&journal, &reservation);' "$$restart"; \
	timeout 10s grep -Fq 'assert_eq!(authority.origin_for_test(), RollbackActionOutcome::AlreadySatisfied);' "$$restart"; \
	timeout 10s test "$$( timeout 10s grep -Fc 'fresh_db_invalidation_removal_call_count(), 0' "$$restart" )" = 2; \
	timeout 10s grep -Fq 'durable: DurableUsrRollbackFreshDbInvalidationRecord::FreshDbInvalidationIntent,' "$$restart"; \
	timeout 10s grep -Fq 'durable: DurableUsrRollbackFreshDbInvalidationRecord::FreshDbInvalidated,' "$$restart"; \
	timeout 10s grep -Fq 'UsrRollbackFreshDbInvalidationAdmission::NotApplicable' "$$restart"; \
	for file in "$$executor" "$$persistence_authority" "$$effect" "$$authority" "$$exact" "$$reopen" "$$recovery_root" "$$reconciliation_root" "$$tests" "$$support" "$$matrix" "$$races" "$$storage" "$$restart" misc/make/startup-fresh-db-invalidation-persistence-tests.mk Makefile misc/make/help.mk; do \
		timeout 10s test "$$( timeout 10s wc -l < "$$file" )" -le 1000; \
	done; \
	timeout 1200s $(CARGO) test -p forge --lib "$$prefix" -- --test-threads=1
