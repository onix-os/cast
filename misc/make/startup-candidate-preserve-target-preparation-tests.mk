.PHONY: forge-startup-usr-rollback-candidate-preserve-target-preparation-test

forge-startup-usr-rollback-candidate-preserve-target-preparation-test:
	@set -eu; \
	listed="$$( timeout 300s $(CARGO) test -p forge --lib -- --list )"; \
	timeout 10s grep -q . <<<"$$listed"; \
	count="$$( timeout 10s grep -c '^client::startup_reconciliation::usr_rollback_candidate_preserve_authority::tests::target_preparation::.*: test$$' <<<"$$listed" )"; \
	timeout 10s test "$$count" = 3; \
	for test in \
		client::startup_reconciliation::usr_rollback_candidate_preserve_authority::tests::target_preparation::startup_candidate_target_preparation_selects_every_new_state_prefix_for_every_origin \
		client::startup_reconciliation::usr_rollback_candidate_preserve_authority::tests::target_preparation::startup_candidate_target_preparation_keeps_archived_and_active_reblit_unsupported \
		client::startup_reconciliation::usr_rollback_candidate_preserve_authority::tests::target_preparation::startup_candidate_target_preparation_selection_is_binding_first_for_every_lease; do \
		timeout 10s grep -Fqx "$$test: test" <<<"$$listed"; \
	done; \
	authority=crates/forge/src/client/startup_reconciliation/usr_rollback_candidate_preserve_authority.rs; \
	proof=crates/forge/src/client/startup_reconciliation/activation_namespace/candidate_preserve_proof.rs; \
	namespace=crates/forge/src/client/startup_reconciliation/activation_namespace/capture/new_state_candidate_target_preparation.rs; \
	startup_recovery=crates/forge/src/client/startup_recovery.rs; \
	production_dispatch=crates/forge/src/client/startup_recovery/usr_rollback_candidate_preserve_dispatch.rs; \
	tests=crates/forge/src/client/startup_reconciliation/usr_rollback_candidate_preserve_authority/tests; \
	create_lease=UsrRollbackNewStateCandidatePreserveCreateTargetLease; \
	normalize_lease=UsrRollbackNewStateCandidatePreserveNormalizeTargetLease; \
	timeout 10s test "$$( timeout 10s rg -l '^pub\(in crate::client\) struct UsrRollbackCandidatePreserveEffectSeal \{' crates/forge/src/client --glob '*.rs' )" = "$$production_dispatch"; \
	timeout 10s grep -Fq '    UsrRollbackCandidatePreserveEffectSeal, UsrRollbackCandidatePreserveReady,' "$$startup_recovery"; \
	timeout 10s grep -Fqx 'pub(in crate::client) struct UsrRollbackCandidatePreserveEffectSeal {' "$$production_dispatch"; \
	timeout 10s awk '$$0 == "pub(in crate::client) struct UsrRollbackCandidatePreserveEffectSeal {" { state = 1; next } state == 1 && $$0 == "    _private: ()," { field = 1; next } state == 1 && $$0 == "}" { found = field; exit !found } END { exit !found }' "$$production_dispatch"; \
	seal_impl="$$( timeout 10s sed -n '/^impl UsrRollbackCandidatePreserveEffectSeal {/,/^}/p' "$$production_dispatch" )"; \
	timeout 10s test "$$( timeout 10s grep -Fc '    fn new() -> Self {' <<<"$$seal_impl" )" = 1; \
	timeout 10s test "$$( timeout 10s grep -Fc '    pub(in crate::client) fn new_for_test() -> Self {' <<<"$$seal_impl" )" = 1; \
	timeout 10s test "$$( timeout 10s grep -Fc 'UsrRollbackCandidatePreserveEffectSeal::new();' "$$production_dispatch" )" = 1; \
	timeout 10s test "$$( timeout 10s grep -Fc '.into_effect_selection(&effect_seal, &journal)?' "$$production_dispatch" )" = 1; \
	production_seal_calls="$$( timeout 10s rg -n -F 'UsrRollbackCandidatePreserveEffectSeal::new();' crates/forge/src/client --glob '*.rs' --glob '!**/tests/**' --glob '!**/tests.rs' --glob '!**/*_tests.rs' --glob '!**/*_tests/**' )"; \
	timeout 10s test "$$( timeout 10s grep -c . <<<"$$production_seal_calls" )" = 1; \
	timeout 10s test "$$( timeout 10s cut -d: -f1 <<<"$$production_seal_calls" )" = "$$production_dispatch"; \
	production_selection_calls="$$( timeout 10s rg -n -F '.into_effect_selection(&effect_seal, &journal)?' crates/forge/src/client --glob '*.rs' --glob '!**/tests/**' --glob '!**/tests.rs' --glob '!**/*_tests.rs' --glob '!**/*_tests/**' )"; \
	timeout 10s test "$$( timeout 10s grep -c . <<<"$$production_selection_calls" )" = 1; \
	timeout 10s test "$$( timeout 10s cut -d: -f1 <<<"$$production_selection_calls" )" = "$$production_dispatch"; \
	timeout 10s test "$$( timeout 10s grep -Fc '    CreateNewStateTarget(UsrRollbackNewStateCandidatePreserveCreateTargetLease<'\''reservation>),' "$$authority" )" = 1; \
	timeout 10s test "$$( timeout 10s grep -Fc '    NormalizeNewStateTarget(UsrRollbackNewStateCandidatePreserveNormalizeTargetLease<'\''reservation>),' "$$authority" )" = 1; \
	timeout 10s test "$$( timeout 10s grep -Fc '    MoveNewState(UsrRollbackNewStateCandidatePreserveEffectLease<'\''reservation>),' "$$authority" )" = 1; \
	timeout 10s test "$$( timeout 10s grep -Fc '    Unsupported,' "$$authority" )" = 1; \
	timeout 10s test "$$( timeout 10s grep -Fc "pub(in crate::client) struct $$create_lease<'reservation> {" "$$authority" )" = 1; \
	timeout 10s test "$$( timeout 10s grep -Fc "pub(in crate::client) struct $$normalize_lease<'reservation> {" "$$authority" )" = 1; \
	for lease in "$$create_lease" "$$normalize_lease"; do \
		timeout 10s awk -v declaration="pub(in crate::client) struct $$lease<'reservation> {" '$$0 == declaration { active = 1; seen = 1; next } active && $$0 == "}" { closed = 1; active = 0; next } active && /pub/ { bad = 1 } active && /^[[:space:]]+[A-Za-z_][A-Za-z0-9_]*:/ { fields++ } END { exit !(seen && closed && fields > 0 && !bad) }' "$$authority"; \
		if timeout 10s rg -n "impl(<'reservation>)?[[:space:]]+$$lease" "$$authority"; then exit 1; fi; \
	done; \
	timeout 10s test "$$( timeout 10s grep -Fc 'UsrRollbackCandidatePreserveTopology::NewStateStaged' "$$authority" )" -ge 1; \
	timeout 10s test "$$( timeout 10s grep -Fc 'UsrRollbackCandidatePreserveTopology::NewStateStagedWithTargetResidue' "$$authority" )" -ge 1; \
	timeout 10s test "$$( timeout 10s grep -Fc 'UsrRollbackCandidatePreserveTopology::NewStateStagedWithEmptyQuarantine' "$$authority" )" -ge 1; \
	timeout 10s awk '$$0 == "    fn into_apply_effect_selection(" { active = 1; next } active && $$0 == "        self.require_journal_binding(journal)?;" { found = 1; exit } active && ($$0 ~ /self\.namespace/ || $$0 ~ /self\.installation/ || $$0 ~ /inspect_current_database/) { exit 1 } END { exit !found }' "$$authority"; \
	if timeout 10s rg -n 'dispatcher|dispatch_' "$$authority" "$$proof" "$$namespace"; then exit 1; fi; \
	if timeout 10s rg -n 'renameat|rename\(|mkdirat|mkdir\(|create_dir|chmod|set_permissions|remove_dir|remove_file|unlinkat|linkat|write_all|sync_all|sync_data|\.sync\(|\.advance\(|forward_successor|rollback_successor|clear_transition_if_matches|remove_transition_if_matches|insert_fresh_metadata|delete_metadata|run_transaction_triggers|run_system_triggers|\.execute\(|\.transaction\(|\.delete\(' "$$authority" "$$proof" "$$namespace"; then exit 1; fi; \
	if timeout 10s rg -n 'AsRawFd|IntoRawFd|FromRawFd' "$$authority" "$$proof" "$$namespace"; then exit 1; fi; \
	timeout 10s grep -Fqx 'const RESTRICTIVE_RESIDUE_MODES: [u32; 7] = [0o000, 0o100, 0o200, 0o300, 0o400, 0o500, 0o600];' "$$tests/target_preparation.rs"; \
	timeout 10s test "$$( timeout 10s grep -Fc 'for source in CandidateSource::ALL {' "$$tests/target_preparation.rs" )" = 2; \
	timeout 10s test "$$( timeout 10s grep -Fc 'for usr_outcome in [RollbackActionOutcome::Applied, RollbackActionOutcome::AlreadySatisfied] {' "$$tests/target_preparation.rs" )" = 2; \
	timeout 10s test "$$( timeout 10s grep -Fc 'new_state_candidate_preserve_move_attempt_count()' "$$tests/target_preparation.rs" )" = 10; \
	for file in "$$authority" "$$proof" "$$namespace" "$$tests.rs" "$$tests/support.rs" "$$tests/target_preparation.rs" misc/make/startup-candidate-preserve-target-preparation-tests.mk Makefile; do \
		timeout 10s test "$$( timeout 10s wc -l < "$$file" )" -le 1000; \
	done; \
	timeout 1200s $(CARGO) test -p forge --lib \
		'client::startup_reconciliation::usr_rollback_candidate_preserve_authority::tests::target_preparation::' \
		-- --test-threads=1
