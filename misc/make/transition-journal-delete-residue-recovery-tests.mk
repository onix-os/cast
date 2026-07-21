.PHONY: forge-transition-journal-delete-residue-recovery-test

forge-transition-journal-delete-residue-recovery-test:
	@set -eu; \
	listed="$$( $(CARGO) test -p forge --lib -- --list )"; \
	prefix='transition_journal::tests::'; \
	for name in \
		delete_residue_recovery_restores_genuine_bound_delete_residue_on_fresh_reopen \
		delete_residue_recovery_restores_both_deletable_terminal_phases_exactly \
		delete_residue_recovery_rejects_nonterminal_and_corrupt_frames_without_mutation \
		delete_residue_recovery_rejects_malformed_unsafe_and_foreign_inventory_without_mutation \
		delete_residue_recovery_rejects_canonical_coexistence_and_multiple_residues \
		delete_residue_recovery_rejects_same_bytes_different_inode_before_restore \
		delete_residue_recovery_rejects_same_inode_framed_byte_mutation_between_observations \
		delete_residue_recovery_rejects_same_bytes_different_inode_after_restore \
		delete_residue_recovery_revalidates_public_journal_lock_and_inventory_at_every_mutation_seam \
		delete_residue_recovery_restore_faults_are_fresh_reopen_idempotent_without_retry \
		delete_residue_recovery_directory_sync_failure_leaves_exact_canonical_for_reopen \
		delete_residue_recovery_durability_boundaries_follow_restore_then_directory_sync \
		delete_residue_recovery_read_only_inspection_refuses_residue_unchanged; do \
		grep -Fqx "$$prefix$$name: test" <<<"$$listed"; \
	done; \
	test "$$( grep -c '^transition_journal::tests::delete_residue_recovery_.*: test$$' <<<"$$listed" )" = 13; \
	recovery=crates/forge/src/transition_journal/store/delete_residue_recovery.rs; \
	store=crates/forge/src/transition_journal/store.rs; \
	journal=crates/forge/src/transition_journal.rs; \
	tests=crates/forge/src/transition_journal/tests/delete_residue_recovery.rs; \
	method="$$( sed -n '/^    pub(super) fn recover_interrupted_bound_delete(/,/^    fn retain_delete_residue(/p' "$$recovery" | sed '$$d' )"; \
	test "$$( grep -Fc 'renameat2(' <<<"$$method" )" = 1; \
	test "$$( grep -Fc 'nix::libc::RENAME_NOREPLACE' <<<"$$method" )" = 1; \
	test "$$( grep -Fc '.sync_all()' <<<"$$method" )" = 1; \
	if grep -Eq 'unlinkat|remove_file|loop|while' <<<"$$method"; then exit 1; else test "$$?" = 1; fi; \
	grep -Fq 'store.recover_interrupted_bound_delete(cast)?;' "$$store"; \
	grep -Fq 'store.cleanup_stale_temporaries()?;' "$$store"; \
	recovery_line="$$( grep -nF 'store.recover_interrupted_bound_delete(cast)?;' "$$store" | cut -d: -f1 )"; \
	cleanup_line="$$( grep -nF 'store.cleanup_stale_temporaries()?;' "$$store" | cut -d: -f1 )"; \
	test "$$recovery_line" -lt "$$cleanup_line"; \
	grep -Fq 'self.revalidate_retained_cast_binding_locked(cast_directory)?;' "$$recovery"; \
	grep -Fq 'let first = self.observe_delete_residue_layout(cast_directory, retained)?;' "$$recovery"; \
	grep -Fq 'let second = self.observe_delete_residue_layout(cast_directory, retained)?;' "$$recovery"; \
	grep -Fq 'read_bounded(&mut file)' "$$recovery"; \
	grep -Fq 'decode(&framed)' "$$recovery"; \
	grep -Fq 'if !record.phase.deletable() {' "$$recovery"; \
	grep -Fq 'identity != retained.identity || framed != retained.framed' "$$recovery"; \
	grep -Fq 'nix::libc::O_NOATIME' "$$recovery"; \
	grep -Fq 'cooperative same-credential boundary' "$$recovery"; \
	grep -Fq 'No optional work occurs in that window.' "$$recovery"; \
	grep -Fq 'fn valid_delete_name(name: &[u8]) -> bool {' "$$journal"; \
	grep -Fq 'strip_prefix(DELETE_PREFIX)' "$$journal"; \
	for fault in DeleteResidueRestore DeleteResidueRestoreReport DeleteResidueDirectorySync DeleteResidueDirectorySyncReport; do \
		grep -Fq "$$fault" "$$store"; \
		grep -Fq "StorageFaultPoint::$$fault" "$$recovery"; \
	done; \
	grep -Fq 'assert_eq!(cases, 2, "delete-residue terminal phase matrix drifted");' "$$tests"; \
	grep -Fq 'assert_eq!(cases, 12, "delete-residue public seam matrix drifted");' "$$tests"; \
	grep -Fq 'assert_eq!(cases, 2, "delete-residue directory-sync fault matrix drifted");' "$$tests"; \
	for file in "$$store" "$$journal" "$$recovery" crates/forge/src/transition_journal/store/stale_temporary_cleanup.rs "$$tests" misc/make/transition-journal-delete-residue-recovery-tests.mk; do \
		test "$$( wc -l < "$$file" )" -le 1000; \
	done; \
	$(CARGO) test -p forge --lib 'transition_journal::tests::delete_residue_recovery_' -- --test-threads=1
