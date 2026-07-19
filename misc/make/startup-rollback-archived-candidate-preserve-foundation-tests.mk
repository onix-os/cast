.PHONY: forge-startup-usr-rollback-archived-candidate-preserve-foundation-test

forge-startup-usr-rollback-archived-candidate-preserve-foundation-test:
	@set -euo pipefail; \
	timeout 10s mkdir -p "$(TOP_DIR)/target"; \
	listed="$$( timeout 10s mktemp "$(TOP_DIR)/target/archived-candidate-preserve-foundation-list.XXXXXXXXXXXX" )"; \
	trap 'timeout 10s rm -f "$$listed"' EXIT; \
	timeout 300s $(CARGO) test -p forge --lib -- --list | timeout 300s tee "$$listed" >/dev/null; \
	prefix='client::startup_reconciliation::usr_rollback_candidate_preserve_authority::tests::archived_effect::'; \
	timeout 10s test "$$( timeout 10s grep -c "^$$prefix.*: test$$" "$$listed" )" = 12; \
	for name in \
		startup_archived_candidate_child_move_reconciles_every_raw_report_for_every_origin \
		startup_archived_candidate_final_pre_revalidation_refuses_rebound_move_parents \
		startup_archived_candidate_non_namespace_races_never_escape_the_authority_sandwich \
		startup_archived_candidate_pre_faults_stop_at_exact_ordered_prefixes_without_a_move \
		startup_archived_candidate_pre_races_fail_at_every_boundary_without_a_move \
		startup_archived_candidate_reconciliation_closing_rejects_post_classification_child_moves \
		startup_archived_candidate_reconciliation_uses_fresh_namespace_not_the_raw_report \
		post_move_durability::startup_archived_durable_revalidation_is_fresh_and_never_repeats_barriers \
		post_move_durability::startup_archived_post_authority_rejects_journal_database_and_provenance_races_without_another_move \
		post_move_durability::startup_archived_post_durability_has_one_exact_order_for_applied_and_already_satisfied_matrices \
		post_move_durability::startup_archived_post_faults_stop_at_exact_prefixes_for_both_origins \
		post_move_durability::startup_archived_post_races_fail_at_every_boundary_for_both_origins; do \
		timeout 10s grep -Fqx "$$prefix$$name: test" "$$listed"; \
	done; \
	capture_base=crates/forge/src/client/startup_reconciliation/activation_namespace/capture/archived_candidate_preserve; \
	capture="$$capture_base.rs"; \
	target="$$capture_base/target_durability.rs"; \
	post="$$capture_base/post_move_durability.rs"; \
	reconciliation="$$capture_base/effect/reconciliation.rs"; \
	proof_base=crates/forge/src/client/startup_reconciliation/activation_namespace/candidate_preserve_proof/archived_effect; \
	proof="$$proof_base.rs"; \
	proof_post="$$proof_base/post_move_durability.rs"; \
	authority=crates/forge/src/client/startup_reconciliation/usr_rollback_candidate_preserve_authority/archived_effect.rs; \
	authority_root=crates/forge/src/client/startup_reconciliation/usr_rollback_candidate_preserve_authority.rs; \
	tests_base=crates/forge/src/client/startup_reconciliation/usr_rollback_candidate_preserve_authority/tests/archived_effect; \
	tests="$$tests_base.rs"; \
	post_tests="$$tests_base/post_move_durability.rs"; \
	timeout 10s grep -Fq 'record.operation != Operation::ActivateArchived' "$$capture"; \
	timeout 10s grep -Fq 'record.phase != Phase::CandidatePreserveIntent' "$$capture"; \
	timeout 10s grep -Fq 'candidate.marker.links != 2' "$$capture"; \
	timeout 10s grep -Fq 'CanonicalSlotMismatch' "$$capture"; \
	timeout 10s grep -Fq 'ArchivedCandidatePreserveLayout::StagedWithCanonicalSlot' "$$capture"; \
	timeout 10s grep -Fq 'ArchivedCandidatePreserveLayout::Preserved' "$$capture"; \
	timeout 10s grep -Fq 'require_staged_to_preserved' "$$capture" "$$reconciliation"; \
	timeout 10s test "$$( timeout 10s grep -Fc 'renameat2_noreplace_once(staging, c"usr", target, c"usr")' "$$target" )" = 1; \
	timeout 10s test "$$( timeout 10s grep -Fc 'attempt_raw_move_once(&parents.staging, &parents.target)' "$$target" )" = 1; \
	if timeout 10s rg -n 'renameat2_exchange|RENAME_EXCHANGE|unlinkat|remove_(dir|file)|hard_link' "$$target" "$$post" "$$proof" "$$proof_post" "$$authority"; then exit 1; else status="$$?"; timeout 10s test "$$status" = 1; fi; \
	if timeout 10s rg -n 'match[[:space:]]+raw_report|raw_report\.(is_ok|is_err)|if[[:space:]]+raw_report' "$$reconciliation"; then exit 1; else status="$$?"; timeout 10s test "$$status" = 1; fi; \
	timeout 10s grep -Fq 'ClassifiedFreshNamespace::NotApplied { snapshot, projection }' "$$reconciliation"; \
	classification="$$( timeout 10s grep -nF 'let classification = classify_fresh_namespace(' "$$reconciliation" | timeout 10s cut -d: -f1 )"; \
	closing_hook="$$( timeout 10s grep -nF 'run_before_reconciliation_closing();' "$$reconciliation" | timeout 10s cut -d: -f1 )"; \
	closing_match="$$( timeout 10s grep -nF 'match classification {' "$$reconciliation" | timeout 10s cut -d: -f1 )"; \
	timeout 10s test "$$classification" -lt "$$closing_hook"; \
	timeout 10s test "$$closing_hook" -lt "$$closing_match"; \
	parent_close="$$( timeout 10s grep -nF 'parents.revalidate_value_identity(installation).map_err(|_| ())?;' "$$reconciliation" | timeout 10s cut -d: -f1 )"; \
	snapshot_close="$$( timeout 10s grep -nF 'snapshot.revalidate_retained().map_err(|_| ())?;' "$$reconciliation" | timeout 10s cut -d: -f1 )"; \
	projection_close="$$( timeout 10s grep -nF 'let closing_projection =' "$$reconciliation" | timeout 10s cut -d: -f1 )"; \
	timeout 10s test "$$parent_close" -lt "$$snapshot_close"; \
	timeout 10s test "$$snapshot_close" -lt "$$projection_close"; \
	timeout 10s grep -Fq 'arm_before_archived_candidate_preserve_move_reconciliation_closing' "$$reconciliation" "$$tests"; \
	pre_candidate="$$( timeout 10s grep -nF 'require_boundary(DurabilityBoundary::CandidateSync)?;' "$$target" | timeout 10s cut -d: -f1 )"; \
	pre_staging="$$( timeout 10s grep -nF 'require_boundary(DurabilityBoundary::StagingParentSync)?;' "$$target" | timeout 10s cut -d: -f1 )"; \
	pre_target="$$( timeout 10s grep -nF 'require_boundary(DurabilityBoundary::TargetParentSync)?;' "$$target" | timeout 10s cut -d: -f1 )"; \
	pre_roots="$$( timeout 10s grep -nF 'require_boundary(DurabilityBoundary::RootsParentSync)?;' "$$target" | timeout 10s cut -d: -f1 )"; \
	pre_final="$$( timeout 10s grep -nF 'require_boundary(DurabilityBoundary::FinalPreCapture)?;' "$$target" | timeout 10s cut -d: -f1 )"; \
	timeout 10s test "$$pre_candidate" -lt "$$pre_staging"; \
	timeout 10s test "$$pre_staging" -lt "$$pre_target"; \
	timeout 10s test "$$pre_target" -lt "$$pre_roots"; \
	timeout 10s test "$$pre_roots" -lt "$$pre_final"; \
	post_candidate="$$( timeout 10s grep -nF 'require_boundary(DurabilityBoundary::CandidateSync)?;' "$$post" | timeout 10s cut -d: -f1 )"; \
	post_staging="$$( timeout 10s grep -nF 'require_boundary(DurabilityBoundary::StagingParentSync)?;' "$$post" | timeout 10s cut -d: -f1 )"; \
	post_target="$$( timeout 10s grep -nF 'require_boundary(DurabilityBoundary::TargetParentSync)?;' "$$post" | timeout 10s cut -d: -f1 )"; \
	post_roots="$$( timeout 10s grep -nF 'require_boundary(DurabilityBoundary::RootsParentSync)?;' "$$post" | timeout 10s cut -d: -f1 )"; \
	post_final="$$( timeout 10s grep -nF 'require_boundary(DurabilityBoundary::FinalPostCapture)?;' "$$post" | timeout 10s cut -d: -f1 )"; \
	timeout 10s test "$$post_candidate" -lt "$$post_staging"; \
	timeout 10s test "$$post_staging" -lt "$$post_target"; \
	timeout 10s test "$$post_target" -lt "$$post_roots"; \
	timeout 10s test "$$post_roots" -lt "$$post_final"; \
	for symbol in \
		ArchivedCandidatePreserveTargetDurabilityFaultPoint \
		ArchivedCandidatePreserveTargetDurabilityEvent \
		arm_before_archived_candidate_preserve_pre_candidate_sync \
		arm_before_archived_candidate_preserve_pre_staging_parent_sync \
		arm_before_archived_candidate_preserve_pre_target_parent_sync \
		arm_before_archived_candidate_preserve_pre_roots_parent_sync \
		arm_before_archived_candidate_preserve_pre_final_capture \
		arm_before_archived_candidate_preserve_pre_move_revalidation; do \
		timeout 10s grep -Fq "$$symbol" "$$target" "$$tests"; \
	done; \
	for symbol in \
		ArchivedCandidatePreservePostMoveDurabilityFaultPoint \
		ArchivedCandidatePreservePostMoveDurabilityEvent \
		arm_before_archived_candidate_preserve_post_candidate_sync \
		arm_before_archived_candidate_preserve_post_staging_parent_sync \
		arm_before_archived_candidate_preserve_post_target_parent_sync \
		arm_before_archived_candidate_preserve_post_roots_parent_sync \
		arm_before_archived_candidate_preserve_post_final_capture \
		arm_before_archived_candidate_preserve_durable_post_revalidation_capture; do \
		timeout 10s grep -Fq "$$symbol" "$$post" "$$post_tests"; \
	done; \
	timeout 10s test "$$( timeout 10s grep -hF 'for epoch in FixtureEpoch::ALL {' "$$tests" "$$post_tests" | timeout 10s wc -l )" = 2; \
	timeout 10s grep -Fq 'PostAuthorityRace::ALL' "$$post_tests"; \
	timeout 10s grep -Fq 'Self::Journal =>' "$$post_tests"; \
	timeout 10s grep -Fq 'Self::DatabaseOwnership' "$$post_tests"; \
	timeout 10s grep -Fq 'Self::Provenance' "$$post_tests"; \
	timeout 10s grep -Fqx 'mod archived_effect;' "$$authority_root"; \
	if timeout 10s rg -n 'ArchivedCandidatePreserveEffectSeal|into_archived_effect_for_test|into_archived_finish_for_test' crates/forge/src/client/startup_gate crates/forge/src/client/startup_recovery --glob '*.rs'; then exit 1; else status="$$?"; timeout 10s test "$$status" = 1; fi; \
	if timeout 10s rg -n '\.advance[[:space:]]*\(|persist_|dispatch_|rollback_successor|run_(transaction|system)_triggers' "$$capture" "$$target" "$$post" "$$reconciliation" "$$proof" "$$proof_post" "$$authority"; then exit 1; else status="$$?"; timeout 10s test "$$status" = 1; fi; \
	production_code="$$( timeout 10s sed -E 's,//.*$$,,' "$$target" "$$post" "$$reconciliation" "$$proof" "$$proof_post" "$$authority" )"; \
	if timeout 10s rg -n '^[[:space:]]*(loop|while)[[:space:]]|=[[:space:]]*(loop|while)[[:space:]]|retry' <<<"$$production_code"; then exit 1; else status="$$?"; timeout 10s test "$$status" = 1; fi; \
	for file in "$$capture" "$$target" "$$post" "$$reconciliation" "$$proof" "$$proof_post" "$$authority" "$$tests" "$$post_tests" \
		crates/forge/src/client/startup_reconciliation.rs \
		crates/forge/src/client/startup_reconciliation/activation_namespace.rs \
		crates/forge/src/client/startup_reconciliation/activation_namespace/capture/mod.rs \
		crates/forge/src/client/startup_reconciliation/activation_namespace/candidate_preserve_proof.rs \
		"$$authority_root" misc/make/startup-rollback-archived-candidate-preserve-foundation-tests.mk; do \
		timeout 10s test "$$( timeout 10s wc -l < "$$file" )" -le 1000; \
	done; \
	timeout 1200s $(CARGO) test -p forge --lib "$$prefix" -- --test-threads=1
