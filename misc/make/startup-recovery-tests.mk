.PHONY: forge-startup-usr-rollback-decision-test

forge-startup-usr-rollback-decision-test:
	@set -eu; \
	listed="$$( timeout 300s $(CARGO) test -p forge --lib -- --list )"; \
	timeout 10s grep -q . <<<"$$listed"; \
	count="$$( timeout 10s grep -c '^client::startup_recovery::usr_rollback_decision::tests::.*: test$$' <<<"$$listed" )"; \
	timeout 10s test "$$count" = 11; \
	for test in \
		client::startup_recovery::usr_rollback_decision::tests::matrix::startup_usr_rollback_decision_admitted_matrix_persists_exact_plan \
		client::startup_recovery::usr_rollback_decision::tests::matrix::startup_usr_rollback_decision_exchanged_pre_remains_incompatible \
		client::startup_recovery::usr_rollback_decision::tests::matrix::startup_usr_rollback_decision_changes_only_the_canonical_journal \
		client::startup_recovery::usr_rollback_decision::tests::evidence_races::startup_usr_rollback_decision_database_and_provenance_conflicts_never_advance \
		client::startup_recovery::usr_rollback_decision::tests::evidence_races::startup_usr_rollback_decision_namespace_layout_and_abi_conflicts_never_advance \
		client::startup_recovery::usr_rollback_decision::tests::evidence_races::startup_usr_rollback_decision_evidence_races_fail_before_advance \
		client::startup_recovery::usr_rollback_decision::tests::evidence_races::startup_usr_rollback_decision_historical_epoch_uses_durable_identity \
		client::startup_recovery::usr_rollback_decision::tests::evidence_races::startup_usr_rollback_decision_active_reblit_uses_one_state_row_and_retains_reservation \
		client::startup_recovery::usr_rollback_decision::tests::storage_reopen::startup_usr_rollback_decision_storage_faults_reopen_to_exact_source_or_decision \
		client::startup_recovery::usr_rollback_decision::tests::storage_reopen::startup_usr_rollback_decision_consumes_journal_before_reopen \
		client::startup_recovery::usr_rollback_decision::tests::storage_reopen::startup_usr_rollback_decision_next_startup_routes_exact_decision; do \
		timeout 10s grep -Fqx "$$test: test" <<<"$$listed"; \
	done; \
	executor=crates/forge/src/client/startup_recovery/usr_rollback_decision.rs; \
	reopen=crates/forge/src/client/startup_recovery/canonical_journal_reopen.rs; \
	authority=crates/forge/src/client/startup_reconciliation/usr_rollback_decision_authority.rs; \
	reconciliation=crates/forge/src/client/startup_reconciliation.rs; \
	startup_gate=crates/forge/src/client/startup_gate.rs; \
	journal_store=crates/forge/src/transition_journal/store.rs; \
	decision_count="$$( timeout 10s rg -n '\.rollback_decision\(' "$$executor" "$$authority" | timeout 10s wc -l )"; \
	timeout 10s test "$$decision_count" = 1; \
	timeout 10s grep -Fqx '    let decision = match source_record.rollback_decision(observations) {' "$$executor"; \
	advance_count="$$( timeout 10s rg -n '\.advance\(' "$$executor" "$$authority" "$$reopen" | timeout 10s wc -l )"; \
	timeout 10s test "$$advance_count" = 1; \
	timeout 10s grep -Fqx '    let advance = journal.advance(&source_record, &decision);' "$$executor"; \
	timeout 10s grep -Fqx 'mod canonical_journal_reopen;' crates/forge/src/client/startup_recovery.rs; \
	timeout 10s test "$$( timeout 10s rg -n 'canonical_journal_reopen' crates/forge/src/client/startup_recovery.rs | timeout 10s wc -l )" = 1; \
	timeout 10s test "$$( timeout 10s rg -n '^pub\(super\) fn reopen_canonical_journal\(' "$$reopen" | timeout 10s wc -l )" = 1; \
	timeout 10s grep -Fqx 'pub(super) enum CanonicalJournalReopenError {' "$$reopen"; \
	timeout 10s rg -U -q '^pub\(super\) fn reopen_canonical_journal\(\n    installation: &Installation,\n\) -> Result<\(TransitionJournalStore, Option<TransitionRecord>\), CanonicalJournalReopenError> \{' "$$reopen"; \
	timeout 10s test "$$( timeout 10s grep -Fc 'reopen_canonical_journal(&installation)' "$$executor" )" = 1; \
	timeout 10s grep -Fqx '    let reopened = reopen_canonical_journal(&installation).map_err(UsrRollbackDecisionReopenError::from);' "$$executor"; \
	clone_line="$$( timeout 10s grep -nF '    let installation = authority.installation().clone();' "$$executor" | timeout 10s cut -d: -f1 )"; \
	advance_line="$$( timeout 10s grep -nF '    let advance = journal.advance(&source_record, &decision);' "$$executor" | timeout 10s cut -d: -f1 )"; \
	drop_authority_line="$$( timeout 10s grep -nF '    drop(authority);' "$$executor" | timeout 10s tail -n 1 | timeout 10s cut -d: -f1 )"; \
	drop_journal_line="$$( timeout 10s grep -nF '    drop(journal);' "$$executor" | timeout 10s tail -n 1 | timeout 10s cut -d: -f1 )"; \
	reopen_line="$$( timeout 10s grep -nF '    let reopened = reopen_canonical_journal(&installation).map_err(UsrRollbackDecisionReopenError::from);' "$$executor" | timeout 10s cut -d: -f1 )"; \
	timeout 10s test "$$clone_line" -lt "$$advance_line"; \
	timeout 10s test "$$advance_line" -lt "$$drop_authority_line"; \
	timeout 10s test "$$drop_authority_line" -lt "$$drop_journal_line"; \
	timeout 10s test "$$drop_journal_line" -lt "$$reopen_line"; \
	suffix="$$( timeout 10s sed -n '/    let advance = journal.advance(&source_record, &decision);/,/    let reopened = reopen_canonical_journal/p' "$$executor" )"; \
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
	if timeout 10s rg -n 'rollback_successor|forward_successor|transition_identity|linux_fs|std::fs|nix::|renameat|unlinkat|linkat|sync_all|sync_data|write_all|set_permissions|create_dir|remove_dir|remove_file|hard_link|symlink|run_transaction_triggers|run_system_triggers|root_links|archive_previous|rearchive_archived|preserve_failed|exchange_forward|exchange_reverse|remove_exact_archived|add_with_transition|insert_fresh_metadata|delete_metadata_provenance|clear_transition_if_matches|remove_transition_if_matches|\.add\(|\.remove\(|\.batch_remove\(|\.execute\(|\.transaction\(|\.delete\(' "$$executor" "$$authority" "$$reopen"; then exit 1; fi; \
	if timeout 10s rg -n 'PendingSystemTransition|ActivationNamespaceEvidence' "$$executor" "$$authority"; then exit 1; fi; \
	timeout 10s awk '$$0 == "pub(in crate::client) fn persist_usr_rollback_decision_and_reopen(" { state = 1; next } state == 1 && $$0 == "    journal: TransitionJournalStore," { state = 2; next } state == 2 && $$0 ~ /authority: UsrRollbackDecisionAuthority/ { found = 1 } END { exit !found }' "$$executor"; \
	if timeout 10s rg -n 'journal: &[[:space:]]*TransitionJournalStore' "$$executor"; then exit 1; fi; \
	seal_count="$$( timeout 10s rg -n '^pub\(in crate::client\) struct UsrRollbackDecisionSeal \{' "$$startup_gate" | timeout 10s wc -l )"; \
	timeout 10s test "$$seal_count" = 1; \
	timeout 10s awk '$$0 == "pub(in crate::client) struct UsrRollbackDecisionSeal {" { state = 1; next } state == 1 && $$0 == "    _private: ()," { state = 2; next } state == 2 && $$0 == "}" { found += 1; state = 0 } END { exit found != 1 }' "$$startup_gate"; \
	timeout 10s awk '$$0 == "impl UsrRollbackDecisionSeal {" { state = 1; next } state == 1 && $$0 == "    fn new() -> Self {" { found += 1; state = 0 } END { exit found != 1 }' "$$startup_gate"; \
	seal_call_count="$$( timeout 10s rg -n 'UsrRollbackDecisionSeal::new\(' crates/forge/src/client --glob '*.rs' --glob '!**/tests/**' --glob '!**/*_tests.rs' --glob '!**/*_tests/**' | timeout 10s wc -l )"; \
	timeout 10s test "$$seal_call_count" = 1; \
	capture_call_count="$$( timeout 10s rg -n 'UsrRollbackDecisionAuthority::capture\(' crates/forge/src/client --glob '*.rs' --glob '!**/tests/**' --glob '!**/*_tests.rs' --glob '!**/*_tests/**' | timeout 10s wc -l )"; \
	timeout 10s test "$$capture_call_count" = 1; \
	timeout 10s grep -Fqx '        _startup_gate_seal: &UsrRollbackDecisionSeal,' "$$authority"; \
	decision_seal_impl="$$( timeout 10s sed -n '/^impl UsrRollbackDecisionSeal {/,/^}/p' "$$startup_gate" )"; \
	timeout 10s test -n "$$decision_seal_impl"; \
	timeout 10s test "$$( timeout 10s grep -Fc '    pub(in crate::client) fn new_for_test() -> Self {' <<<"$$decision_seal_impl" )" = 1; \
	timeout 10s awk '$$0 == "    #[cfg(test)]" { gated = 1; next } $$0 == "    pub(in crate::client) fn new_for_test() -> Self {" { if (!gated) exit 1; found += 1; gated = 0; next } gated { exit 1 } END { exit found != 1 }' <<<"$$decision_seal_impl"; \
	if timeout 10s rg -n 'fn new_for_test\(' "$$authority"; then exit 1; else status="$$?"; timeout 10s test "$$status" = 1; fi; \
	timeout 10s grep -Fqx '    journal_binding: TransitionJournalBinding,' "$$authority"; \
	binding_capture_count="$$( timeout 10s rg -n 'let journal_binding = journal\.binding\(\);' "$$authority" | timeout 10s wc -l )"; \
	timeout 10s test "$$binding_capture_count" = 1; \
	binding_check_count="$$( timeout 10s rg -n 'journal\.has_binding\(&self\.journal_binding\)' "$$authority" | timeout 10s wc -l )"; \
	timeout 10s test "$$binding_check_count" = 1; \
	timeout 10s awk '$$0 ~ /^impl UsrRollbackDecisionEvidence/ { evidence = 1; next } evidence && $$0 ~ /^    fn revalidate\(/ { active = 1; next } active && $$0 == "        if !journal.has_binding(&self.journal_binding) {" { found = 1; exit } active && ($$0 ~ /self\.installation/ || $$0 ~ /journal\.load/) { exit 1 } END { exit !found }' "$$authority"; \
	timeout 10s grep -Fqx 'pub(crate) struct TransitionJournalBinding(Arc<()>);' "$$journal_store"; \
	timeout 10s grep -Fqx '    binding: Arc<()>,' "$$journal_store"; \
	timeout 10s grep -Fqx '            binding: Arc::new(()),' "$$journal_store"; \
	timeout 10s grep -Fqx '        Arc::ptr_eq(&self.binding, &expected.0)' "$$journal_store"; \
	intent_post_count="$$( timeout 10s rg -n '^            \(Phase::UsrExchangeIntent, UsrExchangeLayout::Post\) => None,$$' "$$authority" | timeout 10s wc -l )"; \
	timeout 10s test "$$intent_post_count" = 1; \
	parent_required_count="$$( timeout 10s rg -n 'UsrRollbackDecisionAdmission::ParentDurabilityRequired\(' "$$authority" | timeout 10s wc -l )"; \
	timeout 10s test "$$parent_required_count" = 1; \
	if timeout 10s rg -n 'UsrRollbackDecisionDeferral::ForwardExchangeDurabilityUnproven' "$$authority"; then exit 1; fi; \
	timeout 10s grep -Fqx '            (Phase::UsrExchangeIntent, UsrExchangeLayout::Pre) => Some(InitialRollbackAction::AlreadySatisfied),' "$$authority"; \
	timeout 10s grep -Fqx '            (Phase::UsrExchanged, UsrExchangeLayout::Post) => Some(InitialRollbackAction::Pending),' "$$authority"; \
	blocker_count="$$( timeout 10s rg -n 'RecoveryBlocker::ForwardExchangeDurabilityUnproven' "$$reconciliation" | timeout 10s wc -l )"; \
	timeout 10s test "$$blocker_count" = 1; \
	timeout 10s grep -Fq 'record.phase == Phase::UsrExchangeIntent && namespace.usr_exchange_layout() == Some(UsrExchangeLayout::Post)' "$$reconciliation"; \
	for file in "$$executor" "$$authority" "$$reopen" misc/make/startup-recovery-tests.mk; do \
		timeout 10s test "$$( timeout 10s wc -l < "$$file" )" -le 1000; \
	done; \
	timeout 1200s $(CARGO) test -p forge --lib \
		'client::startup_recovery::usr_rollback_decision::tests::' \
		-- --test-threads=1
