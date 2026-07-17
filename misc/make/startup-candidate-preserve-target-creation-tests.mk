.PHONY: forge-startup-usr-rollback-candidate-preserve-target-creation-test

forge-startup-usr-rollback-candidate-preserve-target-creation-test:
	@set -eu; \
	listed="$$( timeout 300s $(CARGO) test -p forge --lib -- --list )"; \
	timeout 10s grep -q . <<<"$$listed"; \
	count="$$( timeout 10s grep -c '^client::startup_reconciliation::usr_rollback_candidate_preserve_authority::tests::target_creation::.*: test$$' <<<"$$listed" )"; \
	timeout 10s test "$$count" = 11; \
	for test in \
		client::startup_reconciliation::usr_rollback_candidate_preserve_authority::tests::target_creation::startup_new_state_target_creation_reconciles_raw_reports_semantically_and_requires_restart \
		client::startup_reconciliation::usr_rollback_candidate_preserve_authority::tests::target_creation::startup_new_state_target_creation_eexist_exact_post_state_requires_restart_without_claiming_apply \
		client::startup_reconciliation::usr_rollback_candidate_preserve_authority::tests::target_creation::startup_new_state_target_creation_accepts_every_restrictive_umask_residue_only_as_restart \
		client::startup_reconciliation::usr_rollback_candidate_preserve_authority::tests::target_creation::startup_new_state_target_creation_keeps_restrictive_residue_payload_opaque_and_requires_restart \
		client::startup_reconciliation::usr_rollback_candidate_preserve_authority::tests::target_creation::startup_new_state_target_creation_ambiguous_evidence_consumes_all_retry_capability \
		client::startup_reconciliation::usr_rollback_candidate_preserve_authority::tests::target_creation::startup_new_state_target_creation_absent_parent_metadata_delta_is_ambiguous \
		client::startup_reconciliation::usr_rollback_candidate_preserve_authority::tests::target_creation::startup_new_state_target_creation_accepts_an_exact_empty_replacement_only_as_restart \
		client::startup_reconciliation::usr_rollback_candidate_preserve_authority::tests::target_creation::startup_new_state_target_creation_records_but_does_not_authorize_arbitrary_xattrs \
		client::startup_reconciliation::usr_rollback_candidate_preserve_authority::tests::target_creation::startup_new_state_target_creation_consumption_starts_with_the_open_journal_binding \
		client::startup_reconciliation::usr_rollback_candidate_preserve_authority::tests::target_creation::startup_new_state_target_creation_final_pre_races_prevent_the_attempt \
		client::startup_reconciliation::usr_rollback_candidate_preserve_authority::tests::target_creation::startup_new_state_target_creation_rechecks_database_and_journal_after_the_attempt; do \
		timeout 10s grep -Fqx "$$test: test" <<<"$$listed"; \
	done; \
	authority=crates/forge/src/client/startup_reconciliation/usr_rollback_candidate_preserve_authority.rs; \
	authority_create=crates/forge/src/client/startup_reconciliation/usr_rollback_candidate_preserve_authority/target_creation.rs; \
	effect_evidence=crates/forge/src/client/startup_reconciliation/usr_rollback_candidate_preserve_authority/effect_evidence.rs; \
	proof=crates/forge/src/client/startup_reconciliation/activation_namespace/candidate_preserve_proof.rs; \
	proof_create=crates/forge/src/client/startup_reconciliation/activation_namespace/candidate_preserve_proof/target_creation.rs; \
	namespace=crates/forge/src/client/startup_reconciliation/activation_namespace/capture/new_state_candidate_target_preparation.rs; \
	namespace_create=crates/forge/src/client/startup_reconciliation/activation_namespace/capture/new_state_candidate_target_preparation/create.rs; \
	namespace_reconciliation=crates/forge/src/client/startup_reconciliation/activation_namespace/capture/new_state_candidate_target_preparation/create/reconciliation.rs; \
	linux_fs=crates/forge/src/linux_fs/namespace_operations.rs; \
	startup_recovery=crates/forge/src/client/startup_recovery.rs; \
	production_dispatch=crates/forge/src/client/startup_recovery/usr_rollback_candidate_preserve_dispatch.rs; \
	tests=crates/forge/src/client/startup_reconciliation/usr_rollback_candidate_preserve_authority/tests/target_creation.rs; \
	timeout 10s grep -Fqx 'mod target_creation;' "$$authority"; \
	timeout 10s grep -Fqx 'mod target_creation;' "$$proof"; \
	timeout 10s grep -Fqx 'mod create;' "$$namespace"; \
	timeout 10s grep -Fqx 'mod reconciliation;' "$$namespace_create"; \
	timeout 10s grep -Fqx '    CreateNewStateTarget(UsrRollbackNewStateCandidatePreserveCreateTargetLease<'\''reservation>),' "$$authority"; \
	timeout 10s grep -Fqx '    RestartRequired,' "$$authority_create"; \
	timeout 10s grep -Fqx '    NotApplied,' "$$authority_create"; \
	timeout 10s grep -Fqx '    Ambiguous,' "$$authority_create"; \
	timeout 10s grep -Fqx '    effect_evidence::{require_effect_binding, require_post_effect_evidence, require_pre_effect_evidence},' "$$authority_create"; \
	timeout 10s awk '$$0 == "pub(in crate::client) enum UsrRollbackNewStateCandidatePreserveCreateTargetReconciliation {" { active = 1; seen = 1; next } active && $$0 == "}" { closed = 1; active = 0; next } active && /[(][^)]|[{][^}]/ { payload = 1 } END { exit !(seen && closed && !payload) }' "$$authority_create"; \
	timeout 10s grep -Fq 'require_effect_binding(&self.effect.journal_binding, journal)?;' "$$authority_create"; \
	timeout 10s grep -Fq 'arm_before_usr_rollback_new_state_target_create_final_pre_capture' "$$proof_create"; \
	timeout 10s grep -Fq 'arm_before_new_state_target_create_attempt' "$$namespace_create"; \
	timeout 10s test "$$( timeout 10s grep -Fc 'mkdirat_once(' "$$namespace_create" )" = 1; \
	timeout 10s grep -Fq 'NewStateTargetCreateFault::ErrorAfterApply' "$$namespace_create"; \
	timeout 10s grep -Fq 'NewStateTargetCreateFault::ErrorWithoutApply' "$$namespace_create"; \
	timeout 10s grep -Fq 'NewStateTargetCreateFault::SuccessWithoutApply' "$$namespace_create"; \
	timeout 10s grep -Fq 'pub(crate) fn mkdirat_once(parent_directory: &std::fs::File, name: &CStr, mode: u32) -> io::Result<()> {' "$$linux_fs"; \
	production_create="$$( timeout 10s sed -E 's,//.*$$,,' "$$effect_evidence" "$$authority_create" "$$proof_create" "$$namespace_create" "$$namespace_reconciliation" )"; \
	if timeout 10s rg -n '^[[:space:]]*(loop|while|for)[[:space:]]|=[[:space:]]*(loop|while|for)[[:space:]]' <<<"$$production_create"; then exit 1; fi; \
	if timeout 10s rg -n 'raw_report\.(is_ok|is_err|unwrap|expect)|match[[:space:]]+raw_report|if[[:space:]]+let.*raw_report' "$$namespace_create" "$$namespace_reconciliation"; then exit 1; fi; \
	if timeout 10s rg -n 'MoveNewState|reconcile_move|attempt_move|renameat|rename\(' "$$effect_evidence" "$$authority_create" "$$proof_create" "$$namespace_create" "$$namespace_reconciliation"; then exit 1; fi; \
	if timeout 10s rg -n 'sync_all|sync_data|complete_.*durability|RollbackActionOutcome|rollback_successor|forward_successor|\.advance[[:space:]]*\(|clear_transition_if_matches|remove_transition_if_matches|run_transaction_triggers|run_system_triggers|insert_fresh_metadata|delete_metadata|\.execute\(|\.transaction\(|\.delete\(' "$$effect_evidence" "$$authority_create" "$$proof_create" "$$namespace_create" "$$namespace_reconciliation"; then exit 1; fi; \
	timeout 10s test "$$( timeout 10s rg -l '^pub\(in crate::client\) struct UsrRollbackCandidatePreserveEffectSeal \{' crates/forge/src/client --glob '*.rs' )" = "$$production_dispatch"; \
	timeout 10s grep -Fq '    UsrRollbackCandidatePreserveEffectSeal, UsrRollbackCandidatePreserveReady,' "$$startup_recovery"; \
	timeout 10s test "$$( timeout 10s grep -Fc 'UsrRollbackCandidatePreserveEffectSeal::new();' "$$production_dispatch" )" = 1; \
	timeout 10s test "$$( timeout 10s grep -Fc 'UsrRollbackCandidatePreserveApplyEffectSelection::CreateNewStateTarget(lease) =>' "$$production_dispatch" )" = 1; \
	timeout 10s test "$$( timeout 10s grep -Fc 'UsrRollbackNewStateCandidatePreserveCreateTargetReconciliation::RestartRequired =>' "$$production_dispatch" )" = 1; \
	timeout 10s grep -Fq 'NewStateTargetCreateFault::ErrorAfterApply' "$$tests"; \
	timeout 10s grep -Fq 'NewStateTargetCreateFault::ErrorWithoutApply' "$$tests"; \
	timeout 10s grep -Fq 'NewStateTargetCreateFault::SuccessWithoutApply' "$$tests"; \
	timeout 10s grep -Fq 'arm_before_new_state_target_create_attempt' "$$tests"; \
	timeout 10s grep -Fq 'system.posix_acl_access' "$$tests"; \
	timeout 10s grep -Fq 'system.posix_acl_default' "$$tests"; \
	timeout 10s grep -Fq 'nix::libc::mkfifo' "$$tests"; \
	timeout 10s grep -Fq 'symlink(external, target)' "$$tests"; \
	timeout 10s test "$$( timeout 10s grep -Fc 'new_state_target_create_attempt_count()' "$$tests" )" -ge 10; \
	timeout 10s test "$$( timeout 10s grep -Fc 'new_state_candidate_preserve_move_attempt_count()' "$$tests" )" -ge 1; \
	for file in "$$authority" "$$authority_create" "$$effect_evidence" "$$proof" "$$proof_create" "$$namespace" "$$namespace_create" "$$namespace_reconciliation" "$$tests" crates/forge/src/client/startup_reconciliation/usr_rollback_candidate_preserve_authority/tests.rs misc/make/startup-candidate-preserve-target-creation-tests.mk Makefile; do \
		timeout 10s test "$$( timeout 10s wc -l < "$$file" )" -le 1000; \
	done; \
	timeout 1200s $(CARGO) test -p forge --lib \
		'client::startup_reconciliation::usr_rollback_candidate_preserve_authority::tests::target_creation::' \
		-- --test-threads=1
