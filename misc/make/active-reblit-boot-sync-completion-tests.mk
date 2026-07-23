ACTIVE_REBLIT_BOOT_SYNC_COMPLETION_TOP_DIR := $(abspath $(dir $(lastword $(MAKEFILE_LIST)))/../..)
CARGO ?= cargo

.PHONY: forge-active-reblit-boot-sync-completion-test forge-active-reblit-boot-commit-decision-test forge-active-reblit-boot-commit-cleanup-test

ACTIVE_REBLIT_BOOT_COMMIT_DECISION_FILTER ?= client::active_reblit_boot_publication_preflight::immutable_attempt::tests::receipt_promotion::completion::commit_decision::

forge-active-reblit-boot-commit-decision-test:
	@$(CARGO) test --manifest-path "$(ACTIVE_REBLIT_BOOT_SYNC_COMPLETION_TOP_DIR)/Cargo.toml" -p forge --lib "$(ACTIVE_REBLIT_BOOT_COMMIT_DECISION_FILTER)" -- --test-threads=1 --include-ignored

ACTIVE_REBLIT_BOOT_COMMIT_CLEANUP_FILTER ?= client::active_reblit_boot_publication_preflight::immutable_attempt::tests::receipt_promotion::completion::commit_cleanup::

forge-active-reblit-boot-commit-cleanup-test: forge-startup-active-reblit-commit-cleanup-authority-test forge-startup-active-reblit-commit-cleanup-effect-test forge-startup-active-reblit-commit-cleanup-dispatch-test forge-startup-active-reblit-commit-cleanup-complete-test
	@set -euo pipefail; \
	forge_root="$(ACTIVE_REBLIT_BOOT_SYNC_COMPLETION_TOP_DIR)/crates/forge/src"; \
	attempt="$$forge_root/client/boot/active_reblit_boot_publication_preflight/immutable_attempt.rs"; \
	cleanup_outer="$$forge_root/client/boot/active_reblit_boot_publication_preflight/immutable_attempt/receipt_promotion/boot_sync_completion/commit_decision/commit_cleanup.rs"; \
	complete_outer="$$forge_root/client/boot/active_reblit_boot_publication_preflight/immutable_attempt/receipt_promotion/boot_sync_completion/commit_decision/commit_cleanup/complete.rs"; \
	cleanup_staging="$$forge_root/client/boot/active_reblit_boot_sync_staging/boot_sync_complete_persistence/commit_decision_handoff/commit_cleanup_handoff.rs"; \
	complete_staging="$$forge_root/client/boot/active_reblit_boot_sync_staging/boot_sync_complete_persistence/commit_decision_handoff/commit_cleanup_handoff/complete_handoff.rs"; \
	cleanup_authority="$$forge_root/client/startup_reconciliation/active_reblit_commit_cleanup_authority.rs"; \
	cleanup_persistence="$$forge_root/client/startup_recovery/active_reblit_commit_cleanup_complete.rs"; \
	complete_authority="$$forge_root/client/startup_reconciliation/active_reblit_commit_cleanup_complete_authority.rs"; \
	complete_retained_authority="$$forge_root/client/startup_reconciliation/active_reblit_commit_cleanup_complete_authority/retained_binding.rs"; \
	complete_persistence="$$forge_root/client/startup_recovery/active_reblit_commit_cleanup_complete_to_complete.rs"; \
	complete_seal_mints="$$( rg -n -F 'ActiveReblitCommitCleanupCompleteSeal { _private: () }' "$$forge_root" --glob '*.rs' --glob '!**/tests/**' --glob '!**/tests.rs' --glob '!**/*_tests.rs' --glob '!**/*_test.rs' )"; \
	test "$$( grep -c . <<<"$$complete_seal_mints" )" = 1; \
	grep -Fq "$$complete_outer:" <<<"$$complete_seal_mints"; \
	if rg --pcre2 -U -n '#\[derive\([^]]*(?:Clone|Copy)[^]]*\)\]\s*pub\(in crate::client\) struct (?:ActiveReblitCommitCleanupCompleteSeal|ActiveReblitBootCompleteHandoff|CompleteStagedActiveReblitBootSync)|impl(?:<[^>]+>)?\s+(?:Clone|Copy)\s+for\s+(?:ActiveReblitCommitCleanupCompleteSeal|ActiveReblitBootCompleteHandoff|CompleteStagedActiveReblitBootSync)' "$$attempt" "$$complete_outer" "$$complete_staging"; then exit 1; else status="$$?"; test "$$status" = 1; fi; \
	if rg -n 'pub(?:\(in crate::client\))? (?:const )?fn (?:new|from_parts|into_parts)\(' "$$complete_outer" "$$complete_staging"; then exit 1; else status="$$?"; test "$$status" = 1; fi; \
	test "$$( grep -Fc 'pub(in crate::client) fn capture_retained_binding' "$$complete_retained_authority" )" = 1; \
	grep -Fq 'record.generation != 14' "$$complete_retained_authority"; \
	grep -Fq 'ActiveReblitCommitCleanupCompleteCapture::Apply => Err(' "$$complete_retained_authority"; \
	grep -Fq 'CanonicalReopenMode::RetainedNonBlocking => try_reopen_canonical_journal(&installation)' "$$complete_persistence"; \
	test "$$( grep -Fc 'authority.advance_to_complete(&journal)' "$$complete_persistence" )" = 1; \
	if rg -n 'advance_record_binding_until|advance_to_complete_until|ActiveReblitBootCommitDecisionFinalValidation|input_deadline\(' "$$complete_outer" "$$complete_staging" "$$complete_retained_authority" "$$complete_persistence"; then \
		printf '%s\n' 'live Complete roll-forward may not add callback, deadline, or until-based advance authority' >&2; exit 1; \
	else \
		status="$$?"; test "$$status" = 1; \
	fi; \
	if rg --pcre2 -n '[.](?:publish_from_staged_authority|publish_preflighted_immutable_leaf|publish_immutable_boot_file_until|replace_exact_boot_file_until|cleanup_replaced_boot_file_sidecar_until|cleanup_restored_boot_file_sidecar_until|cleanup_authenticated_stale_boot_file_until|stage_boot_publication_receipt|promote_boot_publication_receipt|clear_boot_publication_receipt|delete_boot_publication_receipt|delete_record_binding(?:_until)?)\(|(?:postblit::triggers|execute_trigger_directly|before_ephemeral_system_triggers|after_ephemeral_system_triggers)\(|system_trigger_container::|finalize_active_reblit|ActiveReblitCompleteFinalization|remove_(?:file|dir)|unlinkat|renameat|Command::new|(?:nix::|libc::)(?:mount|umount)' "$$cleanup_outer" "$$complete_outer" "$$cleanup_staging" "$$complete_staging" "$$cleanup_authority" "$$complete_authority" "$$complete_retained_authority" "$$cleanup_persistence" "$$complete_persistence"; then \
		printf '%s\n' 'live commit cleanup contains an unrelated boot, receipt, journal-delete, trigger, finalization, command, or mount effect' >&2; exit 1; \
	else \
		status="$$?"; test "$$status" = 1; \
	fi
	@$(CARGO) test --manifest-path "$(ACTIVE_REBLIT_BOOT_SYNC_COMPLETION_TOP_DIR)/Cargo.toml" -p forge --lib "$(ACTIVE_REBLIT_BOOT_COMMIT_CLEANUP_FILTER)" -- --test-threads=1 --include-ignored

