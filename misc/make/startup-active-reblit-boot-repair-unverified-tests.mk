.PHONY: forge-startup-active-reblit-boot-repair-unverified-test

forge-startup-active-reblit-boot-repair-unverified-test: \
	forge-startup-active-reblit-boot-repair-required-test
	@set -euo pipefail; \
	timeout 10s mkdir -p "$(TOP_DIR)/target"; \
	listed="$$( timeout 10s mktemp "$(TOP_DIR)/target/active-reblit-boot-unverified-list.XXXXXXXXXXXX" )"; \
	trap 'timeout 10s rm -f "$$listed"' EXIT; \
	timeout 300s $(CARGO) test -p forge --lib -- --list | timeout 300s tee "$$listed" >/dev/null; \
	timeout 10s grep -q . "$$listed"; \
	prefix='client::startup_gate::usr_rollback_active_reblit::tests::'; \
	classifier_prefix='client::startup_reconciliation::activation_namespace::active_reblit_boot_repair_started_error_classification::tests::'; \
	for name in \
		boot_repair_unverified_capture_faults::startup_active_reblit_boot_repair_unverified_capture_faults_propagate_without_boot_or_journal_mutation \
		boot_repair_unverified_retention::startup_active_reblit_boot_repair_unverified_is_retained_exactly_for_manual_recovery \
		boot_repair_unverified_storage_faults::startup_active_reblit_boot_repair_unverified_all_five_faults_converge_without_boot; do \
		timeout 10s grep -Fqx "$$prefix$$name: test" "$$listed"; \
	done; \
	for name in \
		stable_shape_mismatches_are_structural_but_changed_evidence_is_not \
		missing_shapes_may_defer_but_operational_io_never_does; do \
		timeout 10s grep -Fqx "$$classifier_prefix$$name: test" "$$listed"; \
	done; \
	tests=crates/forge/src/client/startup_gate/usr_rollback_active_reblit/tests; \
	for module in \
		boot_repair_unverified_capture_faults \
		boot_repair_unverified_retention \
		boot_repair_unverified_storage_faults; do \
		timeout 10s grep -Fqx "mod $$module;" "$$tests/mod.rs"; \
	done; \
	orchestrator=crates/forge/src/client/startup_gate/usr_rollback_active_reblit.rs; \
	reconciliation=crates/forge/src/client/startup_reconciliation.rs; \
	recovery=crates/forge/src/client/startup_recovery.rs; \
	authority=crates/forge/src/client/startup_reconciliation/usr_rollback_active_reblit_boot_repair_unverified_authority.rs; \
	classifier=crates/forge/src/client/startup_reconciliation/activation_namespace/active_reblit_boot_repair_started_error_classification.rs; \
	timeout 10s grep -Fq 'Phase::BootRepairStarted =>' "$$orchestrator"; \
	timeout 10s grep -Fq 'persist_usr_rollback_active_reblit_boot_repair_unverified_and_reopen' "$$orchestrator"; \
	timeout 10s grep -Fq 'mod usr_rollback_active_reblit_boot_repair_unverified_authority;' "$$reconciliation"; \
	timeout 10s grep -Fq 'mod usr_rollback_active_reblit_boot_repair_unverified;' "$$recovery"; \
	grep -Fq 'Phase::BootRepairRequired =>' "$$orchestrator"; \
	grep -Fq 'persist_usr_rollback_active_reblit_boot_repair_start_and_reopen' "$$orchestrator"; \
	if timeout 10s rg -q 'start_and_attempt_usr_rollback_active_reblit_boot_repair|claim_for_active_reblit_boot_repair|synchronize_active_reblit_boot_repair' crates/forge/src/client; then exit 1; else status="$$?"; timeout 10s test "$$status" = 1; fi; \
	if timeout 10s grep -Fq 'Err(_) => return Ok(UsrRollbackActiveReblitBootRepairUnverifiedAdmission::Deferred)' "$$authority"; then exit 1; else status="$$?"; timeout 10s test "$$status" = 1; fi; \
	timeout 10s grep -Fq 'started_namespace_error_is_structural(&source)' "$$authority"; \
	timeout 10s grep -Fq 'NamespaceError::Journal(_)' "$$classifier"; \
	timeout 10s grep -Fq 'CaptureError::Deadline' "$$classifier"; \
	timeout 10s grep -Fq 'nix::libc::EACCES' "$$tests/boot_repair_unverified_capture_faults.rs" crates/forge/src/client/startup_reconciliation/activation_namespace/active_reblit_boot_repair_started_proof.rs; \
	timeout 10s grep -Fq 'RecoveryDisposition::ManualBootRepair' "$$tests/boot_repair_unverified_retention.rs"; \
	timeout 10s grep -Fq 'RecoveryBlocker::ManualBootRepair' "$$tests/boot_repair_unverified_retention.rs"; \
	for fault in temporary_sync update_exchange update_first_directory_sync displaced_unlink update_final_directory_sync; do \
		timeout 10s grep -Fq "$$fault" "$$tests/boot_repair_unverified_storage_faults.rs"; \
	done; \
	for file in \
		"$$orchestrator" "$$reconciliation" "$$recovery" "$$authority" "$$classifier" \
		crates/forge/src/client/startup_reconciliation/active_reblit_boot_repair_evidence.rs \
		crates/forge/src/client/startup_reconciliation/activation_namespace/active_reblit_boot_repair_started_proof.rs \
		crates/forge/src/client/startup_recovery/usr_rollback_active_reblit_boot_repair_unverified.rs \
		"$$tests/mod.rs" "$$tests/support.rs" "$$tests"/boot_repair_unverified_*.rs \
		misc/make/startup-active-reblit-boot-repair-unverified-tests.mk; do \
		timeout 10s test "$$( timeout 10s wc -l < "$$file" )" -le 1000; \
	done; \
	timeout 1800s $(CARGO) test -p forge --lib startup_active_reblit_boot_repair_unverified -- --test-threads=1; \
	timeout 300s $(CARGO) test -p forge --lib "$$classifier_prefix" -- --test-threads=1
