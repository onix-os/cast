ACTIVE_REBLIT_BOOT_COMPLETE_FINALIZATION_TOP_DIR := $(abspath $(dir $(lastword $(MAKEFILE_LIST)))/../..)
CARGO ?= cargo

.PHONY: forge-active-reblit-boot-complete-finalization-test

ACTIVE_REBLIT_BOOT_COMPLETE_FINALIZATION_FILTER ?= client::active_reblit_boot_publication_preflight::immutable_attempt::tests::receipt_promotion::completion::commit_cleanup::complete::finalization::

forge-active-reblit-boot-complete-finalization-test: forge-startup-active-reblit-complete-finalization-test
	@set -euo pipefail; \
	mkdir -p "$(ACTIVE_REBLIT_BOOT_COMPLETE_FINALIZATION_TOP_DIR)/target"; \
	listed="$$( mktemp "$(ACTIVE_REBLIT_BOOT_COMPLETE_FINALIZATION_TOP_DIR)/target/active-reblit-boot-complete-finalization-list.XXXXXXXXXXXX" )"; \
	trap 'rm -f "$$listed"' EXIT; \
	$(CARGO) test --manifest-path "$(ACTIVE_REBLIT_BOOT_COMPLETE_FINALIZATION_TOP_DIR)/Cargo.toml" -p forge --lib -- --list | tee "$$listed" >/dev/null; \
	prefix='$(ACTIVE_REBLIT_BOOT_COMPLETE_FINALIZATION_FILTER)'; \
	test "$$( grep -Ec "^$$prefix.*: test$$" "$$listed" )" = 3; \
	for name in exact_complete_finalizes_once_and_preserves_clean_authority same_bytes_new_inode_rejects_finalization_before_delete_without_other_effects terminal_delete_fault_states_return_no_clean_handoff; do \
		grep -Fqx "$$prefix$$name: test" "$$listed"; \
	done; \
	forge_root="$(ACTIVE_REBLIT_BOOT_COMPLETE_FINALIZATION_TOP_DIR)/crates/forge/src"; \
	attempt="$$forge_root/client/boot/active_reblit_boot_publication_preflight/immutable_attempt.rs"; \
	outer="$$forge_root/client/boot/active_reblit_boot_publication_preflight/immutable_attempt/receipt_promotion/boot_sync_completion/commit_decision/commit_cleanup/complete/finalization.rs"; \
	inner="$$forge_root/client/boot/active_reblit_boot_sync_staging/boot_sync_complete_persistence/commit_decision_handoff/commit_cleanup_handoff/complete_handoff/finalization.rs"; \
	authority="$$forge_root/client/startup_reconciliation/active_reblit_complete_finalization_authority.rs"; \
	retained_authority="$$forge_root/client/startup_reconciliation/active_reblit_complete_finalization_authority/retained_binding.rs"; \
	recovery="$$forge_root/client/startup_recovery/active_reblit_complete_finalization.rs"; \
	startup_gate="$$forge_root/client/startup_gate.rs"; \
	tests="$$forge_root/client/boot/active_reblit_boot_publication_preflight/immutable_attempt/receipt_promotion/tests/completion/commit_cleanup/complete/finalization.rs"; \
	seal_mints="$$( rg -n -F 'ActiveReblitBootCompleteFinalizationSeal { _private: () }' "$$forge_root" --glob '*.rs' --glob '!**/tests/**' --glob '!**/tests.rs' --glob '!**/*_tests.rs' --glob '!**/*_test.rs' )"; \
	test "$$( grep -c . <<<"$$seal_mints" )" = 1; \
	grep -Fq "$$outer:" <<<"$$seal_mints"; \
	if rg --pcre2 -U -n '#\[derive\([^]]*(?:Clone|Copy)[^]]*\)\]\s*pub\(in crate::client\) struct (?:ActiveReblitBootCompleteFinalizationSeal|ActiveReblitBootFinalizedHandoff|FinalizedStagedActiveReblitBootSync)|impl(?:<[^>]+>)?\s+(?:Clone|Copy)\s+for\s+(?:ActiveReblitBootCompleteFinalizationSeal|ActiveReblitBootFinalizedHandoff|FinalizedStagedActiveReblitBootSync)' "$$attempt" "$$outer" "$$inner"; then exit 1; else status="$$?"; test "$$status" = 1; fi; \
	if rg -n 'pub(?:\(in crate::client\))? (?:const )?fn (?:new|from_parts|into_parts)\(' "$$outer" "$$inner"; then exit 1; else status="$$?"; test "$$status" = 1; fi; \
	test "$$( grep -Fc 'pub(in crate::client) fn capture_retained_binding' "$$retained_authority" )" = 1; \
	grep -Fq 'record.generation != 15' "$$retained_authority"; \
	grep -Fq 'ActiveReblitCompleteFinalizationCapture::NotApplicable' "$$retained_authority"; \
	grep -Fq 'ActiveReblitCompleteFinalizationCapture::Deferred => Err(' "$$retained_authority"; \
	test "$$( grep -Fc 'finalize_active_reblit_complete(journal, authority)' "$$inner" )" = 1; \
	test "$$( grep -Fc 'CleanSystemStartup::admit_clean_after_terminal_finalization(' "$$inner" )" = 1; \
	grep -Fq 'pub(in crate::client) fn admit_clean_after_terminal_finalization(' "$$startup_gate"; \
	if rg -n 'CleanSystemStartup::enter\(' "$$inner" "$$outer"; then exit 1; else status="$$?"; test "$$status" = 1; fi; \
	test "$$( grep -Fc '.delete_record_binding(' "$$authority" )" = 1; \
	test "$$( grep -Fc 'revalidate_after_journal_delete(&journal)' "$$recovery" )" = 2; \
	rg --pcre2 -U -q 'clean_startup: CleanSystemStartup,\s*active_state_reservation: CoordinatorActiveStateReservation,' "$$inner"; \
	if rg --pcre2 -n '[.](?:advance_record_binding(?:_until)?|advance|promote_boot_publication_receipt|stage_boot_publication_receipt|clear_boot_publication_receipt|delete_boot_publication_receipt|publish_preflighted_immutable_leaf|publish_immutable_boot_file_until|replace_exact_boot_file_until|cleanup_replaced_boot_file_sidecar_until|cleanup_restored_boot_file_sidecar_until|cleanup_authenticated_stale_boot_file_until)\(|(?:postblit::triggers|execute_trigger_directly|before_ephemeral_system_triggers|after_ephemeral_system_triggers)\(|system_trigger_container::|reopen_canonical_journal|try_reopen_canonical_journal|TransitionJournalStore::(?:open|open_in_retained_cast|try_open_in_retained_cast)|input_deadline\(|advance_.*_until|Command::new|(?:nix::|libc::)(?:mount|umount)' "$$outer" "$$inner" "$$retained_authority" "$$recovery"; then \
		printf '%s\n' 'live terminal finalization contains an unrelated advance, receipt, boot, trigger, reopen, deadline, command, or mount effect' >&2; exit 1; \
	else \
		status="$$?"; test "$$status" = 1; \
	fi; \
	for file in "$$attempt" "$$outer" "$$inner" "$$authority" "$$retained_authority" "$$recovery" "$$startup_gate" "$$tests" "$(ACTIVE_REBLIT_BOOT_COMPLETE_FINALIZATION_TOP_DIR)/misc/make/active-reblit-boot-complete-finalization-tests.mk"; do \
		test "$$( wc -l < "$$file" )" -le 1000; \
	done
	@$(CARGO) test --manifest-path "$(ACTIVE_REBLIT_BOOT_COMPLETE_FINALIZATION_TOP_DIR)/Cargo.toml" -p forge --lib "$(ACTIVE_REBLIT_BOOT_COMPLETE_FINALIZATION_FILTER)" -- --test-threads=1
