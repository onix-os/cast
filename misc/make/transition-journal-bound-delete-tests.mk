.PHONY: forge-transition-journal-bound-delete-test

forge-transition-journal-bound-delete-test:
	@set -eu; \
	listed="$$( $(CARGO) test -p forge --lib -- --list )"; \
	prefix='transition_journal::tests::'; \
	for name in \
		bound_record_delete_consumes_exact_terminal_binding_and_returns_clean_locked_store \
		bound_record_delete_rejects_wrong_store_record_phase_and_cast_without_unlink \
		bound_record_delete_same_byte_inode_replacement_at_every_seam_never_deletes_replacement \
		bound_record_delete_public_journal_and_lock_replacement_at_every_seam_fail_closed \
		bound_record_delete_final_publication_sandwich_rejects_observation_gap_replacements \
		bound_record_delete_noreplace_collision_preserves_exact_source_and_foreign_winner \
		bound_record_delete_storage_faults_reconcile_exact_source_or_absence_without_retry \
		bound_record_delete_storage_reconciliation_never_deletes_same_byte_replacement \
		bound_record_delete_rejects_same_inode_record_change_at_both_preunlink_checks \
		bound_record_delete_durability_callbacks_follow_sole_private_unlink_then_sync \
		bound_record_delete_private_residue_is_preserved_and_rejected_on_reopen; do \
		grep -Fqx "$$prefix$$name: test" <<<"$$listed"; \
	done; \
	test "$$( grep -c '^transition_journal::tests::bound_record_delete_.*: test$$' <<<"$$listed" )" = 11; \
	record_binding=crates/forge/src/transition_journal/store/record_binding.rs; \
	store=crates/forge/src/transition_journal/store.rs; \
	journal=crates/forge/src/transition_journal.rs; \
	tests=crates/forge/src/transition_journal/tests/record_binding_delete.rs; \
	method="$$( sed -n '/^    pub(crate) fn delete_record_binding(/,/^    fn reconcile_private_unlink_failure(/p' "$$record_binding" | sed '$$d' )"; \
	delete_code="$$( sed -n '/^    pub(crate) fn delete_record_binding(/,/^fn require_loaded_record_binding(/p' "$$record_binding" | sed '$$d' )"; \
	classification="$$( sed -n '/^    fn classify_bound_record_delete_layout_at_boundary_locked(/,/^    fn observe_bound_record_delete_layout(/p' "$$record_binding" | sed '$$d' )"; \
	rg -U -q '^    pub\(crate\) fn delete_record_binding\(\n        &self,\n        cast_directory: &std::fs::File,\n        expected: TransitionJournalRecordBinding,\n        record: &TransitionRecord,\n    \) -> Result<\(\), TransitionJournalRecordDeleteError> \{' "$$record_binding"; \
	test "$$( grep -Fc 'renameat2(' <<<"$$method" )" = 1; \
	test "$$( grep -Fc 'renameat2(' <<<"$$delete_code" )" = 2; \
	test "$$( grep -Fc 'nix::libc::RENAME_NOREPLACE' <<<"$$delete_code" )" = 2; \
	test "$$( grep -Fc 'unlinkat(self.directory.as_raw_fd(), &private_name)' <<<"$$delete_code" )" = 1; \
	test "$$( grep -Fc '.and_then(|()| self.directory.sync_all())' <<<"$$delete_code" )" = 1; \
	test "$$( grep -Fc '.lock_operation()' <<<"$$method" )" = 1; \
	test "$$( grep -Fc 'self.has_record_store_binding(&expected)' <<<"$$method" )" = 1; \
	test "$$( grep -Fc 'require_loaded_record_binding(&expected, record, &loaded)' <<<"$$method" )" = 2; \
	test "$$( grep -Fc 'drop(expected);' <<<"$$method" )" = 1; \
	pre_fault="$$( grep -nF 'storage_fault(StorageFaultPoint::CanonicalUnlink)' <<<"$$method" | cut -d: -f1 )"; \
	private_boundary="$$( grep -nF 'BeforeBoundDeletePrivateUnlink' <<<"$$method" | cut -d: -f1 )"; \
	private_classification="$$( grep -nF 'match self.classify_bound_record_delete_layout_locked(' <<<"$$method" | tail -n 1 | cut -d: -f1 )"; \
	private_unlink="$$( grep -nF 'let unlink = unlinkat(self.directory.as_raw_fd(), &private_name);' <<<"$$method" | cut -d: -f1 )"; \
	test "$$pre_fault" -lt "$$private_boundary"; \
	test "$$private_boundary" -lt "$$private_classification"; \
	test "$$private_classification" -lt "$$private_unlink"; \
	grep -Fq '// optional callback, fault hook, or other work after the exact-private' "$$record_binding"; \
	grep -Fq '// writer replacing this private name inside the final compare/unlink' "$$record_binding"; \
	test "$$( grep -Fc 'self.revalidate_retained_cast_binding_locked(cast_directory)?' <<<"$$classification" )" = 2; \
	test "$$( grep -Fc 'self.observe_bound_record_delete_layout(&journal, loaded, private_name)?' <<<"$$classification" )" = 2; \
	grep -Fq 'if first != second {' <<<"$$classification"; \
	grep -Fq 'BeforeBoundDeletePublicationFinalBinding' <<<"$$method"; \
	for boundary in \
		BeforeBoundDeleteAdmission \
		BeforeBoundDeleteDetach \
		BeforeBoundDeletePrivateUnlink \
		AfterBoundDeleteUnlink \
		BeforeBoundDeleteFailureReconciliation \
		BeforeBoundDeletePublication \
		BeforeBoundDeletePublicationFinalBinding; do \
		grep -Fq "$$boundary" "$$store"; \
		grep -Fq "PublicBindingRevalidationBoundary::$$boundary" "$$record_binding"; \
	done; \
	for fault in BoundDeleteDetach BoundDeleteDetachReport CanonicalUnlink BoundDeleteUnlinkReport DeleteDirectorySync; do \
		grep -Fq "$$fault" "$$store"; \
		grep -Fq "StorageFaultPoint::$$fault" "$$record_binding"; \
	done; \
	grep -Fq 'pub(crate) enum TransitionJournalRecordDeleteState {' "$$record_binding"; \
	grep -Fq '    ExactSource,' "$$record_binding"; \
	grep -Fq '    Absent,' "$$record_binding"; \
	grep -Fq 'TransitionJournalRecordDeleteError::StorageAndReconciliation {' "$$record_binding"; \
	grep -Fq 'const DELETE_PREFIX: &[u8] = b".state-transition.delete-";' "$$journal"; \
	valid_temporary="$$( sed -n '/^fn valid_temporary_name(/,/^fn directory_entries(/p' "$$journal" | sed '$$d' )"; \
	grep -Fq 'strip_prefix(TEMPORARY_PREFIX)' <<<"$$valid_temporary"; \
	if grep -Fq 'DELETE_PREFIX' <<<"$$valid_temporary"; then exit 1; fi; \
	grep -Fq 'const ALL: [Self; 5] = [' "$$tests"; \
	grep -Fq 'assert_eq!(cases, 2, "bound-delete terminal phase matrix drifted");' "$$tests"; \
	grep -Fq 'assert_eq!(cases, 5, "bound-delete canonical seam matrix drifted");' "$$tests"; \
	grep -Fq 'assert_eq!(cases, 10, "bound-delete public identity seam matrix drifted");' "$$tests"; \
	grep -Fq 'assert_eq!(cases, 2, "bound-delete final public sandwich matrix drifted");' "$$tests"; \
	grep -Fq 'assert_eq!(cases, 5, "bound-delete storage reconciliation matrix drifted");' "$$tests"; \
	grep -Fq 'assert_eq!(cases, 5, "bound-delete ambiguous storage replacement matrix drifted");' "$$tests"; \
	grep -Fq 'assert_eq!(cases, 2, "bound-delete same-inode pre-unlink matrix drifted");' "$$tests"; \
	grep -Fq 'assert_eq!(cases, 2, "bound-delete durability callback matrix drifted");' "$$tests"; \
	if rg -U -n '#\[derive\([^]]*Clone[^]]*\)\]\n(?:#\[[^\n]*\]\n)*pub\(crate\) struct TransitionJournalRecordBinding' "$$record_binding"; then exit 1; else test "$$?" = 1; fi; \
	if rg -n 'open_in_retained_cast|TransitionJournalStore::open|fs_err|remove_file|loop|while|PathBuf|Path::' <<<"$$delete_code"; then exit 1; else test "$$?" = 1; fi; \
	for file in "$$record_binding" "$$store" "$$journal" "$$tests" misc/make/transition-journal-bound-delete-tests.mk; do \
		test "$$( wc -l < "$$file" )" -le 1000; \
	done; \
	$(CARGO) test -p forge --lib 'transition_journal::tests::bound_record_delete_' -- --test-threads=1
