.PHONY: forge-startup-usr-rollback-finalization-test

forge-startup-usr-rollback-finalization-test:
	@set -euo pipefail; \
	timeout 10s mkdir -p "$(TOP_DIR)/target"; \
	listed="$$( timeout 10s mktemp "$(TOP_DIR)/target/rollback-finalization-list.XXXXXXXXXXXX" )"; \
	refs="$$( timeout 10s mktemp "$(TOP_DIR)/target/rollback-finalization-refs.XXXXXXXXXXXX" )"; \
	trap 'timeout 10s rm -f "$$listed" "$$refs"' EXIT; \
	timeout 300s $(CARGO) test -p forge --lib -- --list | timeout 30s tee "$$listed" >/dev/null; \
	timeout 10s grep -q . "$$listed"; \
	prefix='client::startup_reconciliation::usr_rollback_finalization_authority::tests::'; \
	timeout 10s test "$$( timeout 10s grep -c "^$$prefix.*: test$$" "$$listed" )" = 5; \
	for name in \
		admission::startup_usr_rollback_finalization_admits_exact_current_and_historical_terminal_evidence \
		admission::startup_usr_rollback_finalization_rejects_inexact_phase_operation_plan_and_database \
		evidence::startup_usr_rollback_finalization_capture_sandwich_rejects_database_and_namespace_changes \
		evidence::startup_usr_rollback_finalization_revalidation_rejects_reopened_and_changed_authority \
		evidence::startup_usr_rollback_finalization_refuses_terminal_namespace_lookalikes; do \
		timeout 10s grep -Fqx "$$prefix$$name: test" "$$listed"; \
	done; \
	authority=crates/forge/src/client/startup_reconciliation/usr_rollback_finalization_authority.rs; \
	proof=crates/forge/src/client/startup_reconciliation/activation_namespace/rollback_finalization_proof.rs; \
	topology=crates/forge/src/client/startup_reconciliation/activation_namespace/candidate_preserve_proof.rs; \
	reconciliation_root=crates/forge/src/client/startup_reconciliation.rs; \
	namespace_root=crates/forge/src/client/startup_reconciliation/activation_namespace.rs; \
	orchestrator=crates/forge/src/client/startup_gate/usr_rollback_new_state.rs; \
	startup_gate=crates/forge/src/client/startup_gate.rs; \
	timeout 10s grep -Fqx 'mod usr_rollback_finalization_authority;' "$$reconciliation_root"; \
	timeout 10s grep -Fqx 'mod rollback_finalization_proof;' "$$namespace_root"; \
	timeout 10s grep -Fqx 'pub(in crate::client) struct UsrRollbackFinalizationSeal {' "$$orchestrator"; \
	seal_impl="$$( timeout 10s sed -n '/^impl UsrRollbackFinalizationSeal {/,/^}/p' "$$orchestrator" )"; \
	timeout 10s test "$$( timeout 10s grep -Fc '    pub(in crate::client) fn new_for_test() -> Self {' <<<"$$seal_impl" )" = 1; \
	if timeout 10s grep -Fq '    fn new() -> Self {' <<<"$$seal_impl"; then exit 1; else status="$$?"; timeout 10s test "$$status" = 1; fi; \
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
	timeout 10s rg -n -F 'UsrRollbackFinalizationAuthority::capture(' crates/forge/src/client --glob '*.rs' --glob '!**/tests/**' --glob '!**/tests.rs' --glob '!**/*_tests.rs' --glob '!**/*_tests/**' --glob '!**/usr_rollback_finalization_authority.rs' > "$$refs" || status="$$?"; \
	timeout 10s test "$${status:-1}" = 1; \
	timeout 10s test ! -s "$$refs"; \
	timeout 300s $(CARGO) test -p forge --lib "$$prefix" -- --test-threads=1
