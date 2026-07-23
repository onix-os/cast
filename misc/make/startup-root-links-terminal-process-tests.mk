.PHONY: forge-startup-root-links-terminal-process-test

forge-startup-root-links-terminal-process-test:
	@set -euo pipefail; \
	mkdir -p "$(TOP_DIR)/target"; \
	listed="$$( mktemp "$(TOP_DIR)/target/root-links-terminal-process-list.XXXXXXXXXXXX" )"; \
	executed="$$( mktemp "$(TOP_DIR)/target/root-links-terminal-process-run.XXXXXXXXXXXX" )"; \
	trap 'rm -f "$$listed" "$$executed"' EXIT; \
	$(CARGO) test -p forge --lib -- --list | tee "$$listed" >/dev/null; \
	grep -q . "$$listed"; \
	shared=crates/forge/src/client/startup_gate/root_links_terminal_process_harness.rs; \
	new_state=crates/forge/src/client/startup_gate/usr_rollback_new_state/tests/root_links_terminal_process_kill.rs; \
	archived=crates/forge/src/client/startup_gate/usr_rollback_activate_archived/tests/root_links_terminal_process_kill.rs; \
	active=crates/forge/src/client/startup_gate/usr_rollback_active_reblit/tests/root_links_terminal_process_kill.rs; \
	new_state_mod=crates/forge/src/client/startup_gate/usr_rollback_new_state/tests/mod.rs; \
	archived_mod=crates/forge/src/client/startup_gate/usr_rollback_activate_archived/tests/mod.rs; \
	active_mod=crates/forge/src/client/startup_gate/usr_rollback_active_reblit/tests/mod.rs; \
	new_state_test='client::startup_gate::usr_rollback_new_state::tests::root_links_terminal_process_kill::startup_new_state_root_links_terminal_delete_process_kills_restart_cleanly'; \
	archived_test='client::startup_gate::usr_rollback_activate_archived::tests::root_links_terminal_process_kill::startup_activate_archived_root_links_terminal_delete_process_kills_restart_cleanly'; \
	active_test='client::startup_gate::usr_rollback_active_reblit::tests::root_links_terminal_process_kill::startup_active_reblit_root_links_terminal_delete_process_kills_restart_cleanly'; \
	for test_name in "$$new_state_test" "$$archived_test" "$$active_test"; do \
		grep -Fqx "$$test_name: test" "$$listed"; \
	done; \
	operation_count="$$( grep -c '::root_links_terminal_process_kill::.*: test$$' "$$listed" )"; \
	test "$$operation_count" = 3; \
	grep -Fqx '#[cfg(test)]' crates/forge/src/client/startup_gate.rs; \
	grep -Fqx 'mod root_links_terminal_process_harness;' crates/forge/src/client/startup_gate.rs; \
	for module in "$$new_state_mod" "$$archived_mod" "$$active_mod"; do \
		grep -Fqx 'mod root_links_terminal_process_kill;' "$$module"; \
	done; \
	grep -Fq 'pub(super) const ALL: [Self; 2] = [Self::Current, Self::Historical];' "$$shared"; \
	grep -Fq 'pub(super) const ALL: [Self; 6] = [' "$$shared"; \
	grep -Fq 'pub(super) fn arm_initial_bound_delete_kill(self, death: fn()) -> bool {' "$$shared"; \
	grep -Fq 'pub(super) fn arm_recovery_kill(self, death: fn()) {' "$$shared"; \
	for scenario in FinalPre BeforeBoundDeletePrivateUnlink PrivateUnlinked DeleteDirectorySynced RecoveryAfterCanonicalRestored RecoveryAfterJournalDirectorySynced; do \
		grep -Fq "Self::$$scenario" "$$shared"; \
	done; \
	epoch_count=2; \
	scenario_count=6; \
	recovery_death_scenario_count=2; \
	case_count=$$(( operation_count * epoch_count * scenario_count )); \
	initial_death_count="$$case_count"; \
	recovery_death_count=$$(( operation_count * epoch_count * recovery_death_scenario_count )); \
	final_recovery_count="$$case_count"; \
	child_count=$$(( initial_death_count + recovery_death_count + final_recovery_count )); \
	death_count=$$(( initial_death_count + recovery_death_count )); \
	test "$$case_count" = 36; \
	test "$$child_count" = 84; \
	test "$$death_count" = 48; \
	test "$$final_recovery_count" = 36; \
	grep -Fq 'Self::RecoveryAfterCanonicalRestored | Self::RecoveryAfterJournalDirectorySynced' "$$shared"; \
	grep -Fq 'PublicBindingRevalidationBoundary::BeforeBoundDeletePrivateUnlink' "$$shared"; \
	for boundary in CanonicalUnlinked DeleteDirectorySynced; do \
		grep -Fq "JournalDeleteDurabilityBoundary::$$boundary" "$$shared"; \
	done; \
	for boundary in CanonicalRestored JournalDirectorySynced; do \
		grep -Fq "DeleteResidueRecoveryDurabilityBoundary::$$boundary" "$$shared"; \
	done; \
	grep -Fq 'tail.len() == 8 + 1 + 16' "$$shared"; \
	grep -Fq "tail[8] == b'-'" "$$shared"; \
	grep -Fq 'assert_exact_record_file(&journal_path(root).join(&residues[0]), self.record, &self.frame);' "$$shared"; \
	grep -Fq 'assert_eq!(metadata.nlink(), 1);' "$$shared"; \
	grep -Fq 'assert_eq!(metadata.mode() & 0o7777, 0o600);' "$$shared"; \
	grep -Fq 'assert_eq!(metadata.uid(), nix::unistd::Uid::current().as_raw());' "$$shared"; \
	grep -Fq 'Command::new(env::current_exe().unwrap())' "$$shared"; \
	grep -Fq 'const CHILD_DEADLINE: Duration = Duration::from_secs(15);' "$$shared"; \
	grep -Fq 'nix::libc::kill(nix::libc::getpid(), nix::libc::SIGKILL)' "$$shared"; \
	grep -Fq 'self.expect_sigkill(ProcessRole::InitialCrash);' "$$shared"; \
	grep -Fq 'self.expect_sigkill(ProcessRole::RecoveryCrash);' "$$shared"; \
	grep -Fq 'self.spawn(ProcessRole::FinalRecover)' "$$shared"; \
	grep -Fq 'assert!(status.success(), "RootLinks final recovery failed: {status:?}");' "$$shared"; \
	grep -Fq 'journal.assert_raw(self.root, self.scenario.after_initial_crash());' "$$shared"; \
	grep -Fq 'journal.assert_raw(self.root, RawRecordState::Canonical);' "$$shared"; \
	grep -Fq 'journal.assert_raw(self.root, RawRecordState::Absent);' "$$shared"; \
	grep -Fq 'assert_host_temporary_path(&root);' "$$shared"; \
	grep -Fq 'assert_host_temporary_path(&control);' "$$shared"; \
	for fixture in "$$new_state" "$$archived" "$$active"; do \
		grep -Fq 'for epoch in ProcessEpoch::ALL {' "$$fixture"; \
		grep -Fq 'for scenario in RootLinksDeleteScenario::ALL {' "$$fixture"; \
		grep -Fq 'assert_eq!(cases, 12,' "$$fixture"; \
		grep -Fq 'CandidateSource::RootLinksComplete' "$$fixture"; \
		grep -Fq 'case.terminal_record(&control_dimensions(case.epoch))' "$$fixture"; \
		grep -Fq 'journal.assert_raw(&case.root, case.expected_entry_state());' "$$fixture"; \
		grep -Fq 'case.scenario.arm_initial_bound_delete_kill(kill_after_zero_effects)' "$$fixture"; \
		grep -Fq 'case.scenario.arm_recovery_kill(kill_after_zero_effects);' "$$fixture"; \
		grep -Fq 'final_revalidation(kill_after_zero_effects);' "$$fixture"; \
		grep -Fq 'fn kill_after_zero_effects() {' "$$fixture"; \
		awk '$$0 == "fn kill_after_zero_effects() {" { callback = 1; next } callback && /assert_zero_effects\(\);/ { asserted = NR } callback && /kill_self\(\);/ { if (!asserted || NR <= asserted) exit 1; killed = 1 } END { exit !(callback && asserted && killed) }' "$$fixture"; \
		test "$$( grep -Fc 'CleanSystemStartup::enter(system, &reservation)' "$$fixture" )" = 4; \
		test "$$( grep -Fo '::enter(' "$$fixture" | wc -l )" = 4; \
		! grep -Eq '(finalize_usr_rollback|persist_usr_rollback_[[:alnum:]_]+_and_reopen|usr_rollback_(new_state|activate_archived|active_reblit)::dispatch|support::enter_)' "$$fixture"; \
		grep -Fq 'clean endpoint did not remain clean' "$$fixture"; \
	done; \
	for process_file in "$$shared" "$$new_state" "$$archived" "$$active"; do \
		! grep -Fq 'CandidateSource::ALL' "$$process_file"; \
		! grep -Fq 'THROUGH_ROLLBACK_COMPLETE' "$$process_file"; \
		! grep -Eq '(finalize_usr|capture_finalization_ready)' "$$process_file"; \
		! grep -Eq '(arm_[[:alnum:]_]*fault|assert_[[:alnum:]_]*fault_consumed)' "$$process_file"; \
	done; \
	grep -Fq 'assert_eq!(record.generation, 18);' "$$new_state"; \
	grep -Fq 'assert_eq!(retained_exchange_syscall_count(), 0);' "$$new_state"; \
	grep -Fq 'assert_eq!(boot_synchronize_attempt_count(), 0);' "$$new_state"; \
	grep -Fq 'assert_eq!(active_reblit_candidate_preserve_exchange_attempt_count(), 0);' "$$new_state"; \
	grep -Fq 'assert_eq!(effects.fresh_removal, 0);' "$$new_state"; \
	grep -Fq 'assert_eq!(record.generation, 12);' "$$archived"; \
	grep -Fq 'assert_eq!(candidate_move_count(), 0);' "$$archived"; \
	grep -Fq 'assert_eq!(retained_exchange_syscall_count(), 0);' "$$archived"; \
	grep -Fq 'assert_eq!(boot_synchronize_attempt_count(), 0);' "$$archived"; \
	grep -Fq 'assert_eq!(fresh_db_invalidation_removal_call_count(), 0);' "$$archived"; \
	grep -Fq 'assert_eq!(active_reblit_candidate_preserve_exchange_attempt_count(), 0);' "$$archived"; \
	grep -Fq 'assert_eq!(record.generation, 14);' "$$active"; \
	grep -Fq 'assert_complete_route_journal_only();' "$$active"; \
	grep -Fq 'assert_eq!(fresh_db_invalidation_removal_call_count(), 0);' "$$active"; \
	grep -Fq 'wrapper-index={}' "$$active"; \
	grep -Fq 'assert_eq!(rollback.boot, BootRollback::NotRequired);' "$$active"; \
	legacy_new_state=crates/forge/src/client/startup_gate/usr_rollback_new_state/tests/terminal_delete_process_kill.rs; \
	legacy_archived=crates/forge/src/client/startup_gate/usr_rollback_activate_archived/tests/finalization_process_kill.rs; \
	legacy_active=crates/forge/src/client/startup_gate/usr_rollback_active_reblit/tests/finalization_process_kill.rs; \
	for legacy in "$$legacy_new_state" "$$legacy_archived" "$$legacy_active"; do \
		grep -Fq 'for source in CandidateSource::ALL {' "$$legacy"; \
		grep -Fq 'cases, 12,' "$$legacy"; \
		grep -Fq 'RootLinksComplete is outside the later process-kill source axis' "$$legacy"; \
	done; \
	for file in "$$shared" "$$new_state" "$$archived" "$$active" "$$new_state_mod" "$$archived_mod" "$$active_mod" misc/make/startup-root-links-terminal-process-tests.mk; do \
		test "$$( wc -l < "$$file" )" -le 1000; \
	done; \
	$(CARGO) test -p forge --lib root_links_terminal_process_kill -- --test-threads=1 2>&1 | tee "$$executed"; \
	observed_child_count="$$( grep -Fc 'running 1 test' "$$executed" )"; \
	observed_final_recovery_count="$$( grep -Fc 'test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured;' "$$executed" )"; \
	observed_death_count=$$(( observed_child_count - observed_final_recovery_count )); \
	test "$$observed_child_count" = "$$child_count"; \
	test "$$observed_final_recovery_count" = "$$final_recovery_count"; \
	test "$$observed_death_count" = "$$death_count"; \
	test "$$( grep -Fc 'test result: ok. 3 passed; 0 failed; 0 ignored; 0 measured;' "$$executed" )" = 1
