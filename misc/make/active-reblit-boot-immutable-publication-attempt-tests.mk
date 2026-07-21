ACTIVE_REBLIT_BOOT_IMMUTABLE_ATTEMPT_TOP_DIR := $(abspath $(dir $(lastword $(MAKEFILE_LIST)))/../..)
CARGO ?= cargo

.PHONY: forge-active-reblit-boot-immutable-publication-attempt-test

forge-active-reblit-boot-immutable-publication-attempt-test: host-storage-safety-test forge-active-reblit-boot-publication-preflight-test forge-boot-publication-receipt-state-test forge-linux-descriptor-boot-publication-parent-test
	@set -euo pipefail; \
	mkdir -p "$(ACTIVE_REBLIT_BOOT_IMMUTABLE_ATTEMPT_TOP_DIR)/target"; \
	listed="$$( mktemp "$(ACTIVE_REBLIT_BOOT_IMMUTABLE_ATTEMPT_TOP_DIR)/target/active-reblit-boot-immutable-attempt-list.XXXXXXXXXXXX" )"; \
	trap 'rm -f "$$listed"' EXIT; \
	$(CARGO) test --manifest-path "$(ACTIVE_REBLIT_BOOT_IMMUTABLE_ATTEMPT_TOP_DIR)/Cargo.toml" -p forge --lib -- --list | tee "$$listed" >/dev/null; \
	test -s "$$listed"; \
	prefix='client::active_reblit_boot_publication_preflight::immutable_attempt::tests::'; \
	test "$$( grep -Ec "^$$prefix.*: test$$" "$$listed" )" = 4; \
	for name in \
		routing::alias_and_distinct_routes_preserve_global_plan_order \
		integration::staged_alias_attempt_publishes_in_phase_order_and_terminally_observes_exact \
		durable_state::pre_effect_journal_identity_drift_fails_before_any_publication \
		failures::leaf_failure_stops_before_later_outputs_and_retains_pending_started; do \
		grep -Fqx "$$prefix$$name: test" "$$listed"; \
	done; \
	staging="$(ACTIVE_REBLIT_BOOT_IMMUTABLE_ATTEMPT_TOP_DIR)/crates/forge/src/client/boot/active_reblit_boot_sync_staging/immutable_publication_attempt.rs"; \
	attempt="$(ACTIVE_REBLIT_BOOT_IMMUTABLE_ATTEMPT_TOP_DIR)/crates/forge/src/client/boot/active_reblit_boot_publication_preflight/immutable_attempt.rs"; \
	leaf="$(ACTIVE_REBLIT_BOOT_IMMUTABLE_ATTEMPT_TOP_DIR)/crates/forge/src/client/boot/active_reblit_mounted_boot_topology/capture/publication_targets/immutable_leaf.rs"; \
	tests="$(ACTIVE_REBLIT_BOOT_IMMUTABLE_ATTEMPT_TOP_DIR)/crates/forge/src/client/boot/active_reblit_boot_publication_preflight/immutable_attempt/tests"; \
	grep -Fq 'pub(in crate::client) fn attempt_immutable_boot_publication(' "$$staging"; \
	grep -Fq 'for (plan_index, output) in self.plan.outputs().enumerate() {' "$$attempt"; \
	grep -Fq '.publish_preflighted_immutable_leaf(' "$$attempt"; \
	grep -Fq 'let terminal_states = terminal_namespace_assessment(&self)?;' "$$attempt"; \
	grep -Fq '.revalidate_publication_targets()' "$$attempt"; \
	grep -Fq 'ActiveReblitBootImmutableLeafPublicationError::DestinationStateChanged' "$$leaf"; \
	if rg -n 'Phase::BootSyncComplete|forward_successor|advance_record_binding|promote_boot_publication|clear_boot_publication|delete_boot_publication' "$$staging" "$$attempt" "$$leaf"; then exit 1; else status="$$?"; test "$$status" = 1; fi; \
	if rg --pcre2 -U -n '#\[derive\([^]]*(?:Clone|Copy)[^]]*\)\]\s*pub\(in crate::client\) struct StagedExactActiveReblitBootPublication|impl(?:<[^>]+>)?\s+(?:Clone|Copy)\s+for\s+StagedExactActiveReblitBootPublication' "$$attempt"; then exit 1; else status="$$?"; test "$$status" = 1; fi; \
	for file in "$$staging" "$$attempt" "$$leaf" "$$tests.rs" "$$tests"/*.rs "$(ACTIVE_REBLIT_BOOT_IMMUTABLE_ATTEMPT_TOP_DIR)/misc/make/active-reblit-boot-immutable-publication-attempt-tests.mk"; do \
		test "$$( wc -l < "$$file" )" -le 1000; \
	done; \
	$(CARGO) test --manifest-path "$(ACTIVE_REBLIT_BOOT_IMMUTABLE_ATTEMPT_TOP_DIR)/Cargo.toml" -p forge --lib "$$prefix" -- --test-threads=1
