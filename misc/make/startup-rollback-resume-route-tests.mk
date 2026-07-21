.PHONY: forge-startup-usr-rollback-resume-route-test

forge-startup-usr-rollback-resume-route-test:
	@set -eu; \
	listed="$$( $(CARGO) test -p forge --lib -- --list )"; \
	grep -q . <<<"$$listed"; \
	count="$$( grep -c '^client::startup_recovery::usr_rollback_resume_route::tests::.*: test$$' <<<"$$listed" )"; \
	test "$$count" = 22; \
	for test in \
		client::startup_recovery::usr_rollback_resume_route::tests::matrix::startup_usr_rollback_resume_route_pending_matrix_persists_reverse_exchange_intent \
		client::startup_recovery::usr_rollback_resume_route::tests::matrix::startup_usr_rollback_resume_route_satisfied_matrix_skips_reverse_exchange \
		client::startup_recovery::usr_rollback_resume_route::tests::matrix::startup_usr_rollback_resume_route_usr_restored_matrix_persists_candidate_preserve_intent \
		client::startup_recovery::usr_rollback_resume_route::tests::matrix::startup_usr_rollback_resume_route_routes_only_and_preserves_exact_plan \
		client::startup_recovery::usr_rollback_resume_route::tests::evidence_races::startup_usr_rollback_resume_route_rejects_a_different_open_journal_binding \
		client::startup_recovery::usr_rollback_resume_route::tests::evidence_races::startup_usr_rollback_resume_route_database_and_provenance_conflicts_never_advance \
		client::startup_recovery::usr_rollback_resume_route::tests::evidence_races::startup_usr_rollback_resume_route_namespace_conflicts_never_advance \
		client::startup_recovery::usr_rollback_resume_route::tests::evidence_races::startup_usr_rollback_resume_route_capture_and_final_revalidation_races_fail_before_advance \
		client::startup_recovery::usr_rollback_resume_route::tests::evidence_races::startup_root_links_complete_usr_restored_root_abi_races_reject_both_route_seams_without_advance \
		client::startup_recovery::usr_rollback_resume_route::tests::evidence_races::startup_usr_rollback_resume_route_historical_and_active_reblit_evidence_remain_exact \
		client::startup_recovery::usr_rollback_resume_route::tests::storage_reopen::startup_usr_rollback_resume_route_storage_faults_reopen_to_exact_source_or_successor \
		client::startup_recovery::usr_rollback_resume_route::tests::storage_reopen::startup_usr_rollback_resume_route_rejects_cross_root_authority_and_reopens_success \
		client::startup_recovery::usr_rollback_resume_route::tests::end_to_end::startup_usr_rollback_resume_route_decision_route_and_reverse_use_one_persistence_boundary_per_entry \
		client::startup_recovery::usr_rollback_resume_route::tests::root_links_route_matrix::root_links_complete_pre_layout_cannot_construct_a_route_fixture \
		client::startup_recovery::usr_rollback_resume_route::tests::root_links_route_matrix::startup_root_links_complete_post_routes_exactly_across_operations_and_epochs \
		client::startup_recovery::usr_rollback_resume_route::tests::root_links_route_matrix::startup_root_links_complete_pre_layout_defers_pending_reverse_plan_across_operations_and_epochs \
		client::startup_recovery::usr_rollback_resume_route::tests::root_links_route_matrix::startup_root_links_complete_post_layout_defers_codec_valid_wrong_plans_across_operations_and_epochs \
		client::startup_recovery::usr_rollback_resume_route::tests::root_links_record_binding::startup_root_links_complete_route_same_byte_predecessor_replacement_breaks_exact_binding \
		client::startup_recovery::usr_rollback_resume_route::tests::root_links_record_binding::startup_root_links_complete_route_same_byte_successor_replacement_reopens_but_never_succeeds \
		client::startup_recovery::usr_rollback_resume_route::tests::root_links_record_binding::startup_root_links_complete_route_same_byte_successor_replacement_after_binding_before_reopen_never_succeeds \
		client::startup_recovery::usr_rollback_resume_route::tests::root_links_storage_faults::startup_root_links_complete_route_all_storage_faults_reopen_exact_record_across_operations_and_epochs \
		client::startup_recovery::usr_rollback_resume_route::tests::root_links_route_endpoint::startup_root_links_complete_fresh_entries_reach_operation_specific_stable_endpoints_without_second_reverse_exchange \
		transition_identity::journal_coordinator::tests::journal_coordinator_usr_exchange_effect_durability_faults_recover_through_exact_usr_restored; do \
		grep -Fqx "$$test: test" <<<"$$listed"; \
	done; \
	executor=crates/forge/src/client/startup_recovery/usr_rollback_resume_route.rs; \
	reopen=crates/forge/src/client/startup_recovery/canonical_journal_reopen.rs; \
	authority=crates/forge/src/client/startup_reconciliation/usr_rollback_resume_route_authority.rs; \
	reverse_authority=crates/forge/src/client/startup_reconciliation/usr_rollback_reverse_authority.rs; \
	reverse_proof=crates/forge/src/client/startup_reconciliation/activation_namespace/rollback_reverse_proof.rs; \
	proof=crates/forge/src/client/startup_reconciliation/activation_namespace/resume_route_proof.rs; \
	startup_gate=crates/forge/src/client/startup_gate.rs; \
	journal_store=crates/forge/src/transition_journal/store.rs; \
	record_binding=crates/forge/src/transition_journal/store/record_binding.rs; \
	coordinator_test=crates/forge/src/transition_identity/journal_coordinator/tests/usr_exchange_effect.rs; \
	forward_support=crates/forge/src/client/startup_recovery/forward_origin_test_support.rs; \
	end_to_end=crates/forge/src/client/startup_recovery/usr_rollback_resume_route/tests/end_to_end.rs; \
	evidence_races=crates/forge/src/client/startup_recovery/usr_rollback_resume_route/tests/evidence_races.rs; \
	root_links_endpoint=crates/forge/src/client/startup_recovery/usr_rollback_resume_route/tests/root_links_route_endpoint.rs; \
	root_links_record_binding=crates/forge/src/client/startup_recovery/usr_rollback_resume_route/tests/root_links_record_binding.rs; \
	root_links_storage=crates/forge/src/client/startup_recovery/usr_rollback_resume_route/tests/root_links_storage_faults.rs; \
	route_matrix=crates/forge/src/client/startup_recovery/usr_rollback_resume_route/tests/matrix.rs; \
	route_support=crates/forge/src/client/startup_recovery/usr_rollback_resume_route/tests/support.rs; \
	grep -Fqx '            if kind == OperationKind::ActiveReblit && expected.phase == Phase::CandidatePreserveIntent {' "$$end_to_end"; \
	grep -Fq 'This resume-route end-to-end test stops before crossing into' "$$end_to_end"; \
	successor_count="$$( rg -n '\.rollback_successor\(' "$$executor" "$$authority" "$$proof" | wc -l )"; \
	test "$$successor_count" = 1; \
	grep -Fqx '    let successor = match source_record.rollback_successor(None) {' "$$executor"; \
	if rg -n 'journal\.advance\(' "$$executor" "$$authority" "$$proof" "$$reopen"; then exit 1; else status="$$?"; test "$$status" = 1; fi; \
	if rg -n 'journal\.load\(\)' "$$reverse_proof"; then exit 1; else status="$$?"; test "$$status" = 1; fi; \
	test "$$( grep -Fc 'authority.advance_record_binding(&journal, &successor)' "$$executor" )" = 1; \
	test "$$( grep -Fc '.advance_record_binding(cast, self.journal_record_binding, next)' "$$authority" )" = 1; \
	grep -Fqx 'mod canonical_journal_reopen;' crates/forge/src/client/startup_recovery.rs; \
	test "$$( rg -n 'canonical_journal_reopen' crates/forge/src/client/startup_recovery.rs | wc -l )" = 1; \
	test "$$( rg -n '^pub\(super\) fn reopen_canonical_journal\(' "$$reopen" | wc -l )" = 1; \
	grep -Fqx 'pub(super) enum CanonicalJournalReopenError {' "$$reopen"; \
	rg -U -q '^pub\(super\) fn reopen_canonical_journal\(\n    installation: &Installation,\n\) -> Result<\(TransitionJournalStore, Option<TransitionRecord>\), CanonicalJournalReopenError> \{' "$$reopen"; \
	test "$$( grep -Fc 'reopen_canonical_journal(&installation)' "$$executor" )" = 1; \
	grep -Fqx '    let reopened = reopen_canonical_journal(&installation).map_err(UsrRollbackResumeRouteReopenError::from);' "$$executor"; \
	clone_line="$$( grep -nF '    let installation = authority.installation().clone();' "$$executor" | cut -d: -f1 )"; \
	advance_line="$$( grep -nF '    let advance = match authority.advance_record_binding(&journal, &successor) {' "$$executor" | cut -d: -f1 )"; \
	drop_journal_line="$$( grep -nF '    drop(journal);' "$$executor" | tail -n 1 | cut -d: -f1 )"; \
	reopen_line="$$( grep -nF '    let reopened = reopen_canonical_journal(&installation).map_err(UsrRollbackResumeRouteReopenError::from);' "$$executor" | cut -d: -f1 )"; \
	test "$$clone_line" -lt "$$advance_line"; \
	test "$$advance_line" -lt "$$drop_journal_line"; \
	test "$$drop_journal_line" -lt "$$reopen_line"; \
	seam_line="$$( grep -nF '        after_usr_rollback_resume_route_successor_binding_check_before_reopen();' "$$executor" | cut -d: -f1 )"; \
	test "$$drop_journal_line" -lt "$$seam_line"; \
	test "$$seam_line" -lt "$$reopen_line"; \
	suffix="$$( sed -n '/    let advance = match authority.advance_record_binding/,/    let reopened = reopen_canonical_journal/p' "$$executor" )"; \
	if grep -Fq 'drop(authority)' <<<"$$suffix"; then exit 1; else status="$$?"; test "$$status" = 1; fi; \
	test "$$( grep -Fc '            drop(journal);' <<<"$$suffix" )" = 2; \
	test "$$( grep -Fc '    drop(journal);' <<<"$$suffix" )" = 3; \
	rg -U -q 'Err\(UsrRollbackResumeRouteRecordAdvanceError::Authority\(source\)\) => \{\n            drop\(journal\);\n            return Err\(UsrRollbackResumeRoutePersistenceError::Authority\(source\)\);\n        \}' <<<"$$suffix"; \
	rg -U -q 'Err\(UsrRollbackResumeRouteRecordAdvanceError::Installation\(source\)\) => \{\n            drop\(journal\);\n            return Err\(UsrRollbackResumeRoutePersistenceError::Installation\(source\)\);\n        \}' <<<"$$suffix"; \
	test "$$( grep -Fc 'reopen_canonical_journal(&installation)' <<<"$$suffix" )" = 1; \
	awk ' \
		function fail() { bad = 1; exit } \
		$$0 == "        Ok(successor_binding) => {" { if (active || seen) fail(); active = 1; seen = 1; next } \
		active && $$0 == "        Err(UsrRollbackResumeRouteRecordAdvanceError::Authority(source)) => {" { active = 0; closed = 1; next } \
		active { \
			if ($$0 ~ /(^|[^[:alnum:]_])return([^[:alnum:]_]|$$)/ || index($$0, "?") || $$0 ~ /[[:alpha:]_][[:alnum:]_]*![[:space:]]*[({[]/) fail(); \
			if (index($$0, "UsrRollbackResumeRouteAdvanceOutcome::SuccessorBindingFailed(")) failures += 1; \
		} \
		END { if (bad || seen != 1 || closed != 1 || active || failures < 1) exit 1 } \
	' "$$executor"; \
	awk ' \
		function fail() { bad = 1; exit } \
		function finish_branch() { \
			if (branch == 0) return; \
			if (errors != 1 || error_open || !error_closed || !branch_closed) fail(); \
			if (branch == 1 && (record_binding != 1 || durable_source != 1 || durable_successor != 0 || combined != 0)) fail(); \
			if (branch == 2 && (record_binding != 1 || durable_source != 0 || durable_successor != 1 || combined != 0)) fail(); \
			if ((branch == 3 || branch == 4) && (record_binding != 0 || durable_source != 0 || durable_successor != 0 || combined != 1)) fail(); \
		} \
		function reset_branch() { \
			errors = record_binding = durable_source = durable_successor = combined = 0; \
			error_open = error_closed = branch_closed = 0; \
		} \
		$$0 == "        UsrRollbackResumeRouteAdvanceOutcome::SuccessorBindingFailed(binding) => match reopened {" { \
			if (active || seen != 0) fail(); active = 1; seen = 1; next; \
		} \
		active && $$0 == "        }," { finish_branch(); active = 0; closed = 1; next; } \
		active && $$0 ~ /^            .* => / { \
			finish_branch(); branch += 1; reset_branch(); \
			if (branch == 1 && $$0 != "            Ok((reopened, Some(actual))) if actual == source_record => {") fail(); \
			if (branch == 2 && $$0 != "            Ok((reopened, Some(actual))) if actual == successor => {") fail(); \
			if (branch == 3 && $$0 != "            Ok((reopened, actual)) => {") fail(); \
			if (branch == 4) { \
				if ($$0 != "            Err(reopen) => Err(UsrRollbackResumeRoutePersistenceError::SuccessorRecordBindingAndReopen {") fail(); \
				errors = 1; combined = 1; error_open = 1; \
			} \
			if (branch > 4) fail(); next; \
		} \
		active { \
			if ($$0 ~ /(^|[^[:alnum:]_])return([^[:alnum:]_]|$$)/ || index($$0, "?") != 0 || $$0 ~ /[[:alpha:]_][[:alnum:]_]*![[:space:]]*[({[]/) fail(); \
			if ($$0 ~ /^[[:space:]]*Ok\(/) fail(); \
			if (branch_closed && $$0 !~ /^[[:space:]]*(\/\/.*)?$$/) fail(); \
			if ($$0 == "                Err(UsrRollbackResumeRoutePersistenceError::SuccessorRecordBinding {") { \
				if (branch > 2 || error_open || error_closed) fail(); errors += 1; record_binding += 1; error_open = 1; next; \
			} \
			if ($$0 == "                Err(UsrRollbackResumeRoutePersistenceError::SuccessorRecordBindingAndReopen {") { \
				if (branch != 3 || error_open || error_closed) fail(); errors += 1; combined += 1; error_open = 1; next; \
			} \
			if ($$0 == "                    durable: DurableUsrRollbackResumeRouteRecord::Source,") durable_source += 1; \
			if ($$0 == "                    durable: DurableUsrRollbackResumeRouteRecord::Successor,") durable_successor += 1; \
			if (branch <= 3 && error_open && $$0 == "                })") { error_open = 0; error_closed = 1; next; } \
			if (branch <= 3 && $$0 == "            }") { if (!error_closed || error_open || branch_closed) fail(); branch_closed = 1; next; } \
			if (branch == 4 && error_open && $$0 == "            }),") { error_open = 0; error_closed = 1; branch_closed = 1; next; } \
			if (error_closed && !branch_closed && $$0 !~ /^[[:space:]]*(\/\/.*)?$$/) fail(); \
		} \
		END { if (bad || seen != 1 || closed != 1 || active || branch != 4) exit 1 } \
	' "$$executor"; \
	if rg -n 'open_in_retained_cast|journal\.load\(' "$$executor"; then exit 1; fi; \
	rg -U -q 'installation\.revalidate_mutable_namespace\(\)\?;\n    let cast = installation\.retained_mutable_cast_directory\(\)\?;\n    let journal = TransitionJournalStore::open_in_retained_cast\(cast, &installation\.root\)\?;\n    installation\.revalidate_mutable_namespace\(\)\?;\n    let record = journal\.load_revalidated_retained_cast\(cast\)\?;\n    installation\.revalidate_mutable_namespace\(\)\?;' "$$reopen"; \
	test "$$( grep -Fc '    installation.revalidate_mutable_namespace()?;' "$$reopen" )" = 3; \
	test "$$( grep -Fc 'installation.retained_mutable_cast_directory()?' "$$reopen" )" = 1; \
	test "$$( grep -Fc 'TransitionJournalStore::open_in_retained_cast(' "$$reopen" )" = 1; \
	test "$$( grep -Fc 'journal.load_revalidated_retained_cast(cast)?' "$$reopen" )" = 1; \
	grep -Fqx '            CanonicalJournalReopenError::Installation(source) => Self::Installation(source),' "$$executor"; \
	grep -Fqx '            CanonicalJournalReopenError::Journal(source) => Self::Journal(source),' "$$executor"; \
	if rg -n 'Phase|rollback_decision|rollback_successor|forward_successor|TransitionJournalStore::(open|open_retained|try_open_in_retained_cast)\(|std::fs|fs::|File::open|OpenOptions|openat|AsRawFd|IntoRawFd|FromRawFd|AsFd|RawFd|BorrowedFd|OwnedFd|unsafe[[:space:]]*\{' "$$reopen"; then exit 1; fi; \
	if rg -n 'forward_successor|RollbackActionOutcome|transition_identity|linux_fs|std::fs|nix::|renameat|unlinkat|linkat|sync_all|sync_data|write_all|set_permissions|create_dir|remove_dir|remove_file|hard_link|symlink|run_transaction_triggers|run_system_triggers|root_links|archive_previous|rearchive_archived|preserve_failed|exchange_forward|exchange_reverse|remove_exact_archived|add_with_transition|insert_fresh_metadata|delete_metadata_provenance|clear_transition_if_matches|remove_transition_if_matches|\.add\(|\.remove\(|\.batch_remove\(|\.execute\(|\.transaction\(|\.delete\(' "$$executor" "$$authority" "$$proof" "$$reopen"; then exit 1; fi; \
	if rg -n 'PendingSystemTransition|ActivationNamespaceEvidence' "$$executor" "$$authority" "$$proof"; then exit 1; fi; \
	awk '$$0 == "pub(in crate::client) fn persist_usr_rollback_resume_route_and_reopen(" { state = 1; next } state == 1 && $$0 == "    journal: TransitionJournalStore," { state = 2; next } state == 2 && $$0 ~ /authority: UsrRollbackResumeRouteAuthority/ { found = 1 } END { exit !found }' "$$executor"; \
	persist_signature="$$( sed -n '/^pub(in crate::client) fn persist_usr_rollback_resume_route_and_reopen(/,/^)/p' "$$executor" )"; \
	if rg -n 'journal: &[[:space:]]*TransitionJournalStore' <<<"$$persist_signature"; then exit 1; fi; \
	grep -Fq 'if actual == source_record' "$$executor"; \
	grep -Fq 'if actual == successor' "$$executor"; \
	seal_count="$$( rg -n '^pub\(in crate::client\) struct UsrRollbackResumeRouteSeal \{' "$$startup_gate" | wc -l )"; \
	test "$$seal_count" = 1; \
	awk '$$0 == "pub(in crate::client) struct UsrRollbackResumeRouteSeal {" { state = 1; next } state == 1 && $$0 == "    _private: ()," { state = 2; next } state == 2 && $$0 == "}" { found = 1 } END { exit !found }' "$$startup_gate"; \
	seal_call_count="$$( rg -n 'UsrRollbackResumeRouteSeal::new\(' crates/forge/src/client --glob '*.rs' --glob '!**/tests/**' --glob '!**/*_tests.rs' --glob '!**/*_tests/**' | wc -l )"; \
	test "$$seal_call_count" = 1; \
	capture_call_count="$$( rg -n 'UsrRollbackResumeRouteAuthority::capture\(' crates/forge/src/client --glob '*.rs' --glob '!**/tests/**' --glob '!**/*_tests.rs' --glob '!**/*_tests/**' | wc -l )"; \
	test "$$capture_call_count" = 1; \
	grep -Fqx '        _startup_gate_seal: &UsrRollbackResumeRouteSeal,' "$$authority"; \
	if rg -n 'TransitionJournalBinding|journal\.binding\(\)|journal\.has_binding\(' "$$executor" "$$authority"; then exit 1; else status="$$?"; test "$$status" = 1; fi; \
	grep -Fqx '    journal_record_binding: TransitionJournalRecordBinding,' "$$authority"; \
	test "$$( grep -Fc 'journal.record_binding(installation.retained_mutable_cast_directory()?, record)?' "$$authority" )" = 1; \
	test "$$( grep -Fc 'require_journal_record_binding(' "$$authority" )" = 4; \
	test "$$( grep -Fc 'journal.has_record_binding(cast, binding, record)?' "$$authority" )" = 1; \
	test "$$( grep -Fc '.has_record_binding(cast, &successor_binding, &successor)' "$$executor" )" = 1; \
	grep -Fqx '    Published(TransitionJournalRecordBinding),' "$$executor"; \
	grep -Fqx '        UsrRollbackResumeRouteAdvanceOutcome::Published(successor_binding) => match reopened {' "$$executor"; \
	test "$$( grep -Fc '.has_reopened_record_binding(cast, successor_binding, successor)' "$$executor" )" = 1; \
	reopened_binding_helper="$$( sed -n '/^fn revalidate_reopened_route_binding(/,/^}/p' "$$executor" )"; \
	test "$$( grep -Fc '.revalidate_mutable_namespace()' <<<"$$reopened_binding_helper" )" = 2; \
	grep -Fq '.has_reopened_record_binding(cast, successor_binding, successor)' <<<"$$reopened_binding_helper"; \
	grep -Fq 'arm_after_usr_rollback_resume_route_successor_binding_check_before_reopen' "$$executor"; \
	binding_helper="$$( sed -n '/^fn require_journal_record_binding(/,/^}/p' "$$authority" )"; \
	grep -Fq '    if !journal.has_record_store_binding(binding) {' <<<"$$binding_helper"; \
	store_binding_line="$$( grep -nF '    if !journal.has_record_store_binding(binding) {' <<<"$$binding_helper" | cut -d: -f1 )"; \
	cast_binding_line="$$( grep -nF '    let cast = installation.retained_mutable_cast_directory()?;' <<<"$$binding_helper" | cut -d: -f1 )"; \
	test "$$store_binding_line" -lt "$$cast_binding_line"; \
	capture_line="$$( grep -nF 'journal.record_binding(installation.retained_mutable_cast_directory()?, record)?' "$$authority" | cut -d: -f1 )"; \
	namespace_line="$$( grep -nF 'let namespace_inspection = match UsrRollbackResumeRouteNamespaceInspection::begin' "$$authority" | cut -d: -f1 )"; \
	test "$$capture_line" -lt "$$namespace_line"; \
	grep -Fqx 'pub(crate) struct TransitionJournalBinding(Arc<()>);' "$$journal_store"; \
	grep -Fqx 'pub(crate) struct TransitionJournalRecordBinding {' "$$record_binding"; \
	grep -Fq 'pub(crate) fn has_record_store_binding(' "$$record_binding"; \
	grep -Fq 'pub(crate) fn advance_record_binding(' "$$record_binding"; \
	grep -Fq 'if !matches!(record.phase, Phase::RollbackDecided | Phase::UsrRestored)' "$$authority"; \
	grep -Fq 'ForwardPhase::UsrExchangeIntent | ForwardPhase::UsrExchanged' "$$authority"; \
	test "$$( grep -Fc 'ForwardPhase::UsrExchangeIntent | ForwardPhase::UsrExchanged | ForwardPhase::RootLinksComplete' "$$authority" )" = 1; \
	grep -Fq 'ForwardPhase::UsrExchangeIntent | ForwardPhase::UsrExchanged | ForwardPhase::RootLinksComplete' "$$reverse_authority"; \
	grep -Fq 'let restored = reverse_intent' "$$root_links_endpoint"; \
	grep -Fq 'let candidate_intent = restored.rollback_successor(None).unwrap();' "$$root_links_endpoint"; \
	grep -Fq 'let candidate_preserved = candidate_intent' "$$root_links_endpoint"; \
	grep -Fq 'assert_eq!(root_links_before.len(), 5, "{case}");' "$$root_links_endpoint"; \
	grep -Fq '                OperationKind::NewState => {' "$$root_links_endpoint"; \
	grep -Fq '                    assert_eq!(candidate_preserved.generation, 15, "{case}");' "$$root_links_endpoint"; \
	grep -Fq '                    let invalidation_intent = candidate_preserved.rollback_successor(None).unwrap();' "$$root_links_endpoint"; \
	grep -Fq '                    assert_eq!(invalidation_intent.generation, 16, "{case}");' "$$root_links_endpoint"; \
	grep -Fq '                    assert_eq!(invalidated.generation, 17, "{case}");' "$$root_links_endpoint"; \
	grep -Fq '                    assert_eq!(rollback_complete.generation, 18, "{case}");' "$$root_links_endpoint"; \
	grep -Fq '.expect("exact generation-18 RootLinks NewState terminal must finalize cleanly");' "$$root_links_endpoint"; \
	grep -Fq '.expect("finalized RootLinks NewState endpoint must remain clean");' "$$root_links_endpoint"; \
	test "$$( grep -Fc 'fresh_db_invalidation_removal_call_count(), 1, "{case}"' "$$root_links_endpoint" )" = 4; \
	grep -Fq '                OperationKind::Archived => {' "$$root_links_endpoint"; \
	grep -Fq '                    assert_eq!(candidate_preserved.generation, 11, "{case}");' "$$root_links_endpoint"; \
	grep -Fq '                    let rollback_complete = candidate_preserved.rollback_successor(None).unwrap();' "$$root_links_endpoint"; \
	grep -Fq '                    assert_eq!(rollback_complete.generation, 12, "{case}");' "$$root_links_endpoint"; \
	grep -Fq '.expect("exact generation-12 RootLinks ActivateArchived terminal must finalize cleanly");' "$$root_links_endpoint"; \
	grep -Fq '.expect("finalized RootLinks ActivateArchived endpoint must remain clean");' "$$root_links_endpoint"; \
	grep -Fq '.join(".cast/journal/state-transition")' "$$root_links_endpoint"; \
	test "$$( grep -Fc 'assert_eq!(archived_candidate_preserve_move_attempt_count(), 1, "{case}");' "$$root_links_endpoint" )" = 2; \
	test "$$( grep -Fc 'assert_eq!(active_reblit_candidate_preserve_exchange_attempt_count(), 0, "{case}");' "$$root_links_endpoint" )" = 4; \
	test "$$( grep -Fc 'fresh_db_invalidations_before_terminal,' "$$root_links_endpoint" )" = 4; \
	test "$$( grep -Fc 'assert_eq!(boot_synchronize_attempt_count(), 0, "{case}");' "$$root_links_endpoint" )" = 6; \
	grep -Fq '                OperationKind::ActiveReblit => {' "$$root_links_endpoint"; \
	grep -Fq '                    assert_eq!(candidate_preserved.generation, 13, "{case}");' "$$root_links_endpoint"; \
	grep -Fq '                    let rollback_complete = candidate_preserved.rollback_successor(None).unwrap();' "$$root_links_endpoint"; \
	grep -Fq '                    assert_eq!(rollback_complete.generation, 14, "{case}");' "$$root_links_endpoint"; \
	grep -Fq '.expect("exact generation-14 RootLinks ActiveReblit terminal must finalize cleanly");' "$$root_links_endpoint"; \
	grep -Fq '.expect("finalized RootLinks ActiveReblit endpoint must remain clean");' "$$root_links_endpoint"; \
	if rg -n 'pending\(&stable_entry\).*Phase::RollbackComplete|canonical_bytes\(\), complete_bytes' "$$root_links_endpoint"; then exit 1; else status="$$?"; test "$$status" = 1; fi; \
	test "$$( grep -Fc 'assert_eq!(active_reblit_candidate_preserve_exchange_attempt_count(), 1, "{case}");' "$$root_links_endpoint" )" = 3; \
	test "$$( grep -Fc 'CleanSystemStartup::enter(&fixture.system' "$$root_links_endpoint" )" = 6; \
	test "$$( grep -Fc 'assert_eq!(archived_candidate_preserve_move_attempt_count(), 0, "{case}");' "$$root_links_endpoint" )" = 4; \
	test "$$( grep -Fc 'Phase::FreshDbInvalidationIntent' "$$root_links_endpoint" )" = 2; \
	test "$$( grep -Fc 'Phase::FreshDbInvalidated' "$$root_links_endpoint" )" = 2; \
	test "$$( grep -Fc 'Phase::RollbackComplete' "$$root_links_endpoint" )" = 6; \
	test "$$( grep -Fc 'assert_eq!(retained_exchange_syscall_count(), 1, "{case}");' "$$root_links_endpoint" )" = 15; \
	test "$$( grep -Fc 'assert_eq!(root_link_snapshot(&fixture), root_links_before, "{case}");' "$$root_links_endpoint" )" = 15; \
	grep -Fq 'for seam in RootAbiRouteSeam::ALL {' "$$evidence_races"; \
	grep -Fq 'for historical in [false, true] {' "$$evidence_races"; \
	grep -Fq 'for kind in OperationKind::ALL {' "$$evidence_races"; \
	grep -Fq 'for outcome in [RollbackActionOutcome::Applied, RollbackActionOutcome::AlreadySatisfied] {' "$$evidence_races"; \
	grep -Fq 'for link_index in 0..ROOT_ABI.len() {' "$$evidence_races"; \
	grep -Fq 'for race in RootAbiRace::ALL {' "$$evidence_races"; \
	grep -Fq 'SourceCase::RootLinksCompletePost,' "$$evidence_races"; \
	grep -Fq 'arm_before_usr_rollback_resume_route_fresh_namespace_capture(hook);' "$$evidence_races"; \
	grep -Fq 'arm_before_usr_rollback_resume_route_final_revalidation(hook);' "$$evidence_races"; \
	grep -Fq 'assert_eq!(fresh_namespace_capture_cases, 180);' "$$evidence_races"; \
	grep -Fq 'assert_eq!(final_revalidation_cases, 180);' "$$evidence_races"; \
	grep -Fq 'assert_eq!(fresh_namespace_capture_cases + final_revalidation_cases, 360);' "$$evidence_races"; \
	grep -Fq 'SourceCase::RootLinksCompletePost,' "$$route_matrix"; \
	grep -Fq 'pub(super) fn root_links_routes_at_epoch(kind: OperationKind, historical: bool) -> [Self; 3] {' "$$route_support"; \
	test "$$( rg -n -F 'RouteFixture::root_links_routes_at_epoch(kind, historical)' "$$root_links_record_binding" "$$root_links_storage" | wc -l )" = 4; \
	grep -Fq '(RollbackAction::Pending, UsrExchangeLayout::Post)' "$$authority"; \
	grep -Fq '(RollbackAction::AlreadySatisfied, UsrExchangeLayout::Pre)' "$$authority"; \
	grep -Fq 'RollbackAction::Applied | RollbackAction::AlreadySatisfied' "$$authority"; \
	grep -Fq 'record.phase == Phase::UsrRestored' "$$proof"; \
	grep -Fq 'wrapper.role == TreeLocation::TransitionQuarantine' "$$proof"; \
	grep -Fq 'super::startup_recovery::persist_usr_rollback_resume_route_and_reopen(journal, authority)?' "$$startup_gate"; \
	grep -Fq 'assert_usr_rollback_decision_routes_to_reverse_exchange_intent(' "$$coordinator_test"; \
	grep -Fq 'decision.rollback_successor(None).unwrap()' "$$coordinator_test"; \
	grep -Fq 'retained_exchange_syscall_count() == 1' "$$coordinator_test"; \
	grep -Fq 'assert_eq!(pending.phase(), Phase::ReverseExchangeIntent);' "$$forward_support"; \
	for file in "$$executor" "$$authority" "$$proof" "$$reopen" "$$evidence_races" "$$root_links_endpoint" "$$root_links_record_binding" "$$root_links_storage" "$$route_matrix" "$$route_support" misc/make/startup-rollback-resume-route-tests.mk; do \
		test "$$( wc -l < "$$file" )" -le 1000; \
	done; \
	$(CARGO) test -p forge --lib \
		'client::startup_recovery::usr_rollback_resume_route::tests::' \
		-- --test-threads=1; \
	$(CARGO) test -p forge --lib \
		'transition_identity::journal_coordinator::tests::journal_coordinator_usr_exchange_effect_durability_faults_recover_through_exact_usr_restored' \
		-- --exact --test-threads=1
