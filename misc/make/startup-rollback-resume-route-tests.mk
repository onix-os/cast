.PHONY: forge-startup-usr-rollback-resume-route-test

forge-startup-usr-rollback-resume-route-test:
	@set -eu; \
	listed="$$( timeout 300s $(CARGO) test -p forge --lib -- --list )"; \
	timeout 10s test -n "$$listed"; \
	count="$$( timeout 10s grep -c '^client::startup_recovery::usr_rollback_resume_route::tests::.*: test$$' <<<"$$listed" )"; \
	timeout 10s test "$$count" = 11; \
	for test in \
		client::startup_recovery::usr_rollback_resume_route::tests::matrix::startup_usr_rollback_resume_route_pending_matrix_persists_reverse_exchange_intent \
		client::startup_recovery::usr_rollback_resume_route::tests::matrix::startup_usr_rollback_resume_route_satisfied_matrix_skips_reverse_exchange \
		client::startup_recovery::usr_rollback_resume_route::tests::matrix::startup_usr_rollback_resume_route_routes_only_and_preserves_exact_plan \
		client::startup_recovery::usr_rollback_resume_route::tests::evidence_races::startup_usr_rollback_resume_route_rejects_a_different_open_journal_binding \
		client::startup_recovery::usr_rollback_resume_route::tests::evidence_races::startup_usr_rollback_resume_route_database_and_provenance_conflicts_never_advance \
		client::startup_recovery::usr_rollback_resume_route::tests::evidence_races::startup_usr_rollback_resume_route_namespace_conflicts_never_advance \
		client::startup_recovery::usr_rollback_resume_route::tests::evidence_races::startup_usr_rollback_resume_route_capture_and_final_revalidation_races_fail_before_advance \
		client::startup_recovery::usr_rollback_resume_route::tests::evidence_races::startup_usr_rollback_resume_route_historical_and_active_reblit_evidence_remain_exact \
		client::startup_recovery::usr_rollback_resume_route::tests::storage_reopen::startup_usr_rollback_resume_route_storage_faults_reopen_to_exact_source_or_successor \
		client::startup_recovery::usr_rollback_resume_route::tests::storage_reopen::startup_usr_rollback_resume_route_rejects_cross_root_authority_and_reopens_success \
		client::startup_recovery::usr_rollback_resume_route::tests::end_to_end::startup_usr_rollback_resume_route_decision_then_route_uses_one_persistence_boundary_per_entry \
		transition_identity::journal_coordinator::tests::journal_coordinator_usr_exchange_effect_durability_faults_are_applied_without_reverse_or_retry; do \
		timeout 10s grep -Fqx "$$test: test" <<<"$$listed"; \
	done; \
	executor=crates/forge/src/client/startup_recovery/usr_rollback_resume_route.rs; \
	reopen=crates/forge/src/client/startup_recovery/canonical_journal_reopen.rs; \
	authority=crates/forge/src/client/startup_reconciliation/usr_rollback_resume_route_authority.rs; \
	proof=crates/forge/src/client/startup_reconciliation/activation_namespace/resume_route_proof.rs; \
	startup_gate=crates/forge/src/client/startup_gate.rs; \
	journal_store=crates/forge/src/transition_journal/store.rs; \
	coordinator_test=crates/forge/src/transition_identity/journal_coordinator/tests/usr_exchange_effect.rs; \
	forward_support=crates/forge/src/client/startup_recovery/forward_origin_test_support.rs; \
	successor_count="$$( timeout 10s rg -n '\.rollback_successor\(' "$$executor" "$$authority" "$$proof" | timeout 10s wc -l )"; \
	timeout 10s test "$$successor_count" = 1; \
	timeout 10s grep -Fqx '    let successor = match source_record.rollback_successor(None) {' "$$executor"; \
	advance_count="$$( timeout 10s rg -n '\.advance\(' "$$executor" "$$authority" "$$proof" "$$reopen" | timeout 10s wc -l )"; \
	timeout 10s test "$$advance_count" = 1; \
	timeout 10s grep -Fqx '    let advance = journal.advance(&source_record, &successor);' "$$executor"; \
	timeout 10s grep -Fqx 'mod canonical_journal_reopen;' crates/forge/src/client/startup_recovery.rs; \
	timeout 10s test "$$( timeout 10s rg -n 'canonical_journal_reopen' crates/forge/src/client/startup_recovery.rs | timeout 10s wc -l )" = 1; \
	timeout 10s test "$$( timeout 10s rg -n '^pub\(super\) fn reopen_canonical_journal\(' "$$reopen" | timeout 10s wc -l )" = 1; \
	timeout 10s grep -Fqx 'pub(super) enum CanonicalJournalReopenError {' "$$reopen"; \
	timeout 10s rg -U -q '^pub\(super\) fn reopen_canonical_journal\(\n    installation: &Installation,\n\) -> Result<\(TransitionJournalStore, Option<TransitionRecord>\), CanonicalJournalReopenError> \{' "$$reopen"; \
	timeout 10s test "$$( timeout 10s grep -Fc 'reopen_canonical_journal(&installation)' "$$executor" )" = 1; \
	timeout 10s grep -Fqx '    let reopened = reopen_canonical_journal(&installation).map_err(UsrRollbackResumeRouteReopenError::from);' "$$executor"; \
	clone_line="$$( timeout 10s grep -nF '    let installation = authority.installation().clone();' "$$executor" | timeout 10s cut -d: -f1 )"; \
	advance_line="$$( timeout 10s grep -nF '    let advance = journal.advance(&source_record, &successor);' "$$executor" | timeout 10s cut -d: -f1 )"; \
	drop_authority_line="$$( timeout 10s grep -nF '    drop(authority);' "$$executor" | timeout 10s tail -n 1 | timeout 10s cut -d: -f1 )"; \
	drop_journal_line="$$( timeout 10s grep -nF '    drop(journal);' "$$executor" | timeout 10s tail -n 1 | timeout 10s cut -d: -f1 )"; \
	reopen_line="$$( timeout 10s grep -nF '    let reopened = reopen_canonical_journal(&installation).map_err(UsrRollbackResumeRouteReopenError::from);' "$$executor" | timeout 10s cut -d: -f1 )"; \
	timeout 10s test "$$clone_line" -lt "$$advance_line"; \
	timeout 10s test "$$advance_line" -lt "$$drop_authority_line"; \
	timeout 10s test "$$drop_authority_line" -lt "$$drop_journal_line"; \
	timeout 10s test "$$drop_journal_line" -lt "$$reopen_line"; \
	suffix="$$( timeout 10s sed -n '/    let advance = journal.advance(&source_record, &successor);/,/    let reopened = reopen_canonical_journal/p' "$$executor" )"; \
	timeout 10s test "$$( timeout 10s grep -Fc '    drop(authority);' <<<"$$suffix" )" = 1; \
	timeout 10s test "$$( timeout 10s grep -Fc '    drop(journal);' <<<"$$suffix" )" = 1; \
	timeout 10s test "$$( timeout 10s grep -Fc 'reopen_canonical_journal(&installation)' <<<"$$suffix" )" = 1; \
	if timeout 10s rg -n 'retained_mutable_cast_directory|open_in_retained_cast|journal\.load\(' "$$executor"; then exit 1; fi; \
	timeout 10s rg -U -q 'installation\.revalidate_mutable_namespace\(\)\?;\n    let cast = installation\.retained_mutable_cast_directory\(\)\?;\n    let journal = TransitionJournalStore::open_in_retained_cast\(cast, &installation\.root\)\?;\n    installation\.revalidate_mutable_namespace\(\)\?;\n    let record = journal\.load\(\)\?;\n    installation\.revalidate_mutable_namespace\(\)\?;' "$$reopen"; \
	timeout 10s test "$$( timeout 10s grep -Fc '    installation.revalidate_mutable_namespace()?;' "$$reopen" )" = 3; \
	timeout 10s test "$$( timeout 10s grep -Fc 'installation.retained_mutable_cast_directory()?' "$$reopen" )" = 1; \
	timeout 10s test "$$( timeout 10s grep -Fc 'TransitionJournalStore::open_in_retained_cast(' "$$reopen" )" = 1; \
	timeout 10s test "$$( timeout 10s grep -Fc 'journal.load()?' "$$reopen" )" = 1; \
	timeout 10s grep -Fqx '            CanonicalJournalReopenError::Installation(source) => Self::Installation(source),' "$$executor"; \
	timeout 10s grep -Fqx '            CanonicalJournalReopenError::Journal(source) => Self::Journal(source),' "$$executor"; \
	if timeout 10s rg -n 'Phase|rollback_decision|rollback_successor|forward_successor|TransitionJournalStore::(open|open_retained|try_open_in_retained_cast)\(|std::fs|fs::|File::open|OpenOptions|openat|AsRawFd|IntoRawFd|FromRawFd|AsFd|RawFd|BorrowedFd|OwnedFd|unsafe[[:space:]]*\{' "$$reopen"; then exit 1; fi; \
	if timeout 10s rg -n 'forward_successor|RollbackActionOutcome|transition_identity|linux_fs|std::fs|nix::|renameat|unlinkat|linkat|sync_all|sync_data|write_all|set_permissions|create_dir|remove_dir|remove_file|hard_link|symlink|run_transaction_triggers|run_system_triggers|root_links|archive_previous|rearchive_archived|preserve_failed|exchange_forward|exchange_reverse|remove_exact_archived|add_with_transition|insert_fresh_metadata|delete_metadata_provenance|clear_transition_if_matches|remove_transition_if_matches|\.add\(|\.remove\(|\.batch_remove\(|\.execute\(|\.transaction\(|\.delete\(' "$$executor" "$$authority" "$$proof" "$$reopen"; then exit 1; fi; \
	if timeout 10s rg -n 'PendingSystemTransition|ActivationNamespaceEvidence' "$$executor" "$$authority" "$$proof"; then exit 1; fi; \
	timeout 10s awk '$$0 == "pub(in crate::client) fn persist_usr_rollback_resume_route_and_reopen(" { state = 1; next } state == 1 && $$0 == "    journal: TransitionJournalStore," { state = 2; next } state == 2 && $$0 ~ /authority: UsrRollbackResumeRouteAuthority/ { found = 1 } END { exit !found }' "$$executor"; \
	if timeout 10s rg -n 'journal: &[[:space:]]*TransitionJournalStore' "$$executor"; then exit 1; fi; \
	timeout 10s grep -Fq 'if actual == source_record' "$$executor"; \
	timeout 10s grep -Fq 'if actual == successor' "$$executor"; \
	seal_count="$$( timeout 10s rg -n '^pub\(in crate::client\) struct UsrRollbackResumeRouteSeal \{' "$$startup_gate" | timeout 10s wc -l )"; \
	timeout 10s test "$$seal_count" = 1; \
	timeout 10s awk '$$0 == "pub(in crate::client) struct UsrRollbackResumeRouteSeal {" { state = 1; next } state == 1 && $$0 == "    _private: ()," { state = 2; next } state == 2 && $$0 == "}" { found = 1 } END { exit !found }' "$$startup_gate"; \
	seal_call_count="$$( timeout 10s rg -n 'UsrRollbackResumeRouteSeal::new\(' crates/forge/src/client --glob '*.rs' --glob '!**/tests/**' --glob '!**/*_tests.rs' --glob '!**/*_tests/**' | timeout 10s wc -l )"; \
	timeout 10s test "$$seal_call_count" = 1; \
	capture_call_count="$$( timeout 10s rg -n 'UsrRollbackResumeRouteAuthority::capture\(' crates/forge/src/client --glob '*.rs' --glob '!**/tests/**' --glob '!**/*_tests.rs' --glob '!**/*_tests/**' | timeout 10s wc -l )"; \
	timeout 10s test "$$capture_call_count" = 1; \
	timeout 10s grep -Fqx '        _startup_gate_seal: &UsrRollbackResumeRouteSeal,' "$$authority"; \
	timeout 10s grep -Fqx '    journal_binding: TransitionJournalBinding,' "$$authority"; \
	timeout 10s test "$$( timeout 10s rg -n 'let journal_binding = journal\.binding\(\);' "$$authority" | timeout 10s wc -l )" = 1; \
	timeout 10s test "$$( timeout 10s rg -n 'journal\.has_binding\(&self\.journal_binding\)' "$$authority" | timeout 10s wc -l )" = 1; \
	timeout 10s awk '$$0 ~ /^    pub\(in crate::client\) fn revalidate\(/ { active = 1; next } active && $$0 == "        if !journal.has_binding(&self.journal_binding) {" { found = 1; exit } active && ($$0 ~ /self\.installation/ || $$0 ~ /journal\.load/) { exit 1 } END { exit !found }' "$$authority"; \
	timeout 10s grep -Fqx 'pub(crate) struct TransitionJournalBinding(Arc<()>);' "$$journal_store"; \
	timeout 10s grep -Fq 'if record.phase != Phase::RollbackDecided || !is_usr_exchange_rollback_source(record)' "$$authority"; \
	timeout 10s grep -Fq '(RollbackAction::Pending, UsrExchangeLayout::Post)' "$$authority"; \
	timeout 10s grep -Fq '(RollbackAction::AlreadySatisfied, UsrExchangeLayout::Pre)' "$$authority"; \
	timeout 10s grep -Fq 'super::startup_recovery::persist_usr_rollback_resume_route_and_reopen(journal, authority)?' "$$startup_gate"; \
	timeout 10s grep -Fq 'assert_usr_rollback_decision_routes_to_reverse_exchange_intent(' "$$coordinator_test"; \
	timeout 10s grep -Fq 'decision.rollback_successor(None).unwrap()' "$$coordinator_test"; \
	timeout 10s grep -Fq 'retained_exchange_syscall_count() == 1' "$$coordinator_test"; \
	timeout 10s grep -Fq 'assert_eq!(pending.phase(), Phase::ReverseExchangeIntent);' "$$forward_support"; \
	for file in "$$executor" "$$authority" "$$proof" "$$reopen" misc/make/startup-rollback-resume-route-tests.mk; do \
		timeout 10s test "$$( timeout 10s wc -l < "$$file" )" -le 1000; \
	done; \
	timeout 1200s $(CARGO) test -p forge --lib \
		'client::startup_recovery::usr_rollback_resume_route::tests::' \
		-- --test-threads=1; \
	timeout 1200s $(CARGO) test -p forge --lib \
		'transition_identity::journal_coordinator::tests::journal_coordinator_usr_exchange_effect_durability_faults_are_applied_without_reverse_or_retry' \
		-- --exact --test-threads=1
