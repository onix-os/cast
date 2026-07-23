STARTUP_ACTIVE_REBLIT_COMMIT_CLEANUP_EFFECT_TOP_DIR := $(abspath $(dir $(lastword $(MAKEFILE_LIST)))/../..)
CARGO ?= cargo

.PHONY: forge-startup-active-reblit-commit-cleanup-effect-test

forge-startup-active-reblit-commit-cleanup-effect-test:
	@set -euo pipefail; \
	mkdir -p "$(STARTUP_ACTIVE_REBLIT_COMMIT_CLEANUP_EFFECT_TOP_DIR)/target"; \
	listed="$$( mktemp "$(STARTUP_ACTIVE_REBLIT_COMMIT_CLEANUP_EFFECT_TOP_DIR)/target/startup-active-reblit-commit-cleanup-effect-list.XXXXXXXXXXXX" )"; \
	trap 'rm -f "$$listed"' EXIT; \
	$(CARGO) test --manifest-path "$(STARTUP_ACTIVE_REBLIT_COMMIT_CLEANUP_EFFECT_TOP_DIR)/Cargo.toml" -p forge --lib -- --list | tee "$$listed" >/dev/null; \
	test -s "$$listed"; \
	prefix='client::startup_gate::usr_rollback_active_reblit::tests::commit_cleanup_effect::'; \
	test "$$( grep -Ec "^$$prefix.*: test$$" "$$listed" )" = 3; \
	for name in single_exchange_report_is_never_the_semantic_outcome apply_and_finish_use_the_same_exact_durability_suffix post_exchange_durability_fault_reenters_finish_without_second_exchange; do \
		grep -Fqx "$$prefix$$name: test" "$$listed"; \
	done; \
	root="$(STARTUP_ACTIVE_REBLIT_COMMIT_CLEANUP_EFFECT_TOP_DIR)/crates/forge/src/client"; \
	reconciliation="$$root/startup_reconciliation.rs"; \
	namespace_root="$$root/startup_reconciliation/activation_namespace.rs"; \
	capture_mod="$$root/startup_reconciliation/activation_namespace/capture/mod.rs"; \
	capture="$$root/startup_reconciliation/activation_namespace/capture/active_reblit_commit_cleanup.rs"; \
	pre="$$root/startup_reconciliation/activation_namespace/capture/active_reblit_commit_cleanup/pre_exchange_safety.rs"; \
	exchange="$$root/startup_reconciliation/activation_namespace/capture/active_reblit_commit_cleanup/effect.rs"; \
	reconcile="$$root/startup_reconciliation/activation_namespace/capture/active_reblit_commit_cleanup/effect/reconciliation.rs"; \
	durability="$$root/startup_reconciliation/activation_namespace/capture/active_reblit_commit_cleanup/post_exchange_durability.rs"; \
	authority="$$root/startup_reconciliation/active_reblit_commit_cleanup_authority.rs"; \
	authority_effect="$$root/startup_reconciliation/active_reblit_commit_cleanup_authority/effect.rs"; \
	tests_mod="$$root/startup_gate/usr_rollback_active_reblit/tests/mod.rs"; \
	tests="$$root/startup_gate/usr_rollback_active_reblit/tests/commit_cleanup_effect.rs"; \
	grep -Fqx 'mod effect;' "$$authority"; \
	grep -Fqx 'mod effect;' "$$capture"; \
	grep -Fqx 'mod post_exchange_durability;' "$$capture"; \
	grep -Fqx 'mod pre_exchange_safety;' "$$capture"; \
	grep -Fqx 'mod commit_cleanup_effect;' "$$tests_mod"; \
	test "$$( grep -Fc 'let raw_report = attempt_raw_exchange_once(&parents);' "$$exchange" )" = 1; \
	test "$$( grep -Fc 'let attempted = prepared.attempt_exchange_once(' "$$authority_effect" )" = 1; \
	grep -Fq 'renameat2_exchange_once(' "$$exchange"; \
	grep -Fq 'ActiveReblitCommitCleanupExchangeReconciliation::NotApplied' "$$authority_effect"; \
	grep -Fq 'ActiveReblitCommitCleanupExchangeReconciliation::Ambiguous' "$$authority_effect"; \
	previous_tree="$$( grep -nF 'parents.previous.sync_retained_tree()' "$$durability" | head -n1 | cut -d: -f1 )"; \
	previous_wrapper="$$( grep -nF 'parents.previous_wrapper.sync_all()' "$$durability" | head -n1 | cut -d: -f1 )"; \
	replacement_wrapper="$$( grep -nF 'parents.replacement_wrapper.sync_all()' "$$durability" | head -n1 | cut -d: -f1 )"; \
	roots_parent="$$( grep -nF 'parents.roots.sync_all()' "$$durability" | head -n1 | cut -d: -f1 )"; \
	quarantine_parent="$$( grep -nF 'parents.quarantine.sync_all()' "$$durability" | head -n1 | cut -d: -f1 )"; \
	final_capture="$$( grep -nF 'let final_finish = capture_snapshot(' "$$durability" | head -n1 | cut -d: -f1 )"; \
	test "$$previous_tree" -lt "$$previous_wrapper"; test "$$previous_wrapper" -lt "$$replacement_wrapper"; \
	test "$$replacement_wrapper" -lt "$$roots_parent"; test "$$roots_parent" -lt "$$quarantine_parent"; \
	test "$$quarantine_parent" -lt "$$final_capture"; \
	if rg -n '[.]advance\(|advance_record_binding|delete_record_binding|promote_boot_publication_receipt|stage_boot_publication_receipt|boot::|run_(transaction|system)_triggers[(]|finalize_usr|std::fs::(rename|remove|write|set_permissions)|Command::new|nix::mount|libc::mount' "$$pre" "$$exchange" "$$reconcile" "$$durability" "$$authority_effect"; then exit 1; else status="$$?"; test "$$status" = 1; fi; \
	if rg --pcre2 -n '#\[derive\([^]]*(?:Clone|Copy)[^]]*\)\]\s*pub\(in crate::client\) struct ActiveReblitCommitCleanup(?:Apply|Finish|Pending|Durable).*Authority|impl(?:<[^>]+>)?\s+(?:Clone|Copy)\s+for\s+ActiveReblitCommitCleanup(?:Apply|Finish|Pending|Durable).*Authority' "$$authority" "$$authority_effect"; then exit 1; else status="$$?"; test "$$status" = 1; fi; \
	for file in "$$reconciliation" "$$namespace_root" "$$capture_mod" "$$capture" "$$pre" "$$exchange" "$$reconcile" "$$durability" "$$authority" "$$authority_effect" "$$tests_mod" "$$tests" "$(STARTUP_ACTIVE_REBLIT_COMMIT_CLEANUP_EFFECT_TOP_DIR)/misc/make/startup-active-reblit-commit-cleanup-effect-tests.mk"; do \
		test "$$( wc -l < "$$file" )" -le 1000; \
	done; \
	$(CARGO) test --manifest-path "$(STARTUP_ACTIVE_REBLIT_COMMIT_CLEANUP_EFFECT_TOP_DIR)/Cargo.toml" -p forge --lib "$$prefix" -- --test-threads=1
