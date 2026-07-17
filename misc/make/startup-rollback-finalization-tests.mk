.PHONY: forge-startup-usr-rollback-finalization-test

forge-startup-usr-rollback-finalization-test:
	@set -euo pipefail; \
	timeout 10s mkdir -p "$(TOP_DIR)/target"; \
	listed="$$( timeout 10s mktemp "$(TOP_DIR)/target/rollback-finalization-list.XXXXXXXXXXXX" )"; \
	refs="$$( timeout 10s mktemp "$(TOP_DIR)/target/rollback-finalization-refs.XXXXXXXXXXXX" )"; \
	executor_code="$$( timeout 10s mktemp "$(TOP_DIR)/target/rollback-finalization-code.XXXXXXXXXXXX" )"; \
	store_read_code="$$( timeout 10s mktemp "$(TOP_DIR)/target/rollback-finalization-store-read.XXXXXXXXXXXX" )"; \
	store_delete_code="$$( timeout 10s mktemp "$(TOP_DIR)/target/rollback-finalization-store-delete.XXXXXXXXXXXX" )"; \
	trap 'timeout 10s rm -f "$$listed" "$$refs" "$$executor_code" "$$store_read_code" "$$store_delete_code"' EXIT; \
	timeout 300s $(CARGO) test -p forge --lib -- --list | timeout 30s tee "$$listed" >/dev/null; \
	timeout 10s grep -q . "$$listed"; \
	authority_prefix='client::startup_reconciliation::usr_rollback_finalization_authority::tests::'; \
	timeout 10s test "$$( timeout 10s grep -c "^$$authority_prefix.*: test$$" "$$listed" )" = 5; \
	for name in \
		admission::startup_usr_rollback_finalization_admits_exact_current_and_historical_terminal_evidence \
		admission::startup_usr_rollback_finalization_rejects_inexact_phase_operation_plan_and_database \
		evidence::startup_usr_rollback_finalization_capture_sandwich_rejects_database_and_namespace_changes \
		evidence::startup_usr_rollback_finalization_revalidation_rejects_reopened_and_changed_authority \
		evidence::startup_usr_rollback_finalization_refuses_terminal_namespace_lookalikes; do \
		timeout 10s grep -Fqx "$$authority_prefix$$name: test" "$$listed"; \
	done; \
	executor_prefix='client::startup_recovery::usr_rollback_finalization::tests::'; \
	timeout 10s test "$$( timeout 10s grep -c "^$$executor_prefix.*: test$$" "$$listed" )" = 13; \
	for name in \
		delete_report::startup_usr_rollback_finalization_false_delete_classifies_only_exact_source_or_absence \
		evidence_races::startup_usr_rollback_finalization_rejects_reopened_and_cross_root_journal_bindings \
		evidence_races::startup_usr_rollback_finalization_final_evidence_races_never_delete \
		matrix::startup_usr_rollback_finalization_success_matrix_retains_exact_canonical_absence \
		post_delete_evidence::startup_usr_rollback_finalization_post_delete_evidence_races_never_report_success \
		public_binding_races::startup_usr_rollback_finalization_rejects_hidden_canonical_record_displacement \
		public_binding_races::startup_usr_rollback_finalization_rejects_public_journal_directory_substitution \
		public_binding_races::startup_usr_rollback_finalization_rejects_public_journal_lock_substitution \
		storage_reconciliation::startup_usr_rollback_finalization_delete_faults_observe_exact_terminal_or_absence \
		storage_reconciliation::startup_usr_rollback_finalization_returns_the_same_continuously_locked_store \
		storage_reconciliation::startup_usr_rollback_finalization_rejects_records_recreated_after_delete \
		storage_reconciliation::startup_usr_rollback_finalization_rejects_change_after_consuming_post_delete_authority \
		storage_reconciliation::startup_usr_rollback_finalization_reports_delete_error_with_ambiguous_observation; do \
		timeout 10s grep -Fqx "$$executor_prefix$$name: test" "$$listed"; \
	done; \
	startup_prefix='client::startup_gate::usr_rollback_new_state::tests::finalization::'; \
	timeout 10s test "$$( timeout 10s grep -c "^$$startup_prefix.*: test$$" "$$listed" )" = 5; \
	for name in \
		startup_new_state_suffix_terminal_handoff_retains_the_same_journal_lock_through_clean_startup \
		startup_new_state_suffix_reaudits_database_after_finalization_before_clean_admission \
		startup_new_state_suffix_finalization_converges_into_the_shared_prune_residue_audit \
		startup_new_state_suffix_rejects_terminal_record_recreated_during_clean_handoff \
		startup_new_state_suffix_rejects_mutable_namespace_substitution_after_terminal_finalization; do \
		timeout 10s grep -Fqx "$$startup_prefix$$name: test" "$$listed"; \
	done; \
	journal_delete_callback_contract='transition_journal::tests::journal_delete_durability_callbacks_follow_filesystem_operation_order'; \
	timeout 10s grep -Fqx "$$journal_delete_callback_contract: test" "$$listed"; \
	authority=crates/forge/src/client/startup_reconciliation/usr_rollback_finalization_authority.rs; \
	proof=crates/forge/src/client/startup_reconciliation/activation_namespace/rollback_finalization_proof.rs; \
	topology=crates/forge/src/client/startup_reconciliation/activation_namespace/candidate_preserve_proof.rs; \
	executor=crates/forge/src/client/startup_recovery/usr_rollback_finalization.rs; \
	recovery_root=crates/forge/src/client/startup_recovery.rs; \
	reconciliation_root=crates/forge/src/client/startup_reconciliation.rs; \
	namespace_root=crates/forge/src/client/startup_reconciliation/activation_namespace.rs; \
	journal_root=crates/forge/src/transition_journal.rs; \
	journal_store=crates/forge/src/transition_journal/store.rs; \
	journal_transactions=crates/forge/src/transition_journal/tests/storage_transactions.rs; \
	orchestrator=crates/forge/src/client/startup_gate/usr_rollback_new_state.rs; \
	startup_gate=crates/forge/src/client/startup_gate.rs; \
	startup_tests=crates/forge/src/client/startup_gate/usr_rollback_new_state/tests/finalization.rs; \
	executor_tests=crates/forge/src/client/startup_recovery/usr_rollback_finalization/tests/mod.rs; \
	executor_support=crates/forge/src/client/startup_recovery/usr_rollback_finalization/tests/support.rs; \
	executor_matrix=crates/forge/src/client/startup_recovery/usr_rollback_finalization/tests/matrix.rs; \
	executor_delete_report=crates/forge/src/client/startup_recovery/usr_rollback_finalization/tests/delete_report.rs; \
	executor_storage=crates/forge/src/client/startup_recovery/usr_rollback_finalization/tests/storage_reconciliation.rs; \
	executor_races=crates/forge/src/client/startup_recovery/usr_rollback_finalization/tests/evidence_races.rs; \
	executor_post=crates/forge/src/client/startup_recovery/usr_rollback_finalization/tests/post_delete_evidence.rs; \
	executor_binding=crates/forge/src/client/startup_recovery/usr_rollback_finalization/tests/public_binding_races.rs; \
	timeout 10s grep -Fqx 'mod usr_rollback_finalization_authority;' "$$reconciliation_root"; \
	timeout 10s grep -Fqx 'mod rollback_finalization_proof;' "$$namespace_root"; \
	timeout 10s grep -Fqx 'mod usr_rollback_finalization;' "$$recovery_root"; \
	timeout 10s grep -Fqx 'pub(super) use usr_rollback_finalization::{UsrRollbackFinalizationError, finalize_usr_rollback};' "$$recovery_root"; \
	for symbol in \
		UsrRollbackFinalizationAdmission \
		UsrRollbackFinalizationAuthority \
		UsrRollbackFinalizationAuthorityError; do \
		timeout 10s grep -Fq "$$symbol" "$$reconciliation_root"; \
	done; \
	for symbol in \
		DurableUsrRollbackFinalizationRecord \
		UsrRollbackFinalizationVerificationError \
		arm_after_usr_rollback_finalization_delete \
		arm_before_usr_rollback_finalization_final_durable_inspection \
		arm_before_usr_rollback_finalization_final_revalidation; do \
		timeout 10s grep -Fq "$$symbol" "$$recovery_root"; \
	done; \
	for module in delete_report evidence_races matrix post_delete_evidence public_binding_races route_support storage_reconciliation support; do \
		timeout 10s grep -Fq "mod $$module;" "$$executor_tests"; \
	done; \
	timeout 10s grep -Fqx 'pub(in crate::client) struct UsrRollbackFinalizationSeal {' "$$orchestrator"; \
	seal_impl="$$( timeout 10s sed -n '/^impl UsrRollbackFinalizationSeal {/,/^}/p' "$$orchestrator" )"; \
	timeout 10s test "$$( timeout 10s grep -Fc '    fn new() -> Self {' <<<"$$seal_impl" )" = 1; \
	timeout 10s test "$$( timeout 10s grep -Fc '    pub(in crate::client) fn new_for_test() -> Self {' <<<"$$seal_impl" )" = 1; \
	timeout 10s grep -Fq '        Self::new()' <<<"$$seal_impl"; \
	timeout 10s grep -Fq 'UsrRollbackFinalizationSeal' "$$startup_gate"; \
	timeout 10s grep -Fqx 'pub(in crate::client) enum UsrRollbackFinalizationAdmission<'\''reservation> {' "$$authority"; \
	timeout 10s grep -Fqx 'pub(in crate::client) struct UsrRollbackFinalizationAuthority<'\''reservation> {' "$$authority"; \
	timeout 10s grep -Fq '    journal_binding: TransitionJournalBinding,' "$$authority"; \
	timeout 10s grep -Fq '    absence: db::state::ExactFreshTransitionAbsence,' "$$authority"; \
	timeout 10s grep -Fq '    namespace: UsrRollbackFinalizationNamespaceProof,' "$$authority"; \
	timeout 10s grep -Fq '    _active_state_reservation: &'\''reservation ActiveStateReservation,' "$$authority"; \
	timeout 10s grep -Fq 'require_exact_new_state_rollback_complete_topology' "$$proof" "$$topology"; \
	timeout 10s test "$$( timeout 10s grep -Fc 'record.phase == Phase::RollbackComplete' "$$authority" )" = 1; \
	if timeout 10s rg -U -n '#\[derive\([^]]*Clone[^]]*\)\]\n(?:#\[[^\n]*\]\n)*(?:pub\([^)]*\)[[:space:]]+)?(?:struct|enum)[[:space:]]+(UsrRollbackFinalization(?:Authority|DatabaseEvidence|NamespaceProof))' "$$authority" "$$proof"; then exit 1; else status="$$?"; timeout 10s test "$$status" = 1; fi; \
	if timeout 10s rg -n 'impl Clone for UsrRollbackFinalization(?:Authority|DatabaseEvidence|NamespaceProof)' "$$authority" "$$proof"; then exit 1; else status="$$?"; timeout 10s test "$$status" = 1; fi; \
	if timeout 10s rg -n '\.delete\(|dispatch_usr_|persist_usr_|fn new\(\) -> Self' "$$authority" "$$proof"; then exit 1; else status="$$?"; timeout 10s test "$$status" = 1; fi; \
	timeout 10s rg -n -F 'UsrRollbackFinalizationAuthority::capture(' crates/forge/src/client --glob '*.rs' --glob '!**/tests/**' --glob '!**/tests.rs' --glob '!**/*_tests.rs' --glob '!**/*_tests/**' --glob '!**/usr_rollback_finalization_authority.rs' > "$$refs"; \
	timeout 10s test "$$( timeout 10s wc -l < "$$refs" )" = 1; \
	timeout 10s test "$$( timeout 10s cut -d: -f1 "$$refs" )" = "$$orchestrator"; \
	timeout 10s grep -Fq 'UsrRollbackFinalizationSeal::new();' "$$orchestrator"; \
	timeout 10s grep -Fq 'let journal = finalize_usr_rollback(journal, authority)?;' "$$orchestrator"; \
	timeout 10s grep -Fq 'Ok(Dispatch::Finalized { journal })' "$$orchestrator"; \
	timeout 10s grep -Fq 'usr_rollback_new_state::Dispatch::Finalized { journal } => {' "$$startup_gate"; \
	timeout 10s grep -Fq 'return Self::admit_clean(installation, state_db, journal, in_flight);' "$$startup_gate"; \
	timeout 10s test "$$( timeout 10s grep -Fc 'authority.journal().load_revalidated_retained_cast(cast)' "$$startup_gate" )" = 1; \
	timeout 10s grep -Fq 'CanonicalTransitionAppearedDuringCleanAdmission {' "$$startup_gate"; \
	timeout 10s grep -Fq 'write_new_private_record(&canonical, &recreated)' "$$startup_tests"; \
	timeout 10s grep -Fq 'fs::rename(callback_cast, &callback_displaced)' "$$startup_tests"; \
	residue_audit_line="$$( timeout 10s grep -nF 'let residue = transition_identity::audit_archived_state_prune_residue' "$$startup_gate" | timeout 10s cut -d: -f1 )"; \
	final_absence_line="$$( timeout 10s grep -nF 'authority.journal().load_revalidated_retained_cast(cast)' "$$startup_gate" | timeout 10s cut -d: -f1 )"; \
	clean_admission_line="$$( timeout 10s grep -nF 'Ok(Self { _authority: authority })' "$$startup_gate" | timeout 10s cut -d: -f1 )"; \
	timeout 10s test "$$residue_audit_line" -lt "$$final_absence_line"; \
	timeout 10s test "$$final_absence_line" -lt "$$clean_admission_line"; \
	timeout 10s rg -U -q '^pub\(in crate::client\) fn finalize_usr_rollback\(\n    journal: TransitionJournalStore,\n    authority: UsrRollbackFinalizationAuthority<'\''_>,\n\) -> Result<TransitionJournalStore, UsrRollbackFinalizationError> \{' "$$executor"; \
	timeout 10s test "$$( timeout 10s grep -Fc '.revalidate(&journal)' "$$executor" )" = 2; \
	timeout 10s test "$$( timeout 10s grep -Fc 'journal.delete_revalidated_retained_cast(cast, &source_record)' "$$executor" )" = 1; \
	timeout 10s test "$$( timeout 10s grep -Fc 'journal.load_revalidated_retained_cast(cast)' "$$executor" )" = 1; \
	timeout 10s grep -Fq 'authority.revalidate_after_journal_delete(journal)?' "$$executor"; \
	timeout 10s grep -Fq 'pub(in crate::client) fn revalidate_after_journal_delete(' "$$authority"; \
	timeout 10s grep -Fq 'pub(in crate::client::startup_reconciliation) fn revalidate_after_journal_delete(' "$$proof"; \
	timeout 10s grep -Fq 'journal.load_revalidated_retained_cast(cast)?' "$$proof"; \
	timeout 10s grep -Fq 'Ok(DurableUsrRollbackFinalizationRecord::Absent) => Ok(journal),' "$$executor"; \
	if timeout 10s rg -n 'canonical_journal_reopen|reopen_canonical_journal' "$$executor" "$$executor_tests" "$$executor_matrix" "$$executor_delete_report" "$$executor_storage" "$$executor_races" "$$executor_post" "$$executor_binding"; then exit 1; else status="$$?"; timeout 10s test "$$status" = 1; fi; \
	if timeout 10s rg -n 'finalize_usr_rollback_and_reopen|UsrRollbackFinalizationReopenError|arm_before_usr_rollback_finalization_delete' "$$executor" "$$recovery_root" "$$executor_tests" "$$executor_matrix" "$$executor_delete_report" "$$executor_storage" "$$executor_races" "$$executor_post" "$$executor_binding"; then exit 1; else status="$$?"; timeout 10s test "$$status" = 1; fi; \
	if timeout 10s rg -n 'journal\.delete[[:space:]]*\(' crates/forge/src/client --glob '*.rs' --glob '!**/tests/**' --glob '!**/tests.rs' --glob '!**/*_tests.rs' --glob '!**/*_tests/**'; then exit 1; else status="$$?"; timeout 10s test "$$status" = 1; fi; \
	for hook in \
		arm_next_delete_canonical_unlink_fault \
		assert_delete_canonical_unlink_fault_consumed \
		arm_next_delete_directory_sync_fault \
		assert_delete_directory_sync_fault_consumed; do \
		timeout 10s grep -Fq "pub(crate) fn $$hook(" "$$journal_root"; \
		timeout 10s grep -Fq "$$hook" "$$executor_storage"; \
	done; \
	for api in revalidate_retained_cast_binding load_revalidated_retained_cast delete_revalidated_retained_cast; do \
		timeout 10s grep -Fq "pub(crate) fn $$api(" "$$journal_store"; \
	done; \
	timeout 10s grep -Fqx 'pub(crate) enum JournalDeleteDurabilityBoundary {' "$$journal_store"; \
	timeout 10s grep -Fqx 'pub(crate) fn arm_journal_delete_durability_callback(' "$$journal_store"; \
	timeout 10s grep -Fq 'JournalDeleteDurabilityBoundary' "$$journal_root"; \
	timeout 10s grep -Fq 'arm_journal_delete_durability_callback' "$$journal_root"; \
	timeout 10s grep -Fq 'fn journal_delete_durability_callbacks_follow_filesystem_operation_order()' "$$journal_transactions"; \
	timeout 10s test "$$( timeout 10s grep -Fc 'arm_journal_delete_durability_callback(' "$$journal_transactions" )" = 2; \
	timeout 10s sed -E 's,//.*$$,,' "$$executor" > "$$executor_code"; \
	if timeout 10s rg -n '^[[:space:]]*(loop|while|for)[[:space:]]|=[[:space:]]*(loop|while|for)[[:space:]]|retry|cleanup' "$$executor_code"; then exit 1; else status="$$?"; timeout 10s test "$$status" = 1; fi; \
	if timeout 10s rg -n 'diesel::|SqliteConnection|sql_query|run_transaction_triggers|run_system_triggers|root_links|archive_previous|rearchive_archived|preserve_failed|clear_transition_if_matches|remove_transition_if_matches|remove_exact_(fresh|archived)|\.add[[:space:]]*\(|\.create[[:space:]]*\(|\.remove[[:space:]]*\(|\.batch_remove[[:space:]]*\(|\.execute[[:space:]]*\(|\.transaction[[:space:]]*\(' "$$executor_code"; then exit 1; else status="$$?"; timeout 10s test "$$status" = 1; fi; \
	if timeout 10s rg -n 'transition_identity|linux_fs|std::fs|nix::|renameat|unlinkat|linkat|sync_(all|data)|write_all|set_permissions|chmod|create_dir|remove_(dir|file)|hard_link|symlink' "$$executor_code"; then exit 1; else status="$$?"; timeout 10s test "$$status" = 1; fi; \
	{ \
		timeout 10s sed -n '/^    pub(crate) fn load_revalidated_retained_cast/,/^    pub(crate) fn binding/p' "$$journal_store" | timeout 10s sed '$$d'; \
		timeout 10s sed -n '/^    pub(crate) fn revalidate_retained_cast_binding/,/^    pub(crate) fn create/p' "$$journal_store" | timeout 10s sed '$$d'; \
		timeout 10s sed -n '/^    fn load_pinned/,/^    pub(super) fn create_temporary/p' "$$journal_store" | timeout 10s sed '$$d'; \
	} | timeout 10s sed -E 's,//.*$$,,' > "$$store_read_code"; \
	timeout 10s grep -Fq 'pub(crate) fn load_revalidated_retained_cast' "$$store_read_code"; \
	timeout 10s grep -Fq 'pub(crate) fn revalidate_retained_cast_binding' "$$store_read_code"; \
	timeout 10s grep -Fq 'fn revalidate_retained_cast_binding_locked' "$$store_read_code"; \
	timeout 10s grep -Fq 'fn inspect_exact_public_entry_set' "$$store_read_code"; \
	timeout 10s grep -Fq 'fn revalidate_exact_public_state' "$$store_read_code"; \
	timeout 10s grep -Fq 'directory_entries(&scan)' "$$store_read_code"; \
	if timeout 10s rg -n 'ensure_|mkdir|chmod|sync_(all|data)|cleanup|create_|unlinkat|renameat|write_all|set_permissions|remove_(dir|file)' "$$store_read_code"; then exit 1; else status="$$?"; timeout 10s test "$$status" = 1; fi; \
	timeout 10s sed -n '/^    pub(crate) fn delete(/,/^    fn lock_operation/p' "$$journal_store" | timeout 10s sed '$$d' | timeout 10s sed -E 's,//.*$$,,' > "$$store_delete_code"; \
	timeout 10s grep -Fq 'pub(crate) fn delete_revalidated_retained_cast' "$$store_delete_code"; \
	timeout 10s test "$$( timeout 10s grep -Fc 'self.revalidate_retained_cast_binding_locked(cast_directory)?;' "$$store_delete_code" )" = 4; \
	timeout 10s test "$$( timeout 10s grep -Fc 'unlinkat(self.directory.as_raw_fd(), CANONICAL_NAME)' "$$store_delete_code" )" = 1; \
	timeout 10s test "$$( timeout 10s grep -Fc '.sync_all()' "$$store_delete_code" )" = 1; \
	timeout 10s test "$$( timeout 10s grep -Fc 'journal_delete_durability_boundary(JournalDeleteDurabilityBoundary::CanonicalUnlinked);' "$$store_delete_code" )" = 1; \
	timeout 10s test "$$( timeout 10s grep -Fc 'journal_delete_durability_boundary(JournalDeleteDurabilityBoundary::DeleteDirectorySynced);' "$$store_delete_code" )" = 1; \
	timeout 10s rg -U -q '#\[cfg\(test\)\]\n        journal_delete_durability_boundary\(JournalDeleteDurabilityBoundary::CanonicalUnlinked\);' "$$store_delete_code"; \
	timeout 10s rg -U -q '#\[cfg\(test\)\]\n        journal_delete_durability_boundary\(JournalDeleteDurabilityBoundary::DeleteDirectorySynced\);' "$$store_delete_code"; \
	unlink_line="$$( timeout 10s grep -nF 'unlinkat(self.directory.as_raw_fd(), CANONICAL_NAME)' "$$store_delete_code" | timeout 10s cut -d: -f1 )"; \
	unlink_callback_line="$$( timeout 10s grep -nF 'journal_delete_durability_boundary(JournalDeleteDurabilityBoundary::CanonicalUnlinked);' "$$store_delete_code" | timeout 10s cut -d: -f1 )"; \
	sync_line="$$( timeout 10s grep -nF '.and_then(|()| self.directory.sync_all())' "$$store_delete_code" | timeout 10s cut -d: -f1 )"; \
	sync_callback_line="$$( timeout 10s grep -nF 'journal_delete_durability_boundary(JournalDeleteDurabilityBoundary::DeleteDirectorySynced);' "$$store_delete_code" | timeout 10s cut -d: -f1 )"; \
	final_binding_line="$$( timeout 10s grep -nF 'PublicBindingRevalidationBoundary::BeforeDeleteFinalBinding' "$$store_delete_code" | timeout 10s cut -d: -f1 )"; \
	timeout 10s test "$$unlink_line" -lt "$$unlink_callback_line"; \
	timeout 10s test "$$unlink_callback_line" -lt "$$sync_line"; \
	timeout 10s test "$$sync_line" -lt "$$sync_callback_line"; \
	timeout 10s test "$$sync_callback_line" -lt "$$final_binding_line"; \
	if timeout 10s rg -n 'ensure_|mkdir|chmod|cleanup|create_|renameat|write_all|set_permissions|remove_(dir|file)|directory_entries' "$$store_delete_code"; then exit 1; else status="$$?"; timeout 10s test "$$status" = 1; fi; \
	for file in \
		"$$authority" "$$proof" "$$topology" "$$executor" "$$recovery_root" "$$reconciliation_root" \
		"$$namespace_root" "$$journal_root" "$$journal_store" "$$journal_transactions" "$$orchestrator" "$$startup_gate" "$$startup_tests" \
		"$$executor_tests" "$$executor_support" "$$executor_matrix" "$$executor_delete_report" "$$executor_storage" \
		"$$executor_races" "$$executor_post" "$$executor_binding" \
		misc/make/startup-rollback-finalization-tests.mk misc/make/help.mk Makefile; do \
		timeout 10s test "$$( timeout 10s wc -l < "$$file" )" -le 1000; \
	done; \
	timeout 300s $(CARGO) test -p forge --lib "$$authority_prefix" -- --test-threads=1; \
	timeout 300s $(CARGO) test -p forge --lib "$$executor_prefix" -- --test-threads=1; \
	timeout 300s $(CARGO) test -p forge --lib "$$startup_prefix" -- --test-threads=1; \
	timeout 300s $(CARGO) test -p forge --lib "$$journal_delete_callback_contract" -- --exact --test-threads=1
