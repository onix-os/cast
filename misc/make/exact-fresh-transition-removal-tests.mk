.PHONY: forge-exact-fresh-transition-removal-test

forge-exact-fresh-transition-removal-test:
	@set -euo pipefail; \
	timeout 10s mkdir -p "$(TOP_DIR)/target"; \
	listed="$$( timeout 10s mktemp "$(TOP_DIR)/target/exact-fresh-transition-removal-list.XXXXXXXXXXXX" )"; \
	trap 'timeout 10s rm -f "$$listed"' EXIT; \
	timeout 300s $(CARGO) test -p forge --lib -- --list | timeout 30s tee "$$listed" >/dev/null; \
	timeout 10s grep -q . "$$listed"; \
	prefix='db::state::exact_fresh_transition_removal::tests::'; \
	count="$$( timeout 10s awk -v prefix="$$prefix" 'index($$0, prefix) == 1 && $$0 ~ /: test$$/ { count += 1 } END { print count + 0 }' "$$listed" )"; \
	timeout 10s test "$$count" = 15; \
	for name in \
		mutation_faults::exact_fresh_removal_atomically_deletes_state_selections_and_provenance_only \
		mutation_faults::exact_fresh_removal_pre_and_in_transaction_faults_preserve_one_complete_preimage_without_retry \
		mutation_faults::exact_fresh_removal_rejects_every_state_selection_provenance_and_transition_change \
		observation::exact_fresh_inspection_refuses_cleared_foreign_and_split_provenance_states \
		observation::exact_fresh_inspection_refuses_orphan_selections_token_rebinding_and_multiple_inflight_rows \
		observation::exact_fresh_inspection_rejects_malformed_transition_evidence \
		observation::exact_fresh_inspection_returns_only_exact_joint_absence \
		observation::exact_fresh_inspection_returns_the_complete_present_preimage \
		reconciliation_concurrency::exact_fresh_removal_after_commit_error_reconciles_joint_absence_as_success \
		reconciliation_concurrency::exact_fresh_removal_exact_post_commit_restoration_is_ambiguous_aba \
		reconciliation_concurrency::exact_fresh_removal_partial_and_changed_post_error_states_are_ambiguous \
		reconciliation_concurrency::exact_fresh_removal_rolled_back_then_external_absence_is_definitely_not_applied \
		reconciliation_concurrency::exact_fresh_removal_stale_preimage_cannot_delete_an_independent_replacement \
		reconciliation_concurrency::exact_fresh_removal_uncertain_report_with_joint_absence_is_ambiguous \
		reconciliation_concurrency::exact_fresh_preimage_and_absence_are_bound_to_their_database_capability; do \
		timeout 10s grep -Fqx "$$prefix$$name: test" "$$listed"; \
	done; \
	production=crates/forge/src/db/state/exact_fresh_transition_removal.rs; \
	provenance=crates/forge/src/db/state/metadata_provenance.rs; \
	state_root=crates/forge/src/db/state/mod.rs; \
	tests=crates/forge/src/db/state/exact_fresh_transition_removal/tests.rs; \
	observation=crates/forge/src/db/state/exact_fresh_transition_removal/tests/observation.rs; \
	mutation=crates/forge/src/db/state/exact_fresh_transition_removal/tests/mutation_faults.rs; \
	reconciliation=crates/forge/src/db/state/exact_fresh_transition_removal/tests/reconciliation_concurrency.rs; \
	support=crates/forge/src/db/state/exact_fresh_transition_removal/tests/support.rs; \
	timeout 10s grep -Fqx 'mod exact_fresh_transition_removal;' "$$state_root"; \
	timeout 10s grep -Fqx '#[cfg(test)]' "$$production"; \
	timeout 10s grep -Fqx 'mod tests;' "$$production"; \
	timeout 10s rg -U -q '^    pub\(crate\) fn inspect_exact_fresh_transition\(\n        &self,\n        state_id: Id,\n        transition_id: &TransitionId,\n    \) -> Result<ExactFreshTransitionObservation, ExactFreshTransitionInspectionError> \{' "$$production"; \
	timeout 10s rg -U -q '^    pub\(crate\) fn remove_exact_fresh_transition\(\n        &self,\n        preimage: ExactFreshTransitionPreimage,\n    \) -> Result<ExactFreshTransitionAbsence, ExactFreshTransitionRemovalError> \{' "$$production"; \
	preimage_definition="$$( timeout 10s sed -n '/^pub(crate) struct ExactFreshTransitionPreimage {/,/^}/p' "$$production" )"; \
	timeout 10s test -n "$$preimage_definition"; \
	if timeout 10s grep -Eq 'pub\([^)]*\)[[:space:]]+[[:alnum:]_]+:' <<<"$$preimage_definition"; then exit 1; fi; \
	absence_definition="$$( timeout 10s sed -n '/^pub(crate) struct ExactFreshTransitionAbsence {/,/^}/p' "$$production" )"; \
	timeout 10s test -n "$$absence_definition"; \
	if timeout 10s grep -Eq 'pub\([^)]*\)[[:space:]]+[[:alnum:]_]+:' <<<"$$absence_definition"; then exit 1; fi; \
	timeout 10s test "$$( timeout 10s grep -Fc '    database: Database,' "$$production" )" = 2; \
	evidence_attributes="$$( timeout 10s sed -n '/^#\[derive(Debug, Eq, PartialEq)\]/,/^impl Eq for ExactFreshTransitionAbsence {}/p' "$$production" )"; \
	timeout 10s test -n "$$evidence_attributes"; \
	if timeout 10s grep -Fq 'Clone' <<<"$$evidence_attributes"; then exit 1; fi; \
	timeout 10s test "$$( timeout 10s grep -Fc 'self.database.same_instance(&other.database)' "$$production" )" = 2; \
	production_removal_references="$$( timeout 10s rg -n -w -F 'remove_exact_fresh_transition' crates/forge/src --glob '*.rs' --glob '!**/tests/**' --glob '!**/tests.rs' )"; \
	timeout 10s test "$$( timeout 10s grep -c . <<<"$$production_removal_references" )" = 1; \
	timeout 10s test "$$( timeout 10s cut -d: -f1 <<<"$$production_removal_references" )" = "$$production"; \
	production_inspection_references="$$( timeout 10s rg -n -w -F 'inspect_exact_fresh_transition' crates/forge/src --glob '*.rs' --glob '!**/tests/**' --glob '!**/tests.rs' )"; \
	timeout 10s test "$$( timeout 10s grep -c . <<<"$$production_inspection_references" )" = 2; \
	timeout 10s test -z "$$( timeout 10s cut -d: -f1 <<<"$$production_inspection_references" | timeout 10s grep -Fvx "$$production" || true )"; \
	timeout 10s test "$$( timeout 10s rg -n -w -F 'remove_exact_fresh_transition_once' "$$production" | timeout 10s wc -l )" = 2; \
	timeout 10s test "$$( timeout 10s rg -n -w -F 'inspect_exact_fresh_transition_impl' "$$production" | timeout 10s wc -l )" = 3; \
	reset_line="$$( timeout 10s grep -nF '        reset_exact_fresh_transition_removal_transaction_attempts();' "$$production" | timeout 10s cut -d: -f1 )"; \
	binding_line="$$( timeout 10s grep -nF '        if !preimage.database.same_instance(self) {' "$$production" | timeout 10s cut -d: -f1 )"; \
	fault_line="$$( timeout 10s grep -nF '        let attempt = if exact_fresh_transition_removal_fault(' "$$production" | timeout 10s cut -d: -f1 )"; \
	increment_line="$$( timeout 10s grep -nF '            increment_exact_fresh_transition_removal_transaction_attempts();' "$$production" | timeout 10s cut -d: -f1 )"; \
	once_call_line="$$( timeout 10s grep -nF '            self.remove_exact_fresh_transition_once(&preimage)' "$$production" | timeout 10s cut -d: -f1 )"; \
	timeout 10s test "$$reset_line" -lt "$$binding_line"; \
	timeout 10s test "$$binding_line" -lt "$$fault_line"; \
	timeout 10s test "$$fault_line" -lt "$$increment_line"; \
	timeout 10s test "$$increment_line" -lt "$$once_call_line"; \
	hook_line="$$( timeout 10s grep -nF '        run_after_exact_fresh_transition_removal_attempt_before_reconciliation();' "$$production" | timeout 10s cut -d: -f1 )"; \
	observation_line="$$( timeout 10s grep -nF '        let observation = self.inspect_exact_fresh_transition(state_id, &transition_id);' "$$production" | timeout 10s cut -d: -f1 )"; \
	timeout 10s test -n "$$hook_line"; \
	timeout 10s test -n "$$observation_line"; \
	timeout 10s test "$$once_call_line" -lt "$$hook_line"; \
	timeout 10s test "$$hook_line" -lt "$$observation_line"; \
	timeout 10s grep -Fq 'if source.known_committed() =>' "$$production"; \
	timeout 10s grep -Fq 'if source.rolled_back_or_not_started() =>' "$$production"; \
	timeout 10s grep -Fq 'ExactFreshTransitionReconciliation::UncertainJointAbsence' "$$production"; \
	timeout 10s grep -Fq 'point: ExactFreshTransitionRemovalFault::AfterCommit' "$$production"; \
	once="$$( timeout 10s sed -n '/^    fn remove_exact_fresh_transition_once(/,/^    }/p' "$$production" )"; \
	timeout 10s test -n "$$once"; \
	timeout 10s test "$$( timeout 10s grep -Fc 'self.conn.exclusive_tx(|tx|' <<<"$$once" )" = 1; \
	timeout 10s test "$$( timeout 10s grep -Fc 'if provenance_changed != 1 {' <<<"$$once" )" = 1; \
	timeout 10s test "$$( timeout 10s grep -Fc 'if selections_changed != preimage.state.selections.len() {' <<<"$$once" )" = 1; \
	timeout 10s test "$$( timeout 10s grep -Fc 'if state_changed != 1 {' <<<"$$once" )" = 1; \
	provenance_line="$$( timeout 10s grep -nF '            let provenance_changed = metadata_provenance::delete_exact_metadata_provenance(' "$$production" | timeout 10s cut -d: -f1 )"; \
	selections_line="$$( timeout 10s grep -nF '            let selections_changed = diesel::delete(' "$$production" | timeout 10s cut -d: -f1 )"; \
	state_line="$$( timeout 10s grep -nF '            let state_changed = diesel::delete(' "$$production" | timeout 10s cut -d: -f1 )"; \
	timeout 10s test "$$provenance_line" -lt "$$selections_line"; \
	timeout 10s test "$$selections_line" -lt "$$state_line"; \
	timeout 10s grep -Fq 'model::state_selections::state_id.eq(i32::from(preimage.state.id))' <<<"$$once"; \
	timeout 10s grep -Fq 'model::state::transition_id.eq(preimage.transition_id.as_str())' <<<"$$once"; \
	provenance_delete="$$( timeout 10s sed -n '/^pub(super) fn delete_exact_metadata_provenance(/,/^}/p' "$$provenance" )"; \
	timeout 10s test -n "$$provenance_delete"; \
	for exact_filter in \
		'state_metadata_provenance::state_id.eq(i32::from(state))' \
		'state_metadata_provenance::os_release_sha256' \
		'expected.os_release_sha256.as_bytes().as_slice()' \
		'state_metadata_provenance::system_model_sha256' \
		'expected.system_model_sha256.as_bytes().as_slice()'; do \
		timeout 10s grep -Fq "$$exact_filter" <<<"$$provenance_delete"; \
	done; \
	exact_helper_references="$$( timeout 10s rg -n -w -F 'delete_exact_metadata_provenance' "$$production" "$$provenance" )"; \
	timeout 10s test "$$( timeout 10s grep -c . <<<"$$exact_helper_references" )" = 2; \
	production_code="$$( timeout 10s sed -E 's,//.*$$,,' "$$production"; timeout 10s sed -E 's,//.*$$,,' <<<"$$provenance_delete" )"; \
	if timeout 10s rg -n 'transition_journal|startup|namespace|run_transaction_triggers|run_system_triggers|root_links|archive_previous|rearchive_archived|preserve_failed|cleanup|retry|remove_transition_if_matches' <<<"$$production_code"; then exit 1; fi; \
	if timeout 10s rg -n '^[[:space:]]*(loop|while|for)[[:space:]]|=[[:space:]]*(loop|while|for)[[:space:]]' <<<"$$production_code"; then exit 1; fi; \
	timeout 10s grep -Fqx '    pub(super) const ALL: [Self; 8] = [' "$$support"; \
	for field in StateId Summary Description Created Kind Selections Provenance Transition; do \
		timeout 10s grep -Fq "Self::$$field" "$$support"; \
	done; \
	for fault in BeforeTransaction BetweenProvenanceAndStateDelete BeforeCommit; do \
		timeout 10s grep -Fq "ExactFreshTransitionRemovalFault::$$fault" "$$mutation"; \
	done; \
	for fault in AfterCommit AfterCommitWithUncertainReport AfterCommitWithPartialRestoration AfterCommitWithChangedRestoration AfterCommitWithExactRestoration; do \
		timeout 10s grep -Fq "ExactFreshTransitionRemovalFault::$$fault" "$$reconciliation"; \
	done; \
	timeout 10s grep -Fq 'arm_after_exact_fresh_transition_removal_attempt_before_reconciliation' "$$reconciliation"; \
	timeout 10s grep -Fq 'ExactFreshTransitionRemovalFault::BeforeCommit' "$$reconciliation"; \
	timeout 10s grep -Fq 'does not prove this invocation committed their removal' "$$reconciliation"; \
	timeout 10s grep -Fq 'exact_fresh_transition_removal_transaction_attempts()' "$$mutation"; \
	timeout 10s grep -Fq 'exact_fresh_transition_removal_transaction_attempts(), 0' "$$reconciliation"; \
	timeout 10s grep -Fq 'assert_ne!(first_actual, second_actual);' "$$reconciliation"; \
	timeout 10s grep -Fq 'assert_ne!(first_absence, second_absence);' "$$reconciliation"; \
	timeout 10s grep -Fq 'OrphanSelections { state_id, count }' "$$observation"; \
	timeout 10s grep -Fq 'count == 2' "$$observation"; \
	timeout 10s grep -Fq 'UnexpectedInFlightTransition {' "$$observation"; \
	timeout 10s grep -Fq 'MultipleInFlightTransitions { .. }' "$$observation"; \
	timeout 10s grep -Fq 'Database::new(path.to_str().unwrap())' "$$reconciliation"; \
	for file in "$$production" "$$provenance" "$$state_root" "$$tests" "$$observation" "$$mutation" "$$reconciliation" "$$support" misc/make/exact-fresh-transition-removal-tests.mk Makefile misc/make/help.mk; do \
		timeout 10s test "$$( timeout 10s wc -l < "$$file" )" -le 1000; \
	done; \
	timeout 1200s $(CARGO) test -p forge --lib "$$prefix" -- --test-threads=1
