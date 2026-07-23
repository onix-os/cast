.PHONY: forge-startup-active-reblit-boot-repair-complete-test

forge-startup-active-reblit-boot-repair-complete-test: \
	forge-startup-active-reblit-boot-repair-unverified-test
	@set -euo pipefail; \
	timeout 10s mkdir -p "$(TOP_DIR)/target"; \
	listed="$$( timeout 10s mktemp "$(TOP_DIR)/target/active-reblit-boot-complete-list.XXXXXXXXXXXX" )"; \
	trap 'timeout 10s rm -f "$$listed"' EXIT; \
	timeout 300s $(CARGO) test -p forge --lib -- --list | timeout 300s tee "$$listed" >/dev/null; \
	timeout 10s grep -q . "$$listed"; \
	prefix='client::startup_gate::usr_rollback_active_reblit::tests::'; \
	classifier_prefix='client::startup_reconciliation::activation_namespace::active_reblit_boot_repair_started_error_classification::tests::'; \
	for name in \
		boot_repair_complete_route_capture_faults::startup_active_reblit_boot_repair_complete_capture_faults_propagate_without_effects \
		boot_repair_complete_route_matrix::startup_active_reblit_boot_repair_complete_routes_all_exact_success_outcomes_without_effects \
		boot_repair_complete_route_exclusions::startup_active_reblit_boot_repair_complete_rejects_every_inexact_route_shape \
		boot_repair_complete_route_exclusions::startup_active_reblit_boot_repair_complete_authority_is_bound_to_its_open_journal \
		boot_repair_complete_route_evidence_races::startup_active_reblit_boot_repair_complete_rejects_database_provenance_journal_and_namespace_races \
		boot_repair_complete_route_storage_faults::startup_active_reblit_boot_repair_complete_all_journal_faults_converge_without_finalization; do \
		timeout 10s grep -Fqx "$$prefix$$name: test" "$$listed"; \
	done; \
	timeout 10s grep -Fqx "$$classifier_prefix"'complete_route_uses_the_same_fail_closed_operational_boundary: test' "$$listed"; \
	tests=crates/forge/src/client/startup_gate/usr_rollback_active_reblit/tests; \
	for module in \
		boot_repair_complete_route_capture_faults \
		boot_repair_complete_route_matrix \
		boot_repair_complete_route_exclusions \
		boot_repair_complete_route_evidence_races \
		boot_repair_complete_route_storage_faults; do \
		timeout 10s grep -Fqx "mod $$module;" "$$tests/mod.rs"; \
	done; \
	gate=crates/forge/src/client/startup_gate.rs; \
	orchestrator=crates/forge/src/client/startup_gate/usr_rollback_active_reblit.rs; \
	reconciliation=crates/forge/src/client/startup_reconciliation.rs; \
	focused=crates/forge/src/client/startup_reconciliation/focused_test_exports.rs; \
	recovery=crates/forge/src/client/startup_recovery.rs; \
	evidence=crates/forge/src/client/startup_reconciliation/active_reblit_boot_repair_evidence.rs; \
	authority=crates/forge/src/client/startup_reconciliation/usr_rollback_active_reblit_boot_repair_complete_authority.rs; \
	proof=crates/forge/src/client/startup_reconciliation/activation_namespace/active_reblit_boot_repair_complete_proof.rs; \
	classifier=crates/forge/src/client/startup_reconciliation/activation_namespace/active_reblit_boot_repair_started_error_classification.rs; \
	effect=crates/forge/src/client/startup_recovery/usr_rollback_active_reblit_boot_repair_complete.rs; \
	timeout 10s grep -Fq 'UsrRollbackActiveReblitBootRepairCompleteSeal' "$$gate"; \
	timeout 10s test "$$( timeout 10s grep -Fc 'Phase::BootRepairComplete =>' "$$orchestrator" )" = 1; \
	timeout 10s test "$$( timeout 10s grep -Fc 'UsrRollbackActiveReblitBootRepairCompleteAuthority::capture(' "$$orchestrator" )" = 1; \
	timeout 10s test "$$( timeout 10s grep -Fc 'persist_usr_rollback_active_reblit_boot_repair_complete_and_reopen(journal, authority)?;' "$$orchestrator" )" = 1; \
	if timeout 10s grep -Fq 'Phase::BootRepairRequired =>' "$$orchestrator"; then exit 1; else status="$$?"; timeout 10s test "$$status" = 1; fi; \
	timeout 10s grep -Fq 'Phase::BootRepairStarted =>' "$$orchestrator"; \
	timeout 10s grep -Fq 'persist_usr_rollback_active_reblit_boot_repair_unverified_and_reopen' "$$orchestrator"; \
	timeout 10s grep -Fq 'mod usr_rollback_active_reblit_boot_repair_complete_authority;' "$$reconciliation"; \
	timeout 10s grep -Fq 'mod usr_rollback_active_reblit_boot_repair_complete;' "$$recovery"; \
	timeout 10s grep -Fq 'active_reblit_completed_boot_repair_plan_is_exact' "$$evidence"; \
	timeout 10s grep -Fq 'complete_namespace_error_is_structural(&source)' "$$authority"; \
	timeout 10s grep -Fq 'complete_namespace_error_is_structural' "$$classifier"; \
	timeout 10s grep -Fq 'arm_active_reblit_boot_repair_complete_capture_fault' "$$focused"; \
	if timeout 10s grep -Fq 'Err(_) => return Ok(UsrRollbackActiveReblitBootRepairCompleteAdmission::Deferred)' "$$authority"; then exit 1; else status="$$?"; timeout 10s test "$$status" = 1; fi; \
	timeout 10s grep -Fq 'ActiveReblitBootRepairCompleteCaptureFault::PermissionDenied' "$$tests/boot_repair_complete_route_capture_faults.rs"; \
	timeout 10s grep -Fq 'nix::libc::EACCES' "$$proof"; \
	timeout 10s test "$$( timeout 10s grep -Fc 'self.record.boot_repair_rollback_complete_successor()' "$$authority" )" = 1; \
	timeout 10s test "$$( timeout 10s grep -Fc 'journal.advance(&source_record, &successor)' "$$effect" )" = 1; \
	timeout 10s test "$$( timeout 10s rg -c 'journal\.advance\(' "$$effect" )" = 1; \
	complete_arm="$$( timeout 10s awk '/Phase::BootRepairComplete => \{/{found=1} found{print} /Phase::RollbackComplete => \{/{exit}' "$$orchestrator" )"; \
	timeout 10s grep -Fq 'UsrRollbackActiveReblitBootRepairCompleteAuthority::capture' <<<"$$complete_arm"; \
	timeout 10s grep -Fq 'persist_usr_rollback_active_reblit_boot_repair_complete_and_reopen' <<<"$$complete_arm"; \
	if timeout 10s rg -q 'boot::|synchronize_boot|synchronize_active|finalize_usr|journal\.delete|\.remove\(|fs::rename|fs::remove' "$$evidence" "$$authority" "$$proof" "$$effect"; then exit 1; else status="$$?"; timeout 10s test "$$status" = 1; fi; \
	if timeout 10s rg -q 'finalize_usr|boot::|synchronize_boot|synchronize_active' <<<"$$complete_arm"; then exit 1; else status="$$?"; timeout 10s test "$$status" = 1; fi; \
	for file in \
		"$$gate" "$$orchestrator" "$$reconciliation" "$$focused" "$$recovery" \
		"$$evidence" "$$authority" "$$proof" "$$classifier" "$$effect" \
		crates/forge/src/client/startup_reconciliation/activation_namespace.rs \
		crates/forge/src/client/startup_reconciliation/activation_namespace/candidate_preserve_proof.rs \
		"$$tests/mod.rs" "$$tests/support.rs" "$$tests"/boot_repair_complete_route_*.rs \
		misc/make/startup-active-reblit-boot-repair-complete-tests.mk; do \
		timeout 10s test "$$( timeout 10s wc -l < "$$file" )" -le 1000; \
	done; \
	timeout 1800s $(CARGO) test -p forge --lib startup_active_reblit_boot_repair_complete -- --test-threads=1
