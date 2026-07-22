STARTUP_ACTIVE_REBLIT_COMMIT_CLEANUP_TOP_DIR := $(abspath $(dir $(lastword $(MAKEFILE_LIST)))/../..)
CARGO ?= cargo

.PHONY: forge-startup-active-reblit-commit-cleanup-authority-test

forge-startup-active-reblit-commit-cleanup-authority-test:
	@set -euo pipefail; \
	mkdir -p "$(STARTUP_ACTIVE_REBLIT_COMMIT_CLEANUP_TOP_DIR)/target"; \
	listed="$$( mktemp "$(STARTUP_ACTIVE_REBLIT_COMMIT_CLEANUP_TOP_DIR)/target/startup-active-reblit-commit-cleanup-list.XXXXXXXXXXXX" )"; \
	trap 'rm -f "$$listed"' EXIT; \
	$(CARGO) test --manifest-path "$(STARTUP_ACTIVE_REBLIT_COMMIT_CLEANUP_TOP_DIR)/Cargo.toml" -p forge --lib -- --list | tee "$$listed" >/dev/null; \
	test -s "$$listed"; \
	authority_prefix='client::startup_gate::usr_rollback_active_reblit::tests::commit_cleanup_startup_authority::'; \
	test "$$( grep -Ec "^$$authority_prefix.*: test$$" "$$listed" )" = 5; \
	for name in \
		exact_apply_and_finish_authorities_are_read_only_and_consumable \
		stable_receipt_selection_and_wrapper_mismatches_defer \
		database_change_inside_admission_fails_stop \
		fresh_namespace_and_same_byte_record_replacements_fail_stop \
		non_commit_decided_sources_are_not_applicable; do \
		grep -Fqx "$$authority_prefix$$name: test" "$$listed"; \
	done; \
	classifier_prefix='client::startup_reconciliation::activation_namespace::active_reblit_commit_cleanup_proof::classification_tests::'; \
	test "$$( grep -Ec "^$$classifier_prefix.*: test$$" "$$listed" )" = 1; \
	grep -Fqx "$$classifier_prefix"'only_stable_shape_mismatches_may_defer: test' "$$listed"; \
	root="$(STARTUP_ACTIVE_REBLIT_COMMIT_CLEANUP_TOP_DIR)/crates/forge/src/client"; \
	reconciliation="$$root/startup_reconciliation.rs"; \
	namespace_root="$$root/startup_reconciliation/activation_namespace.rs"; \
	capture_mod="$$root/startup_reconciliation/activation_namespace/capture/mod.rs"; \
	authority="$$root/startup_reconciliation/active_reblit_commit_cleanup_authority.rs"; \
	proof="$$root/startup_reconciliation/activation_namespace/active_reblit_commit_cleanup_proof.rs"; \
	capture="$$root/startup_reconciliation/activation_namespace/capture/active_reblit_commit_cleanup.rs"; \
	tests_mod="$$root/startup_gate/usr_rollback_active_reblit/tests/mod.rs"; \
	tests="$$root/startup_gate/usr_rollback_active_reblit/tests/commit_cleanup_startup_authority.rs"; \
	grep -Fqx 'mod active_reblit_commit_cleanup_authority;' "$$reconciliation"; \
	grep -Fqx 'mod active_reblit_commit_cleanup_proof;' "$$namespace_root"; \
	grep -Fqx 'mod active_reblit_commit_cleanup;' "$$capture_mod"; \
	grep -Fqx 'mod commit_cleanup_startup_authority;' "$$tests_mod"; \
	grep -Fq 'load_exact_promoted_boot_publication_receipt_state' "$$authority"; \
	grep -Fq 'state_db.get(state_id)' "$$authority"; \
	grep -Fq 'ActiveReblitCommitCleanupNamespaceInspection::begin(' "$$authority"; \
	grep -Fq 'capture_for_startup_recovery(installation)' "$$authority"; \
	grep -Fq 'journal.record_binding(' "$$authority"; \
	grep -Fq 'ActiveReblitCommitCleanupAdmission::Apply(' "$$authority"; \
	grep -Fq 'ActiveReblitCommitCleanupAdmission::Finish(' "$$authority"; \
	grep -Fq 'self.namespace.into_effect_evidence()' "$$authority"; \
	if rg -n '[.]advance\(|advance_record_binding|delete_record_binding|promote_boot_publication_receipt|stage_boot_publication_receipt|boot::|synchronize_(boot|databases|excluding)|run_(transaction|system)_triggers|finalize_usr|fs::(rename|remove|write|set_permissions)|renameat|unlinkat|Command::new|nix::mount|libc::mount' "$$authority" "$$proof" "$$capture"; then exit 1; else status="$$?"; test "$$status" = 1; fi; \
	if rg --pcre2 -n '#\[derive\([^]]*(?:Clone|Copy)[^]]*\)\]\s*pub\(in crate::client\) struct ActiveReblitCommitCleanup(?:Apply|Finish)(?:Effect)?Authority|impl(?:<[^>]+>)?\s+(?:Clone|Copy)\s+for\s+ActiveReblitCommitCleanup(?:Apply|Finish)(?:Effect)?Authority' "$$authority"; then exit 1; else status="$$?"; test "$$status" = 1; fi; \
	for file in "$$reconciliation" "$$namespace_root" "$$capture_mod" "$$authority" "$$proof" "$$capture" "$$tests_mod" "$$tests" "$(STARTUP_ACTIVE_REBLIT_COMMIT_CLEANUP_TOP_DIR)/misc/make/startup-active-reblit-commit-cleanup-authority-tests.mk"; do \
		test "$$( wc -l < "$$file" )" -le 1000; \
	done; \
	$(CARGO) test --manifest-path "$(STARTUP_ACTIVE_REBLIT_COMMIT_CLEANUP_TOP_DIR)/Cargo.toml" -p forge --lib "$$authority_prefix" -- --test-threads=1; \
	$(CARGO) test --manifest-path "$(STARTUP_ACTIVE_REBLIT_COMMIT_CLEANUP_TOP_DIR)/Cargo.toml" -p forge --lib "$$classifier_prefix" -- --test-threads=1