forge-active-reblit-boot-sync-completion-test: forge-active-reblit-boot-terminal-promotion-test forge-transition-journal-successor-test forge-transition-journal-test forge-active-reblit-boot-commit-cleanup-test
	@set -euo pipefail; \
	mkdir -p "$(ACTIVE_REBLIT_BOOT_SYNC_COMPLETION_TOP_DIR)/target"; \
	listed="$$( mktemp "$(ACTIVE_REBLIT_BOOT_SYNC_COMPLETION_TOP_DIR)/target/active-reblit-boot-sync-completion-list.XXXXXXXXXXXX" )"; \
	trap 'rm -f "$$listed"' EXIT; \
	$(CARGO) test --manifest-path "$(ACTIVE_REBLIT_BOOT_SYNC_COMPLETION_TOP_DIR)/Cargo.toml" -p forge --lib -- --list | tee "$$listed" >/dev/null; \
	test -s "$$listed"; \
	prefix='client::active_reblit_boot_publication_preflight::immutable_attempt::tests::receipt_promotion::completion::'; \
	test "$$( grep -Ec "^$$prefix.*: test$$" "$$listed" )" = 24; \
	for name in \
		completion_behavioral_scenario_inventory_is_exactly_forty \
		commit_cleanup::complete::complete_reopen_never_waits_behind_writer_blocked_journal_contender \
		commit_cleanup::complete::exact_cleanup_complete_rolls_forward_once_and_preserves_all_authority \
		commit_cleanup::complete::same_bytes_new_inode_rejects_complete_without_any_effect \
		commit_cleanup::complete::uncertain_complete_advance_returns_no_handoff \
		commit_cleanup::commit_cleanup_reopen_never_waits_behind_writer_blocked_journal_contender \
		commit_cleanup::exact_commit_decision_applies_cleanup_once_and_retains_writer_authority \
		commit_cleanup::replaced_binding_and_uncertain_advance_return_no_cleanup_handoff \
		commit_decision::bound_terminal_validation_is_sandwiched_by_authority_revalidation \
		commit_decision::commit_decision_reopen_never_waits_behind_writer_blocked_journal_contender \
		commit_decision::exact_completion_persists_one_commit_decision_and_retains_writer_authority \
		commit_decision::inner_output_drift_and_deadline_expiry_never_reach_commit_decision \
		commit_decision::uncertain_commit_decision_fault_returns_no_handoff \
		commit_decision::wrong_client_state_and_record_are_rejected_before_commit_decision \
		deadline::inherited_completion_deadline_expires_without_journal_advance_or_token \
		drift::final_return_revalidation_catches_late_drift_after_durable_completion \
		drift::post_advance_namespace_database_journal_and_plan_drift_returns_no_completion_token \
		drift::pre_advance_wrong_client_and_four_drift_axes_never_reach_boot_sync_complete \
		reconciliation::completion_journal_faults_reconcile_only_exact_started_or_complete_without_token \
		reconciliation::completion_reopens_never_wait_behind_a_writer_blocked_journal_contender \
		reconciliation::completion_reconciliation_rejects_representative_wrong_generation_record \
		success::cleaned_promoted_typestate_preserves_exact_authority \
		success::chained_already_promoted_completion_preserves_pair_bodies_and_outputs \
		success::first_adoption_completion_persists_only_exact_boot_sync_complete; do \
		grep -Fqx "$$prefix$$name: test" "$$listed"; \
	done; \
	forge_root="$(ACTIVE_REBLIT_BOOT_SYNC_COMPLETION_TOP_DIR)/crates/forge/src"; \
	attempt="$$forge_root/client/boot/active_reblit_boot_publication_preflight/immutable_attempt.rs"; \
	promotion="$$forge_root/client/boot/active_reblit_boot_publication_preflight/immutable_attempt/receipt_promotion.rs"; \
	outer="$$forge_root/client/boot/active_reblit_boot_publication_preflight/immutable_attempt/receipt_promotion/boot_sync_completion.rs"; \
	commit_outer="$$forge_root/client/boot/active_reblit_boot_publication_preflight/immutable_attempt/receipt_promotion/boot_sync_completion/commit_decision.rs"; \
	cleanup_outer="$$forge_root/client/boot/active_reblit_boot_publication_preflight/immutable_attempt/receipt_promotion/boot_sync_completion/commit_decision/commit_cleanup.rs"; \
	complete_outer="$$forge_root/client/boot/active_reblit_boot_publication_preflight/immutable_attempt/receipt_promotion/boot_sync_completion/commit_decision/commit_cleanup/complete.rs"; \
	terminal="$$forge_root/client/boot/active_reblit_boot_publication_preflight/immutable_attempt/receipt_promotion/terminal_evidence.rs"; \
	evidence="$$forge_root/client/boot/active_reblit_boot_publication_preflight/immutable_attempt/effect_evidence.rs"; \
	staging="$$forge_root/client/boot/active_reblit_boot_sync_staging/boot_sync_complete_persistence.rs"; \
	commit_staging="$$forge_root/client/boot/active_reblit_boot_sync_staging/boot_sync_complete_persistence/commit_decision_handoff.rs"; \
	cleanup_staging="$$forge_root/client/boot/active_reblit_boot_sync_staging/boot_sync_complete_persistence/commit_decision_handoff/commit_cleanup_handoff.rs"; \
	complete_staging="$$forge_root/client/boot/active_reblit_boot_sync_staging/boot_sync_complete_persistence/commit_decision_handoff/commit_cleanup_handoff/complete_handoff.rs"; \
	staging_parent="$$forge_root/client/boot/active_reblit_boot_sync_staging.rs"; \
	commit_persistence="$$forge_root/client/startup_recovery/active_reblit_boot_sync_commit_decision.rs"; \
	commit_authority="$$forge_root/client/startup_reconciliation/active_reblit_boot_sync_complete_authority.rs"; \
	cleanup_authority="$$forge_root/client/startup_reconciliation/active_reblit_commit_cleanup_authority.rs"; \
	cleanup_persistence="$$forge_root/client/startup_recovery/active_reblit_commit_cleanup_complete.rs"; \
	complete_authority="$$forge_root/client/startup_reconciliation/active_reblit_commit_cleanup_complete_authority.rs"; \
	complete_retained_authority="$$forge_root/client/startup_reconciliation/active_reblit_commit_cleanup_complete_authority/retained_binding.rs"; \
	complete_persistence="$$forge_root/client/startup_recovery/active_reblit_commit_cleanup_complete_to_complete.rs"; \
	canonical_reopen="$$forge_root/client/startup_recovery/canonical_journal_reopen.rs"; \
	seal_mints="$$( rg -n -F 'ActiveReblitBootSyncCompletionSeal { _private: () }' "$$forge_root" \
		--glob '*.rs' \
		--glob '!**/tests/**' \
		--glob '!**/tests.rs' \
		--glob '!**/*_tests.rs' \
		--glob '!**/*_test.rs' )"; \
	test "$$( grep -c . <<<"$$seal_mints" )" = 1; \
	grep -Fq "$$outer:" <<<"$$seal_mints"; \
	commit_seal_mints="$$( rg -n -F 'ActiveReblitBootSyncCommitDecisionSeal { _private: () }' "$$forge_root" \
		--glob '*.rs' \
		--glob '!**/tests/**' \
		--glob '!**/tests.rs' \
		--glob '!**/*_tests.rs' \
		--glob '!**/*_test.rs' )"; \
	test "$$( grep -c . <<<"$$commit_seal_mints" )" = 1; \
	grep -Fq "$$commit_outer:" <<<"$$commit_seal_mints"; \
	cleanup_seal_mints="$$( rg -n -F 'ActiveReblitCommitCleanupSeal { _private: () }' "$$forge_root" \
		--glob '*.rs' \
		--glob '!**/tests/**' \
		--glob '!**/tests.rs' \
		--glob '!**/*_tests.rs' \
		--glob '!**/*_test.rs' )"; \
	test "$$( grep -c . <<<"$$cleanup_seal_mints" )" = 1; \
	grep -Fq "$$cleanup_outer:" <<<"$$cleanup_seal_mints"; \
	if rg --pcre2 -U -n '#\[derive\([^]]*(?:Clone|Copy)[^]]*\)\]\s*pub\(in crate::client\) struct (?:ActiveReblitBootSyncCommitDecisionSeal|ActiveReblitBootCommitDecisionFinalValidation|ActiveReblitBootCommitDecisionHandoff|CommittedStagedActiveReblitBootSync)|impl(?:<[^>]+>)?\s+(?:Clone|Copy)\s+for\s+(?:ActiveReblitBootSyncCommitDecisionSeal|ActiveReblitBootCommitDecisionFinalValidation|ActiveReblitBootCommitDecisionHandoff|CommittedStagedActiveReblitBootSync)' "$$attempt" "$$commit_outer" "$$commit_staging"; then exit 1; else status="$$?"; test "$$status" = 1; fi; \
	if rg -n 'pub(?:\(in crate::client\))? (?:const )?fn (?:new|from_parts|into_parts)\(' "$$commit_outer" "$$commit_staging"; then exit 1; else status="$$?"; test "$$status" = 1; fi; \
	if rg --pcre2 -U -n '#\[derive\([^]]*(?:Clone|Copy)[^]]*\)\]\s*pub\(in crate::client\) struct (?:ActiveReblitCommitCleanupSeal|ActiveReblitBootCommitCleanupCompleteHandoff|CommitCleanupCompleteStagedActiveReblitBootSync)|impl(?:<[^>]+>)?\s+(?:Clone|Copy)\s+for\s+(?:ActiveReblitCommitCleanupSeal|ActiveReblitBootCommitCleanupCompleteHandoff|CommitCleanupCompleteStagedActiveReblitBootSync)' "$$attempt" "$$cleanup_outer" "$$cleanup_staging"; then exit 1; else status="$$?"; test "$$status" = 1; fi; \
	if rg -n 'pub(?:\(in crate::client\))? (?:const )?fn (?:new|from_parts|into_parts)\(' "$$cleanup_outer" "$$cleanup_staging"; then exit 1; else status="$$?"; test "$$status" = 1; fi; \
	test "$$( grep -Fc 'pub(in crate::client) fn capture_retained_binding' "$$cleanup_authority" )" = 1; \
	grep -Fq 'record.generation != 13' "$$cleanup_authority"; \
	grep -Fq 'ActiveReblitCommitCleanupAdmission::Finish(_) => Err(' "$$cleanup_authority"; \
	grep -Fq 'CanonicalReopenMode::RetainedNonBlocking => try_reopen_canonical_journal(&installation)' "$$cleanup_persistence"; \
	test "$$( grep -Fc 'let final_validation = ActiveReblitBootCommitDecisionFinalValidation {' "$$commit_outer" )" = 1; \
	test "$$( grep -Fc '.advance_record_binding_until(' "$$commit_authority" )" = 1; \
	bound_advance_body="$$( sed -n '/fn advance_record_binding_after_final_validation(/,/fn advance_record_binding_inner(/p' "$$commit_authority" )"; \
	test -n "$$bound_advance_body"; \
	test "$$( grep -Fc 'self.revalidate(journal)?;' <<<"$$bound_advance_body" )" = 2; \
	test "$$( grep -Fc 'exact_commit_decided_successor(' <<<"$$bound_advance_body" )" = 2; \
	test "$$( grep -Fc '.validate()' <<<"$$bound_advance_body" )" = 1; \
	test "$$( grep -Fc 'after_active_reblit_boot_commit_decision_bound_terminal_validation();' <<<"$$bound_advance_body" )" = 1; \
	if rg -n '[.]promote_boot_publication_receipt\(|[.]publish_preflighted_immutable_leaf\(|[.]delete_record_binding\(|remove_(?:file|dir)|unlinkat|renameat|Command::new|nix::mount|libc::mount' "$$commit_outer" "$$commit_staging" "$$commit_persistence" "$$commit_authority" "$$canonical_reopen"; then exit 1; else status="$$?"; test "$$status" = 1; fi; \
	test "$$( grep -Fc 'let reopened = try_reopen_canonical_journal(&installation)' "$$commit_persistence" )" = 1; \
	test "$$( grep -Fc 'let journal = TransitionJournalStore::try_open_in_retained_cast(cast, &installation.root)?;' "$$canonical_reopen" )" = 1; \
	if grep -nF 'TransitionJournalStore::open_in_retained_cast(' "$$commit_persistence"; then \
		printf '%s\n' 'reservation-owning commit decision may not block reopening the journal' >&2; exit 1; \
	else \
		status="$$?"; test "$$status" = 1; \
	fi; \
	test "$$( grep -Fc 'pub(in crate::client) fn persist_boot_sync_complete(' "$$outer" )" = 1; \
	test "$$( grep -Fc 'pub(in crate::client) fn persist_boot_sync_complete(' "$$staging" )" = 1; \
	grep -Fq 'pub(in crate::client) struct CleanedPromotedExactActiveReblitBootPublication<' "$$promotion"; \
	grep -Fq 'promoted: PromotedExactActiveReblitBootPublication<' "$$promotion"; \
	grep -Fq 'pub(in crate::client) fn try_into_cleaned(' "$$promotion"; \
	grep -Fq 'Err(self)' "$$promotion"; \
	grep -Fq 'Ok(CleanedPromotedExactActiveReblitBootPublication { promoted: self })' "$$promotion"; \
	grep -Fq 'CleanedPromotedExactActiveReblitBootPublication {' "$$outer"; \
	if rg -n 'CleanupRequired|require_cleaned_promoted' "$$promotion" "$$outer"; then exit 1; else status="$$?"; test "$$status" = 1; fi; \
	grep -Fq 'ActiveReblitBootPublicationDeltaAction::ReplaceOwnedDesired' "$$terminal"; \
	grep -Fq 'ReplacedOwned {' "$$evidence"; \
	grep -Fq '.revalidate_promoted_against(client)' "$$outer"; \
	if rg -n '[.]staged[.]plan|[.]terminal[.]staged[.]plan' "$$outer"; then exit 1; else status="$$?"; test "$$status" = 1; fi; \
	test "$$( grep -Fc '.boot_sync_complete_successor(pair)' "$$staging" )" = 1; \
	test "$$( grep -Fc '.advance_record_binding_until(' "$$staging" )" = 1; \
	test "$$( grep -Fc 'Ok(fresh.plan().input_deadline())' "$$staging" )" = 1; \
	test "$$( grep -Fc 'let deadline = require_promoted_staging_admission(&self, client)?;' "$$staging" )" = 1; \
	test "$$( grep -Fc 'let repeated_deadline = require_promoted_staging_admission(&self, client)?;' "$$staging" )" = 1; \
	rg --pcre2 -U -q '[.]advance_record_binding_until\(\s*cast,\s*predecessor_binding,\s*&successor,\s*deadline,\s*\)' "$$staging"; \
	success_body="$$( sed -n '/^fn finish_successful_completion_advance(/,/^}/p' "$$staging" )"; \
	test -n "$$success_body"; \
	test "$$( grep -Fc 'validate_completed_successor(' <<<"$$success_body" )" = 2; \
	test "$$( grep -Fc 'drop(journal);' <<<"$$success_body" )" = 1; \
	test "$$( grep -Fc 'TransitionJournalStore::try_open_in_retained_cast(' <<<"$$success_body" )" = 1; \
	test "$$( grep -Fc 'TransitionJournalStore::try_open_in_retained_cast(' "$$staging" )" = 2; \
	test "$$( grep -Fc 'before_completion_journal_reopen();' "$$staging" )" = 2; \
	if grep -nF 'TransitionJournalStore::open_in_retained_cast' "$$staging"; then \
		printf '%s\n' 'reservation-owning completion may not block reopening the journal' >&2; exit 1; \
	else \
		status="$$?"; test "$$status" = 1; \
	fi; \
	test "$$( grep -Fc '.has_reopened_record_binding(cast, &successor_binding, successor)' <<<"$$success_body" )" = 1; \
	test "$$( grep -Fc '.record_binding(cast, successor)' <<<"$$success_body" )" = 1; \
	test "$$( grep -Fc 'drop(successor_binding);' <<<"$$success_body" )" = 1; \
	initial_validation_line="$$( grep -nF 'validate_completed_successor(' <<<"$$success_body" | head -n 1 | cut -d: -f1 )"; \
	drop_store_line="$$( grep -nF 'drop(journal);' <<<"$$success_body" | cut -d: -f1 )"; \
	reopen_line="$$( grep -nF 'TransitionJournalStore::try_open_in_retained_cast(' <<<"$$success_body" | cut -d: -f1 )"; \
	old_inode_line="$$( grep -nF '.has_reopened_record_binding(cast, &successor_binding, successor)' <<<"$$success_body" | cut -d: -f1 )"; \
	recapture_line="$$( grep -nF '.record_binding(cast, successor)' <<<"$$success_body" | cut -d: -f1 )"; \
	drop_old_binding_line="$$( grep -nF 'drop(successor_binding);' <<<"$$success_body" | cut -d: -f1 )"; \
	final_validation_line="$$( grep -nF 'validate_completed_successor(' <<<"$$success_body" | tail -n 1 | cut -d: -f1 )"; \
	test "$$initial_validation_line" -lt "$$drop_store_line"; \
	test "$$drop_store_line" -lt "$$reopen_line"; \
	test "$$reopen_line" -lt "$$old_inode_line"; \
	test "$$old_inode_line" -lt "$$recapture_line"; \
	test "$$recapture_line" -lt "$$drop_old_binding_line"; \
	test "$$drop_old_binding_line" -lt "$$final_validation_line"; \
	grep -Fq 'DurableActiveReblitBootSyncCompletionRecord::BootSyncStarted' "$$staging"; \
	grep -Fq 'DurableActiveReblitBootSyncCompletionRecord::BootSyncComplete' "$$staging"; \
	if rg --pcre2 -U -n '#\[derive\([^]]*(?:Clone|Copy)[^]]*\)\]\s*pub\(in crate::client\) struct (?:ActiveReblitBootSyncCompletionSeal|CleanedPromotedExactActiveReblitBootPublication|CompletedStagedActiveReblitBootSync|FreshCompletedStagedActiveReblitBootSync|CompletedExactActiveReblitBootPublication)|impl(?:<[^>]+>)?\s+(?:Clone|Copy)\s+for\s+(?:ActiveReblitBootSyncCompletionSeal|CleanedPromotedExactActiveReblitBootPublication|CompletedStagedActiveReblitBootSync|FreshCompletedStagedActiveReblitBootSync|CompletedExactActiveReblitBootPublication)' "$$attempt" "$$promotion" "$$outer" "$$staging"; then exit 1; else status="$$?"; test "$$status" = 1; fi; \
	if rg -n '[.]advance_record_binding\(|Phase::CommitDecided|[.]promote_boot_publication_receipt\(|[.]publish_preflighted_immutable_leaf\(|[.]delete_record_binding\(|remove_(?:file|dir)|unlinkat|renameat|Command::new|nix::mount|libc::mount' "$$outer" "$$terminal" "$$evidence" "$$staging"; then exit 1; else status="$$?"; test "$$status" = 1; fi; \
	for file in \
		"$$attempt" \
		"$$promotion" \
		"$$outer" \
		"$$commit_outer" \
		"$$cleanup_outer" \
		"$$complete_outer" \
		"$$terminal" \
		"$$evidence" \
		"$$staging" \
		"$$commit_staging" \
		"$$cleanup_staging" \
		"$$complete_staging" \
		"$$staging_parent" \
		"$$commit_persistence" \
		"$$commit_authority" \
		"$$cleanup_authority" \
		"$$cleanup_persistence" \
		"$$complete_authority" \
		"$$complete_retained_authority" \
		"$$complete_persistence" \
		"$$canonical_reopen" \
		"$(ACTIVE_REBLIT_BOOT_SYNC_COMPLETION_TOP_DIR)"/crates/forge/src/client/boot/active_reblit_boot_publication_preflight/immutable_attempt/receipt_promotion/tests/completion.rs \
		"$(ACTIVE_REBLIT_BOOT_SYNC_COMPLETION_TOP_DIR)"/crates/forge/src/client/boot/active_reblit_boot_publication_preflight/immutable_attempt/receipt_promotion/tests/completion/*.rs \
		"$(ACTIVE_REBLIT_BOOT_SYNC_COMPLETION_TOP_DIR)"/crates/forge/src/client/boot/active_reblit_boot_publication_preflight/immutable_attempt/receipt_promotion/tests/completion/*/*.rs \
		"$(ACTIVE_REBLIT_BOOT_SYNC_COMPLETION_TOP_DIR)/misc/make/active-reblit-boot-sync-completion-tests.mk"; do \
		test "$$( wc -l < "$$file" )" -le 1000; \
	done; \
	$(CARGO) test --manifest-path "$(ACTIVE_REBLIT_BOOT_SYNC_COMPLETION_TOP_DIR)/Cargo.toml" -p forge --lib "$$prefix" -- --test-threads=1 --include-ignored
