.PHONY: forge-startup-usr-rollback-decision-test

forge-startup-usr-rollback-decision-test:
	@set -eu; \
	listed="$$( timeout 300s $(CARGO) test -p forge --lib -- --list )"; \
	timeout 10s grep -q . <<<"$$listed"; \
	count="$$( timeout 10s grep -c '^client::startup_recovery::usr_rollback_decision::tests::.*: test$$' <<<"$$listed" )"; \
	timeout 10s test "$$count" = 16; \
	for test in \
		client::startup_recovery::usr_rollback_decision::tests::matrix::startup_usr_rollback_decision_admitted_matrix_persists_exact_plan \
		client::startup_recovery::usr_rollback_decision::tests::matrix::startup_usr_rollback_decision_exchanged_pre_remains_incompatible \
		client::startup_recovery::usr_rollback_decision::tests::matrix::startup_root_links_complete_requires_exact_complete_abi_and_never_republishes \
		client::startup_recovery::usr_rollback_decision::tests::matrix::startup_usr_rollback_decision_changes_only_the_canonical_journal \
		client::startup_recovery::usr_rollback_decision::tests::evidence_races::startup_usr_rollback_decision_database_and_provenance_conflicts_never_advance \
		client::startup_recovery::usr_rollback_decision::tests::evidence_races::startup_root_links_complete_same_byte_journal_replacement_breaks_record_binding \
		client::startup_recovery::usr_rollback_decision::tests::evidence_races::startup_root_links_complete_successor_same_byte_replacement_reopens_but_never_succeeds \
		client::startup_recovery::usr_rollback_decision::tests::evidence_races::startup_root_links_complete_successor_same_byte_replacement_after_binding_before_reopen_never_succeeds \
		client::startup_recovery::usr_rollback_decision::tests::evidence_races::startup_usr_rollback_decision_namespace_layout_and_abi_conflicts_never_advance \
		client::startup_recovery::usr_rollback_decision::tests::evidence_races::startup_usr_rollback_decision_evidence_races_fail_before_advance \
		client::startup_recovery::usr_rollback_decision::tests::evidence_races::startup_usr_rollback_decision_historical_epoch_uses_durable_identity \
		client::startup_recovery::usr_rollback_decision::tests::evidence_races::startup_usr_rollback_decision_active_reblit_uses_one_state_row_and_retains_reservation \
		client::startup_recovery::usr_rollback_decision::tests::storage_reopen::startup_usr_rollback_decision_storage_faults_reopen_to_exact_source_or_decision \
		client::startup_recovery::usr_rollback_decision::tests::storage_reopen::startup_root_links_complete_next_entry_routes_exact_decision_without_reverse_effect \
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
	record_binding=crates/forge/src/transition_journal/store/record_binding.rs; \
	decision_count="$$( timeout 10s rg -n '\.rollback_decision\(' "$$executor" "$$authority" | timeout 10s wc -l )"; \
	timeout 10s test "$$decision_count" = 1; \
	timeout 10s grep -Fqx '    let decision = match source_record.rollback_decision(observations) {' "$$executor"; \
	if timeout 10s rg -n 'journal\.advance\(' "$$executor" "$$authority" "$$reopen"; then exit 1; else status="$$?"; timeout 10s test "$$status" = 1; fi; \
	timeout 10s test "$$( timeout 10s grep -Fc 'authority.advance_record_binding(&journal, &decision)' "$$executor" )" = 1; \
	timeout 10s test "$$( timeout 10s grep -Fc '.advance_record_binding(cast, self.evidence.journal_record_binding, next)' "$$authority" )" = 1; \
	timeout 10s grep -Fqx 'mod canonical_journal_reopen;' crates/forge/src/client/startup_recovery.rs; \
	timeout 10s test "$$( timeout 10s rg -n 'canonical_journal_reopen' crates/forge/src/client/startup_recovery.rs | timeout 10s wc -l )" = 1; \
	timeout 10s test "$$( timeout 10s rg -n '^pub\(super\) fn reopen_canonical_journal\(' "$$reopen" | timeout 10s wc -l )" = 1; \
	timeout 10s grep -Fqx 'pub(super) enum CanonicalJournalReopenError {' "$$reopen"; \
	timeout 10s rg -U -q '^pub\(super\) fn reopen_canonical_journal\(\n    installation: &Installation,\n\) -> Result<\(TransitionJournalStore, Option<TransitionRecord>\), CanonicalJournalReopenError> \{' "$$reopen"; \
	timeout 10s test "$$( timeout 10s grep -Fc 'reopen_canonical_journal(&installation)' "$$executor" )" = 1; \
	timeout 10s grep -Fqx '    let reopened = reopen_canonical_journal(&installation).map_err(UsrRollbackDecisionReopenError::from);' "$$executor"; \
	clone_line="$$( timeout 10s grep -nF '    let installation = authority.installation().clone();' "$$executor" | timeout 10s cut -d: -f1 )"; \
	advance_line="$$( timeout 10s grep -nF '    let advance = match authority.advance_record_binding(&journal, &decision) {' "$$executor" | timeout 10s cut -d: -f1 )"; \
	drop_journal_line="$$( timeout 10s grep -nF '    drop(journal);' "$$executor" | timeout 10s tail -n 1 | timeout 10s cut -d: -f1 )"; \
	reopen_line="$$( timeout 10s grep -nF '    let reopened = reopen_canonical_journal(&installation).map_err(UsrRollbackDecisionReopenError::from);' "$$executor" | timeout 10s cut -d: -f1 )"; \
	timeout 10s test "$$clone_line" -lt "$$advance_line"; \
	timeout 10s test "$$advance_line" -lt "$$drop_journal_line"; \
	timeout 10s test "$$drop_journal_line" -lt "$$reopen_line"; \
	seam_line="$$( timeout 10s grep -nF '        after_usr_rollback_decision_successor_binding_check_before_reopen();' "$$executor" | timeout 10s cut -d: -f1 )"; \
	timeout 10s test "$$drop_journal_line" -lt "$$seam_line"; \
	timeout 10s test "$$seam_line" -lt "$$reopen_line"; \
	suffix="$$( timeout 10s sed -n '/    let advance = match authority.advance_record_binding/,/    let reopened = reopen_canonical_journal/p' "$$executor" )"; \
	if timeout 10s grep -Fq 'drop(authority)' <<<"$$suffix"; then exit 1; else status="$$?"; timeout 10s test "$$status" = 1; fi; \
	timeout 10s grep -Fq '    drop(journal);' <<<"$$suffix"; \
	timeout 10s test "$$( timeout 10s grep -Fc 'reopen_canonical_journal(&installation)' <<<"$$suffix" )" = 1; \
	timeout 10s awk ' \
		function fail() { bad = 1; exit } \
		$$0 == "        Ok(successor_binding) => {" { if (active || seen) fail(); active = 1; seen = 1; next } \
		active && $$0 == "        Err(UsrRollbackDecisionRecordAdvanceError::Authority(source)) => {" { active = 0; closed = 1; next } \
		active { \
			if ($$0 ~ /(^|[^[:alnum:]_])return([^[:alnum:]_]|$$)/ || index($$0, "?") || $$0 ~ /[[:alpha:]_][[:alnum:]_]*![[:space:]]*[({[]/) fail(); \
			if (index($$0, "UsrRollbackDecisionAdvanceOutcome::SuccessorBindingFailed(")) failures += 1; \
		} \
		END { if (bad || seen != 1 || closed != 1 || active || failures < 1) exit 1 } \
	' "$$executor"; \
	timeout 10s awk ' \
		function fail() { bad = 1; exit } \
		function finish_branch() { \
			if (branch == 0) return; \
			if (errors != 1 || error_open || !error_closed || !branch_closed) fail(); \
			if (branch == 1 && (record_binding != 1 || durable_source != 1 || durable_decision != 0 || combined != 0)) fail(); \
			if (branch == 2 && (record_binding != 1 || durable_source != 0 || durable_decision != 1 || combined != 0)) fail(); \
			if ((branch == 3 || branch == 4) && (record_binding != 0 || durable_source != 0 || durable_decision != 0 || combined != 1)) fail(); \
		} \
		function reset_branch() { \
			errors = record_binding = durable_source = durable_decision = combined = 0; \
			error_open = error_closed = branch_closed = 0; \
		} \
		$$0 == "        UsrRollbackDecisionAdvanceOutcome::SuccessorBindingFailed(binding) => match reopened {" { \
			if (active || seen != 0) fail(); active = 1; seen = 1; next; \
		} \
		active && $$0 == "        }," { finish_branch(); active = 0; closed = 1; next; } \
		active && $$0 ~ /^            .* => / { \
			finish_branch(); branch += 1; reset_branch(); \
			if (branch == 1 && $$0 != "            Ok((reopened, Some(actual))) if actual == source_record => {") fail(); \
			if (branch == 2 && $$0 != "            Ok((reopened, Some(actual))) if actual == decision => {") fail(); \
			if (branch == 3 && $$0 != "            Ok((reopened, actual)) => {") fail(); \
			if (branch == 4) { \
				if ($$0 != "            Err(reopen) => Err(UsrRollbackDecisionPersistenceError::SuccessorRecordBindingAndReopen {") fail(); \
				errors = 1; combined = 1; error_open = 1; \
			} \
			if (branch > 4) fail(); next; \
		} \
		active { \
			if ($$0 ~ /(^|[^[:alnum:]_])return([^[:alnum:]_]|$$)/ || index($$0, "?") != 0 || $$0 ~ /[[:alpha:]_][[:alnum:]_]*![[:space:]]*[({[]/) fail(); \
			if ($$0 ~ /^[[:space:]]*Ok\(/) fail(); \
			if (branch_closed && $$0 !~ /^[[:space:]]*(\/\/.*)?$$/) fail(); \
			if ($$0 == "                Err(UsrRollbackDecisionPersistenceError::SuccessorRecordBinding {") { \
				if (branch > 2 || error_open || error_closed) fail(); errors += 1; record_binding += 1; error_open = 1; next; \
			} \
			if ($$0 == "                Err(UsrRollbackDecisionPersistenceError::SuccessorRecordBindingAndReopen {") { \
				if (branch != 3 || error_open || error_closed) fail(); errors += 1; combined += 1; error_open = 1; next; \
			} \
			if ($$0 == "                    durable: DurableUsrRollbackDecisionRecord::Source,") durable_source += 1; \
			if ($$0 == "                    durable: DurableUsrRollbackDecisionRecord::Decision,") durable_decision += 1; \
			if (branch <= 3 && error_open && $$0 == "                })") { error_open = 0; error_closed = 1; next; } \
			if (branch <= 3 && $$0 == "            }") { if (!error_closed || error_open || branch_closed) fail(); branch_closed = 1; next; } \
			if (branch == 4 && error_open && $$0 == "            }),") { error_open = 0; error_closed = 1; branch_closed = 1; next; } \
			if (error_closed && !branch_closed && $$0 !~ /^[[:space:]]*(\/\/.*)?$$/) fail(); \
		} \
		END { if (bad || seen != 1 || closed != 1 || active || branch != 4) exit 1 } \
	' "$$executor"; \
	if timeout 10s rg -n 'open_in_retained_cast|journal\.load\(' "$$executor"; then exit 1; fi; \
	timeout 10s rg -U -q 'installation\.revalidate_mutable_namespace\(\)\?;\n    let cast = installation\.retained_mutable_cast_directory\(\)\?;\n    let journal = TransitionJournalStore::open_in_retained_cast\(cast, &installation\.root\)\?;\n    installation\.revalidate_mutable_namespace\(\)\?;\n    let record = journal\.load_revalidated_retained_cast\(cast\)\?;\n    installation\.revalidate_mutable_namespace\(\)\?;' "$$reopen"; \
	timeout 10s test "$$( timeout 10s grep -Fc '    installation.revalidate_mutable_namespace()?;' "$$reopen" )" = 3; \
	timeout 10s test "$$( timeout 10s grep -Fc 'installation.retained_mutable_cast_directory()?' "$$reopen" )" = 1; \
	timeout 10s test "$$( timeout 10s grep -Fc 'TransitionJournalStore::open_in_retained_cast(' "$$reopen" )" = 1; \
	timeout 10s test "$$( timeout 10s grep -Fc 'journal.load_revalidated_retained_cast(cast)?' "$$reopen" )" = 1; \
	timeout 10s grep -Fqx '            CanonicalJournalReopenError::Installation(source) => Self::Installation(source),' "$$executor"; \
	timeout 10s grep -Fqx '            CanonicalJournalReopenError::Journal(source) => Self::Journal(source),' "$$executor"; \
	if timeout 10s rg -n 'Phase|rollback_decision|rollback_successor|forward_successor|TransitionJournalStore::(open|open_retained|try_open_in_retained_cast)\(|std::fs|fs::|File::open|OpenOptions|openat|AsRawFd|IntoRawFd|FromRawFd|AsFd|RawFd|BorrowedFd|OwnedFd|unsafe[[:space:]]*\{' "$$reopen"; then exit 1; fi; \
	if timeout 10s rg -n 'rollback_successor|forward_successor|transition_identity|linux_fs|std::fs|nix::|renameat|unlinkat|linkat|sync_all|sync_data|write_all|set_permissions|create_dir|remove_dir|remove_file|hard_link|symlink|run_transaction_triggers|run_system_triggers|root_links|archive_previous|rearchive_archived|preserve_failed|exchange_forward|exchange_reverse|remove_exact_archived|add_with_transition|insert_fresh_metadata|delete_metadata_provenance|clear_transition_if_matches|remove_transition_if_matches|\.add\(|\.remove\(|\.batch_remove\(|\.execute\(|\.transaction\(|\.delete\(' "$$executor" "$$authority" "$$reopen"; then exit 1; fi; \
	if timeout 10s rg -n 'PendingSystemTransition|ActivationNamespaceEvidence' "$$executor" "$$authority"; then exit 1; fi; \
	timeout 10s awk '$$0 == "pub(in crate::client) fn persist_usr_rollback_decision_and_reopen(" { state = 1; next } state == 1 && $$0 == "    journal: TransitionJournalStore," { state = 2; next } state == 2 && $$0 ~ /authority: UsrRollbackDecisionAuthority/ { found = 1 } END { exit !found }' "$$executor"; \
	persist_signature="$$( timeout 10s sed -n '/^pub(in crate::client) fn persist_usr_rollback_decision_and_reopen(/,/^)/p' "$$executor" )"; \
	if timeout 10s rg -n 'journal: &[[:space:]]*TransitionJournalStore' <<<"$$persist_signature"; then exit 1; fi; \
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
	timeout 10s grep -Fqx '    journal_record_binding: TransitionJournalRecordBinding,' "$$authority"; \
	if timeout 10s rg -n 'Option<TransitionJournalRecordBinding>' "$$authority"; then exit 1; else status="$$?"; timeout 10s test "$$status" = 1; fi; \
	if rg -U -n '#\[derive\([^]]*Clone[^]]*\)\]\n(?:#\[[^\n]*\]\n)*pub\(crate\) struct TransitionJournalRecordBinding' "$$record_binding"; then exit 1; else status="$$?"; test "$$status" = 1; fi; \
	if rg -n 'impl[[:space:]]+Clone[[:space:]]+for[[:space:]]+TransitionJournalRecordBinding' "$$record_binding"; then exit 1; else status="$$?"; test "$$status" = 1; fi; \
	timeout 10s test "$$( timeout 10s grep -Fc 'journal.record_binding(installation.retained_mutable_cast_directory()?, record)?' "$$authority" )" = 1; \
	timeout 10s test "$$( timeout 10s grep -Fc 'require_journal_record_binding(' "$$authority" )" = 4; \
	timeout 10s test "$$( timeout 10s grep -Fc 'journal.has_record_binding(cast, binding, record)?' "$$authority" )" = 1; \
	timeout 10s test "$$( timeout 10s grep -Fc '.has_record_binding(cast, &successor_binding, &decision)' "$$executor" )" = 1; \
	timeout 10s grep -Fqx '    Published(TransitionJournalRecordBinding),' "$$executor"; \
	timeout 10s grep -Fqx '        UsrRollbackDecisionAdvanceOutcome::Published(successor_binding) => match reopened {' "$$executor"; \
	timeout 10s test "$$( timeout 10s grep -Fc '.has_reopened_record_binding(cast, successor_binding, decision)' "$$executor" )" = 1; \
	reopened_binding_helper="$$( timeout 10s sed -n '/^fn revalidate_reopened_decision_binding(/,/^}/p' "$$executor" )"; \
	timeout 10s test "$$( timeout 10s grep -Fc '.revalidate_mutable_namespace()' <<<"$$reopened_binding_helper" )" = 2; \
	timeout 10s grep -Fq '.has_reopened_record_binding(cast, successor_binding, decision)' <<<"$$reopened_binding_helper"; \
	timeout 10s grep -Fq 'arm_after_usr_rollback_decision_successor_binding_check_before_reopen' "$$executor"; \
	binding_helper="$$( timeout 10s sed -n '/^fn require_journal_record_binding(/,/^}/p' "$$authority" )"; \
	timeout 10s grep -Fq '    if !journal.has_record_store_binding(binding) {' <<<"$$binding_helper"; \
	store_binding_line="$$( timeout 10s grep -nF '    if !journal.has_record_store_binding(binding) {' <<<"$$binding_helper" | timeout 10s cut -d: -f1 )"; \
	cast_binding_line="$$( timeout 10s grep -nF '    let cast = installation.retained_mutable_cast_directory()?;' <<<"$$binding_helper" | timeout 10s cut -d: -f1 )"; \
	timeout 10s test "$$store_binding_line" -lt "$$cast_binding_line"; \
	capture_line="$$( timeout 10s grep -nF 'journal.record_binding(installation.retained_mutable_cast_directory()?, record)?' "$$authority" | timeout 10s cut -d: -f1 )"; \
	namespace_line="$$( timeout 10s grep -nF 'let namespace_inspection = match UsrRollbackDecisionNamespaceInspection::begin' "$$authority" | timeout 10s cut -d: -f1 )"; \
	timeout 10s test "$$capture_line" -lt "$$namespace_line"; \
	timeout 10s grep -Fqx 'pub(crate) struct TransitionJournalBinding(Arc<()>);' "$$journal_store"; \
	timeout 10s grep -Fqx '    binding: Arc<()>,' "$$journal_store"; \
	timeout 10s grep -Fqx '            binding: Arc::new(()),' "$$journal_store"; \
	timeout 10s grep -Fqx '        Arc::ptr_eq(&self.binding, &expected.0)' "$$journal_store"; \
	timeout 10s grep -Fqx 'pub(crate) struct TransitionJournalRecordBinding {' "$$record_binding"; \
	timeout 10s grep -Fq 'pub(crate) fn has_record_store_binding(' "$$record_binding"; \
	timeout 10s grep -Fq 'pub(crate) fn advance_record_binding(' "$$record_binding"; \
	intent_post_count="$$( timeout 10s rg -n '^            \(Phase::UsrExchangeIntent, UsrExchangeLayout::Post\) => None,$$' "$$authority" | timeout 10s wc -l )"; \
	timeout 10s test "$$intent_post_count" = 1; \
	parent_required_count="$$( timeout 10s rg -n 'UsrRollbackDecisionAdmission::ParentDurabilityRequired\(' "$$authority" | timeout 10s wc -l )"; \
	timeout 10s test "$$parent_required_count" = 1; \
	if timeout 10s rg -n 'UsrRollbackDecisionDeferral::ForwardExchangeDurabilityUnproven' "$$authority"; then exit 1; fi; \
	timeout 10s grep -Fqx '            (Phase::UsrExchangeIntent, UsrExchangeLayout::Pre) => Some(InitialRollbackAction::AlreadySatisfied),' "$$authority"; \
	timeout 10s grep -Fqx '            (Phase::UsrExchanged, UsrExchangeLayout::Post) => Some(InitialRollbackAction::Pending),' "$$authority"; \
	timeout 10s grep -Fqx '            (Phase::RootLinksComplete, UsrExchangeLayout::Post) => Some(InitialRollbackAction::Pending),' "$$authority"; \
	if timeout 10s rg -n 'normalize_usr_exchanged_root_abi|synchronize_usr_exchanged_root_abi|publish_root_abi|root_abi\.publish|create_root_links' "$$executor" "$$authority"; then exit 1; else status="$$?"; timeout 10s test "$$status" = 1; fi; \
	blocker_count="$$( timeout 10s rg -n 'RecoveryBlocker::ForwardExchangeDurabilityUnproven' "$$reconciliation" | timeout 10s wc -l )"; \
	timeout 10s test "$$blocker_count" = 1; \
	timeout 10s grep -Fq 'record.phase == Phase::UsrExchangeIntent && namespace.usr_exchange_layout() == Some(UsrExchangeLayout::Post)' "$$reconciliation"; \
	for file in "$$executor" "$$authority" "$$reopen" misc/make/startup-recovery-tests.mk; do \
		timeout 10s test "$$( timeout 10s wc -l < "$$file" )" -le 1000; \
	done; \
	timeout 1200s $(CARGO) test -p forge --lib \
		'client::startup_recovery::usr_rollback_decision::tests::' \
		-- --test-threads=1
