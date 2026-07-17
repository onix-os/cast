.PHONY: forge-startup-usr-rollback-reverse-durability-test

forge-startup-usr-rollback-reverse-durability-test:
	@set -eu; \
	listed="$$( timeout 300s $(CARGO) test -p forge --lib -- --list )"; \
	timeout 10s test -n "$$listed"; \
	count="$$( timeout 10s grep -c '^client::startup_reconciliation::usr_rollback_reverse_authority::effect_reconciliation::durability::tests::.*: test$$' <<<"$$listed" )"; \
	timeout 10s test "$$count" = 6; \
	for test in \
		client::startup_reconciliation::usr_rollback_reverse_authority::effect_reconciliation::durability::tests::reverse_durability_constructs_outcome_only_after_both_parent_barriers_for_every_operation \
		client::startup_reconciliation::usr_rollback_reverse_authority::effect_reconciliation::durability::tests::reverse_durability_binding_is_the_first_check_for_both_provenances \
		client::startup_reconciliation::usr_rollback_reverse_authority::effect_reconciliation::durability::tests::reverse_durability_faults_consume_authority_at_each_ordered_boundary \
		client::startup_reconciliation::usr_rollback_reverse_authority::effect_reconciliation::durability::tests::reverse_durability_rejects_database_journal_and_namespace_changes_before_sync \
		client::startup_reconciliation::usr_rollback_reverse_authority::effect_reconciliation::durability::tests::reverse_durability_rejects_database_journal_and_namespace_changes_between_syncs \
		client::startup_reconciliation::usr_rollback_reverse_authority::effect_reconciliation::durability::tests::reverse_durability_rejects_database_journal_and_namespace_changes_after_parent_syncs; do \
		timeout 10s grep -Fqx "$$test: test" <<<"$$listed"; \
	done; \
	parent=crates/forge/src/client/startup_reconciliation/usr_rollback_reverse_authority/effect_reconciliation.rs; \
	authority=crates/forge/src/client/startup_reconciliation/usr_rollback_reverse_authority/effect_reconciliation/durability.rs; \
	executor=crates/forge/src/client/startup_recovery/usr_rollback_reverse_durability.rs; \
	dispatcher=crates/forge/src/client/startup_recovery/usr_rollback_reverse_dispatch.rs; \
	tests=crates/forge/src/client/startup_reconciliation/usr_rollback_reverse_authority/effect_reconciliation/durability/tests/mod.rs; \
	timeout 10s grep -Fqx 'mod durability;' "$$parent"; \
	timeout 10s grep -Fqx 'mod usr_rollback_reverse_durability;' crates/forge/src/client/startup_recovery.rs; \
	timeout 10s grep -Fqx 'pub(in crate::client) struct UsrRollbackReverseDurabilitySeal {' "$$executor"; \
	timeout 10s grep -Fqx '    _private: (),' "$$executor"; \
	timeout 10s test "$$( timeout 10s grep -Fc 'UsrRollbackReverseDurabilitySeal::new()' "$$executor" )" = 2; \
	timeout 10s grep -Fqx "pub(in crate::client) struct UsrRollbackReverseDurableEffectAuthority<'reservation> {" "$$authority"; \
	timeout 10s grep -Fqx '    outcome: RollbackActionOutcome,' "$$authority"; \
	timeout 10s grep -Fq 'outcome: RollbackActionOutcome::Applied,' "$$authority"; \
	timeout 10s grep -Fq 'outcome: RollbackActionOutcome::AlreadySatisfied,' "$$authority"; \
	timeout 10s test "$$( timeout 10s grep -Fc 'if !journal.has_binding(&self._effect.journal_binding) {' "$$authority" )" = 3; \
	timeout 10s test "$$( timeout 10s grep -Fc 'namespace.complete_parent_durability(&installation, &record)' "$$authority" )" = 2; \
	timeout 10s grep -Fqx '    pub(in crate::client) fn installation(&self) -> &Installation {' "$$authority"; \
	timeout 10s grep -Fqx '    pub(in crate::client) fn record(&self) -> &TransitionRecord {' "$$authority"; \
	timeout 10s grep -Fqx '    pub(in crate::client) fn usr_restored_successor(&self) -> Result<TransitionRecord, CodecError> {' "$$authority"; \
	timeout 10s test "$$( timeout 10s grep -Fc 'self._effect.record.rollback_successor(Some(self.outcome))' "$$authority" )" = 1; \
	if timeout 10s rg -n 'fn outcome\(' "$$authority"; then exit 1; fi; \
	if timeout 10s rg -n '^[[:space:]]{8,}outcome: RollbackActionOutcome([,)]|$$)' "$$authority"; then exit 1; fi; \
	timeout 10s rg -U -q '#\[cfg\(test\)\]\nimpl UsrRollbackReverseDurableEffectAuthority<'"'"'_> \{\n    pub\(in crate::client\) fn outcome_for_test\(&self\) -> RollbackActionOutcome \{' "$$authority"; \
	revalidate_line="$$( timeout 10s grep -nF '    pub(in crate::client) fn revalidate(' "$$authority" | timeout 10s head -n 1 | timeout 10s cut -d: -f1 )"; \
	binding_line="$$( timeout 10s grep -nF '        if !journal.has_binding(&self._effect.journal_binding) {' "$$authority" | timeout 10s head -n 1 | timeout 10s cut -d: -f1 )"; \
	pre_line="$$( timeout 10s grep -nF '        require_pre_namespace_evidence(' "$$authority" | timeout 10s head -n 1 | timeout 10s cut -d: -f1 )"; \
	namespace_line="$$( timeout 10s grep -nF '        let namespace_result = effect.namespace.revalidate(&effect.installation, &effect.record);' "$$authority" | timeout 10s cut -d: -f1 )"; \
	trailing_line="$$( timeout 10s grep -nF '        let trailing_evidence = require_post_namespace_evidence(' "$$authority" | timeout 10s head -n 1 | timeout 10s cut -d: -f1 )"; \
	namespace_result_line="$$( timeout 10s grep -nF '        namespace_result?;' "$$authority" | timeout 10s cut -d: -f1 )"; \
	trailing_result_line="$$( timeout 10s grep -nFx '        trailing_evidence' "$$authority" | timeout 10s cut -d: -f1 )"; \
	timeout 10s test "$$revalidate_line" -lt "$$binding_line"; \
	timeout 10s test "$$binding_line" -lt "$$pre_line"; \
	timeout 10s test "$$pre_line" -lt "$$namespace_line"; \
	timeout 10s test "$$namespace_line" -lt "$$trailing_line"; \
	timeout 10s test "$$trailing_line" -lt "$$namespace_result_line"; \
	timeout 10s test "$$namespace_result_line" -lt "$$trailing_result_line"; \
	if timeout 10s rg -n 'RollbackActionOutcome|outcome:' "$$parent"; then exit 1; fi; \
	callers="$$( timeout 10s rg -n 'complete_(applied|already_satisfied)_usr_rollback_reverse_durability\(' crates/forge/src/client --glob '*.rs' --glob '!**/tests/**' --glob '!**/*_tests.rs' --glob '!**/*_tests/**' --glob '!**/usr_rollback_reverse_durability.rs' | timeout 10s wc -l )"; \
	timeout 10s test "$$callers" = 2; \
	timeout 10s test "$$( timeout 10s grep -Fc 'complete_applied_usr_rollback_reverse_durability(&journal, authority)?' "$$dispatcher" )" = 1; \
	timeout 10s test "$$( timeout 10s grep -Fc 'complete_already_satisfied_usr_rollback_reverse_durability(&journal, authority)?' "$$dispatcher" )" = 1; \
	if timeout 10s rg -n 'exchange_retained_usr_once|attempt_usr_exchange_once|renameat2|RENAME_EXCHANGE|unlinkat|linkat|symlinkat' "$$authority" "$$executor"; then exit 1; fi; \
	if timeout 10s rg -n 'rollback_successor|forward_successor' "$$executor"; then exit 1; fi; \
	if timeout 10s rg -n '\.advance[[:space:]]*\(|forward_successor|clear_transition_if_matches|remove_transition_if_matches|run_transaction_triggers|run_system_triggers|root_links|archive_previous|rearchive_archived|preserve_failed|remove_exact_archived' "$$authority" "$$executor"; then exit 1; fi; \
	if timeout 10s rg -n 'AsRawFd|IntoRawFd|FromRawFd|AsFd|RawFd|BorrowedFd|OwnedFd|as_raw_fd|into_raw_fd|from_raw_fd|as_fd[[:space:]]*\(|std::fs::File|fs::File|unsafe[[:space:]]*\{' "$$authority" "$$executor"; then exit 1; fi; \
	if timeout 10s rg -n 'File::open|OpenOptions|open_beneath|openat2|openat[[:space:]]*\(' "$$authority" "$$executor"; then exit 1; fi; \
	if timeout 10s rg -n '^[[:space:]]*(loop|while|for)[[:space:]]|=[[:space:]]*(loop|while)[[:space:]]' "$$authority" "$$executor"; then exit 1; fi; \
	for file in "$$parent" "$$authority" "$$executor" "$$tests" misc/make/startup-rollback-reverse-durability-tests.mk; do \
		timeout 10s test "$$( timeout 10s wc -l < "$$file" )" -le 1000; \
	done; \
	timeout 1200s $(CARGO) test -p forge --lib \
		'client::startup_reconciliation::usr_rollback_reverse_authority::effect_reconciliation::durability::tests::' \
		-- --test-threads=1
