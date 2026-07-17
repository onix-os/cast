.PHONY: forge-startup-usr-rollback-fresh-db-invalidation-effect-test

forge-startup-usr-rollback-fresh-db-invalidation-effect-test:
	@set -euo pipefail; \
	timeout 10s mkdir -p "$(TOP_DIR)/target"; \
	listed="$$( timeout 10s mktemp "$(TOP_DIR)/target/fresh-db-invalidation-effect-list.XXXXXXXXXXXX" )"; \
	bare_calls="$$( timeout 10s mktemp "$(TOP_DIR)/target/fresh-db-invalidation-effect-calls.XXXXXXXXXXXX" )"; \
	production_code="$$( timeout 10s mktemp "$(TOP_DIR)/target/fresh-db-invalidation-effect-code.XXXXXXXXXXXX" )"; \
	apply_body="$$( timeout 10s mktemp "$(TOP_DIR)/target/fresh-db-invalidation-apply.XXXXXXXXXXXX" )"; \
	finish_body="$$( timeout 10s mktemp "$(TOP_DIR)/target/fresh-db-invalidation-finish.XXXXXXXXXXXX" )"; \
	trap 'timeout 10s rm -f "$$listed" "$$bare_calls" "$$production_code" "$$apply_body" "$$finish_body"' EXIT; \
	timeout 300s $(CARGO) test -p forge --lib -- --list | timeout 30s tee "$$listed" >/dev/null; \
	timeout 10s grep -q . "$$listed"; \
	prefix='client::startup_reconciliation::usr_rollback_fresh_db_invalidation_authority::tests::'; \
	count="$$( timeout 10s awk -v prefix="$$prefix" 'index($$0, prefix) == 1 && $$0 ~ /: test$$/ { count += 1 } END { print count + 0 }' "$$listed" )"; \
	timeout 10s test "$$count" = 12; \
	for name in \
		admission::startup_fresh_db_invalidation_admits_exact_present_apply_and_bound_joint_absence_finish_matrix \
		admission::startup_fresh_db_invalidation_plan_accepts_only_the_exact_new_state_pending_fresh_action \
		admission::startup_fresh_db_invalidation_refuses_wrong_phase_operation_and_missing_rollback_before_effect \
		effect::startup_fresh_db_invalidation_apply_and_finish_fix_applied_and_already_satisfied_origins \
		effect::startup_fresh_db_invalidation_known_committed_error_reconciles_as_applied_once \
		effect::startup_fresh_db_invalidation_proven_nonapplication_maps_to_fieldless_not_applied_once \
		effect::startup_fresh_db_invalidation_rollback_then_external_disappearance_stays_not_applied \
		effect::startup_fresh_db_invalidation_uncertain_partial_changed_and_exact_aba_map_to_fieldless_ambiguous \
		evidence_races::startup_fresh_db_invalidation_binding_rejects_reopened_and_cross_root_journals_before_removal \
		evidence_races::startup_fresh_db_invalidation_capture_rejects_database_changes_between_its_two_snapshots \
		evidence_races::startup_fresh_db_invalidation_final_database_namespace_and_journal_races_refuse_authority \
		evidence_races::startup_fresh_db_invalidation_refuses_conflicting_lookalikes_and_retains_stable_ambient_quarantine; do \
		timeout 10s grep -Fqx "$$prefix$$name: test" "$$listed"; \
	done; \
	authority=crates/forge/src/client/startup_reconciliation/usr_rollback_fresh_db_invalidation_authority.rs; \
	effect=crates/forge/src/client/startup_reconciliation/usr_rollback_fresh_db_invalidation_authority/effect_reconciliation.rs; \
	proof=crates/forge/src/client/startup_reconciliation/activation_namespace/fresh_db_invalidation_proof.rs; \
	reconciliation_root=crates/forge/src/client/startup_reconciliation.rs; \
	namespace_root=crates/forge/src/client/startup_reconciliation/activation_namespace.rs; \
	startup_gate=crates/forge/src/client/startup_gate.rs; \
	startup_recovery=crates/forge/src/client/startup_recovery.rs; \
	exact=crates/forge/src/db/state/exact_fresh_transition_removal.rs; \
	tests=crates/forge/src/client/startup_reconciliation/usr_rollback_fresh_db_invalidation_authority/tests.rs; \
	admission_tests=crates/forge/src/client/startup_reconciliation/usr_rollback_fresh_db_invalidation_authority/tests/admission.rs; \
	effect_tests=crates/forge/src/client/startup_reconciliation/usr_rollback_fresh_db_invalidation_authority/tests/effect.rs; \
	race_tests=crates/forge/src/client/startup_reconciliation/usr_rollback_fresh_db_invalidation_authority/tests/evidence_races.rs; \
	support=crates/forge/src/client/startup_reconciliation/usr_rollback_fresh_db_invalidation_authority/tests/support.rs; \
	timeout 10s grep -Fqx 'mod usr_rollback_fresh_db_invalidation_authority;' "$$reconciliation_root"; \
	timeout 10s grep -Fqx 'mod effect_reconciliation;' "$$authority"; \
	timeout 10s grep -Fqx 'mod fresh_db_invalidation_proof;' "$$namespace_root"; \
	timeout 10s grep -Fqx '#[cfg(test)]' "$$authority"; \
	timeout 10s grep -Fqx 'mod tests;' "$$authority"; \
	timeout 10s grep -Fqx 'pub(in crate::client) enum UsrRollbackFreshDbInvalidationAdmission<'\''reservation> {' "$$authority"; \
	for variant in '    NotApplicable,' '    Deferred,' '    Apply(UsrRollbackFreshDbInvalidationApplyAuthority<'\''reservation>),' '    Finish(UsrRollbackFreshDbInvalidationFinishAuthority<'\''reservation>),'; do \
		timeout 10s grep -Fqx "$$variant" "$$authority"; \
	done; \
	timeout 10s grep -Fqx 'pub(in crate::client) enum UsrRollbackFreshDbInvalidationApplyReconciliation<'\''reservation> {' "$$effect"; \
	for variant in '    Applied(UsrRollbackFreshDbInvalidationEffectAuthority<'\''reservation>),' '    NotApplied,' '    Ambiguous,'; do \
		timeout 10s grep -Fqx "$$variant" "$$effect"; \
	done; \
	for seal in UsrRollbackFreshDbInvalidationSeal UsrRollbackFreshDbInvalidationEffectSeal; do \
		seal_file="$$startup_gate"; \
		if timeout 10s test "$$seal" = UsrRollbackFreshDbInvalidationEffectSeal; then seal_file="$$startup_recovery"; fi; \
		timeout 10s grep -Fqx "pub(in crate::client) struct $$seal {" "$$seal_file"; \
		timeout 10s awk -v seal="$$seal" '$$0 == "pub(in crate::client) struct " seal " {" { state = 1; next } state == 1 && $$0 == "    _private: ()," { field = 1; next } state == 1 && $$0 == "}" { found = field; exit !found } END { exit !found }' "$$seal_file"; \
		timeout 10s awk -v seal="$$seal" '$$0 == "impl " seal " {" { state = 1; next } state == 1 && $$0 == "    #[cfg(test)]" { gated = 1; next } state == 1 && gated && $$0 == "    pub(in crate::client) fn new_for_test() -> Self {" { test_only = 1; gated = 0; next } state == 1 && gated { exit 1 } state == 1 && $$0 ~ /^    .*fn new/ { exit 1 } state == 1 && $$0 == "}" { found = test_only; exit !found } END { exit !found }' "$$seal_file"; \
	done; \
	if timeout 10s rg -n 'UsrRollbackFreshDbInvalidation(Effect)?Seal::(new|new_for_test)\(' crates/forge/src/client --glob '*.rs' --glob '!**/tests/**' --glob '!**/tests.rs' --glob '!**/*_tests.rs'; then exit 1; fi; \
	if timeout 10s rg -n -F 'UsrRollbackFreshDbInvalidationAuthority::capture' crates/forge/src/client --glob '*.rs' --glob '!**/tests/**' --glob '!**/tests.rs' --glob '!**/usr_rollback_fresh_db_invalidation_authority.rs'; then exit 1; fi; \
	timeout 10s rg -n -w -F 'remove_exact_fresh_transition' crates/forge/src/client --glob '*.rs' --glob '!**/tests/**' --glob '!**/tests.rs' --glob '!**/*_tests.rs' > "$$bare_calls"; \
	timeout 10s test "$$( timeout 10s wc -l < "$$bare_calls" )" = 1; \
	timeout 10s test "$$( timeout 10s cut -d: -f1 "$$bare_calls" )" = "$$effect"; \
	timeout 10s grep -Fq 'state_db.remove_exact_fresh_transition(preimage)' "$$effect"; \
	timeout 10s sed -n '/^impl<'\''reservation> UsrRollbackFreshDbInvalidationApplyAuthority/,/^}/p' "$$effect" > "$$apply_body"; \
	timeout 10s sed -n '/^impl<'\''reservation> UsrRollbackFreshDbInvalidationFinishAuthority/,/^}/p' "$$effect" > "$$finish_body"; \
	timeout 10s grep -q . "$$apply_body"; \
	timeout 10s grep -q . "$$finish_body"; \
	timeout 10s test "$$( timeout 10s grep -Fc 'remove_exact_fresh_transition' "$$apply_body" )" = 0; \
	timeout 10s test "$$( timeout 10s grep -Fc 'remove_exact_fresh_transition' "$$finish_body" )" = 0; \
	binding_line="$$( timeout 10s grep -nE 'require_(effect_|journal_)?binding|has_binding' "$$effect" | timeout 10s head -n 1 | timeout 10s cut -d: -f1 )"; \
	removal_line="$$( timeout 10s grep -nF 'remove_exact_fresh_transition' "$$effect" | timeout 10s cut -d: -f1 )"; \
	timeout 10s test -n "$$binding_line"; \
	timeout 10s test -n "$$removal_line"; \
	timeout 10s test "$$binding_line" -lt "$$removal_line"; \
	timeout 10s test "$$( timeout 10s grep -Fc 'increment_removal_call_count();' "$$effect" )" = 1; \
	timeout 10s grep -Fq 'reset_removal_call_count();' "$$apply_body"; \
	timeout 10s grep -Fq 'reset_removal_call_count();' "$$finish_body"; \
	timeout 10s test "$$( timeout 10s grep -Fc 'RollbackActionOutcome::Applied' "$$effect" )" = 1; \
	timeout 10s test "$$( timeout 10s grep -Fc 'RollbackActionOutcome::AlreadySatisfied' "$$effect" )" = 1; \
	timeout 10s grep -Fqx '    origin: RollbackActionOutcome,' "$$effect"; \
	timeout 10s grep -Fqx '    pub(in crate::client) fn origin_for_test(&self) -> RollbackActionOutcome {' "$$effect"; \
	if timeout 10s rg -U -n '#\[derive\([^]]*Clone[^]]*\)\]\n(?:pub\([^)]*\)[[:space:]]+)?(?:struct|enum)[[:space:]]+(UsrRollbackFreshDbInvalidation(?:Authority|ApplyAuthority|FinishAuthority|DatabaseEvidence|EffectAuthority)|ExactFreshTransition(?:Observation|Preimage|Absence))' "$$authority" "$$effect" "$$exact"; then exit 1; fi; \
	if timeout 10s rg -n 'impl Clone for (UsrRollbackFreshDbInvalidation(?:Authority|ApplyAuthority|FinishAuthority|DatabaseEvidence|EffectAuthority)|ExactFreshTransition(?:Observation|Preimage|Absence))' "$$authority" "$$effect" "$$exact"; then exit 1; fi; \
	timeout 10s test "$$( timeout 10s grep -Fc 'inspect_exact_fresh_transition' "$$authority" )" = 2; \
	timeout 10s test "$$( timeout 10s rg -n -w -F 'inspect_current_database' "$$authority" | timeout 10s wc -l )" = 5; \
	timeout 10s test "$$( timeout 10s rg -n -w -F 'inspect_current_database' "$$effect" | timeout 10s wc -l )" = 3; \
	timeout 10s grep -Fq 'fresh_db_invalidation_plan_is_exact' "$$authority"; \
	for field in \
		'record.operation == Operation::NewState' \
		'record.phase == Phase::FreshDbInvalidationIntent' \
		'rollback.previous_archive == RollbackAction::NotRequired' \
		'rollback.fresh_db == RollbackAction::Pending' \
		'rollback.boot == BootRollback::NotRequired' \
		'rollback.external_effects_may_remain'; do \
		timeout 10s grep -Fq "$$field" "$$authority"; \
	done; \
	timeout 10s sed -E 's,//.*$$,,' "$$authority" "$$effect" "$$proof" > "$$production_code"; \
	if timeout 10s rg -n '^[[:space:]]*(loop|while|for)[[:space:]]|=[[:space:]]*(loop|while|for)[[:space:]]' "$$production_code"; then exit 1; fi; \
	if timeout 10s rg -n 'retry|rollback_successor|forward_successor|\.advance[[:space:]]*\(|persist_|reopen_canonical|run_transaction_triggers|run_system_triggers|cleanup|clear_transition_if_matches|remove_transition_if_matches|delete_metadata|\.execute[[:space:]]*\(|\.transaction[[:space:]]*\(|std::fs::rename|fs::rename' "$$production_code"; then exit 1; fi; \
	if timeout 10s rg -n 'pub\([^)]*\)[[:space:]]+fn[[:space:]]+.*(path|descriptor|raw_fd|preimage|absence)|AsRawFd|IntoRawFd|FromRawFd|BorrowedFd|OwnedFd' "$$authority" "$$effect" "$$proof"; then exit 1; fi; \
	for marker in \
		'ExactFreshTransitionRemovalFault::BeforeTransaction' \
		'ExactFreshTransitionRemovalFault::BetweenProvenanceAndStateDelete' \
		'ExactFreshTransitionRemovalFault::BeforeCommit' \
		'ExactFreshTransitionRemovalFault::AfterCommit' \
		'ExactFreshTransitionRemovalFault::AfterCommitWithUncertainReport' \
		'ExactFreshTransitionRemovalFault::AfterCommitWithPartialRestoration' \
		'ExactFreshTransitionRemovalFault::AfterCommitWithChangedRestoration' \
		'ExactFreshTransitionRemovalFault::AfterCommitWithExactRestoration' \
		'arm_after_exact_fresh_transition_removal_attempt_before_reconciliation' \
		'fresh_db_invalidation_removal_call_count()'; do \
		timeout 10s grep -Fq "$$marker" "$$effect_tests"; \
	done; \
	timeout 10s grep -Fq 'PreviousDatabaseEvidence' "$$support"; \
	timeout 10s grep -Fq 'ExactFreshTransitionObservation::JointlyAbsent' "$$support"; \
	timeout 10s grep -Fq 'FreshRowLayout::Present' "$$admission_tests"; \
	timeout 10s grep -Fq 'FreshRowLayout::JointlyAbsent' "$$admission_tests"; \
	timeout 10s grep -Fq 'fresh_db_invalidation_removal_call_count(), 0' "$$effect_tests"; \
	timeout 10s grep -Fq 'fresh_db_invalidation_removal_call_count(), 1' "$$effect_tests"; \
	timeout 10s grep -Fq 'canonical_journal' "$$race_tests"; \
	timeout 10s grep -Fq 'transition_quarantine_path' "$$race_tests"; \
	for file in "$$authority" "$$effect" "$$proof" "$$reconciliation_root" "$$namespace_root" "$$startup_gate" "$$startup_recovery" "$$tests" "$$admission_tests" "$$effect_tests" "$$race_tests" "$$support" misc/make/startup-fresh-db-invalidation-effect-tests.mk Makefile misc/make/help.mk; do \
		timeout 10s test "$$( timeout 10s wc -l < "$$file" )" -le 1000; \
	done; \
	timeout 1200s $(CARGO) test -p forge --lib "$$prefix" -- --test-threads=1
