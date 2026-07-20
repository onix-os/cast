.PHONY: forge-transition-root-abi-publication-test

forge-transition-root-abi-publication-test:
	@set -eu; \
	listed="$$( timeout 300s $(CARGO) test -p forge --lib -- --list )"; \
	timeout 10s grep -q . <<<"$$listed"; \
	count="$$( timeout 10s grep -c '^transition_identity::journal_coordinator::tests::journal_coordinator_root_links_complete_.*: test$$' <<<"$$listed" )"; \
	timeout 10s test "$$count" = 15; \
	for test in \
		journal_coordinator_root_links_complete_publishes_all_initial_link_subsets_for_every_operation \
		journal_coordinator_root_links_complete_preserves_synthesized_empty_and_active_reblit_reservations \
		journal_coordinator_root_links_complete_authenticates_exact_eexist_at_every_publisher_index \
		journal_coordinator_root_links_complete_rejects_foreign_eexist_at_every_publisher_index_without_replacement \
		journal_coordinator_root_links_complete_rejects_existing_and_new_exact_target_inode_aba \
		journal_coordinator_root_links_complete_new_state_post_publication_database_and_provenance_races_fail_stop \
		journal_coordinator_root_links_complete_archived_post_publication_database_and_metadata_races_fail_stop \
		journal_coordinator_root_links_complete_active_reblit_post_publication_state_and_reservation_races_fail_stop \
		journal_coordinator_root_links_complete_retained_namespace_binding_races_fail_stop \
		journal_coordinator_root_links_complete_same_byte_canonical_journal_inode_replacement_fails_stop \
		journal_coordinator_root_links_complete_root_directory_sync_failure_keeps_usr_exchanged \
		journal_coordinator_root_links_complete_journal_persistence_faults_expose_only_source_or_successor \
		journal_coordinator_root_links_complete_restart_composes_from_exact_source_and_successor \
		journal_coordinator_root_links_complete_failures_release_journal_and_writer_authorities_while_error_lives \
		journal_coordinator_root_links_complete_success_revalidates_exact_successor_binding_without_replaying_publication; do \
		timeout 10s grep -Fqx "transition_identity::journal_coordinator::tests::$$test: test" <<<"$$listed"; \
	done; \
	contract="crates/forge/src/transition_identity/journal_coordinator/root_abi_publication.rs"; \
	authority="crates/forge/src/client/journal_usr_exchange_authority.rs"; \
	coordinator="crates/forge/src/transition_identity/journal_coordinator/mod.rs"; \
	support="crates/forge/src/transition_identity/journal_coordinator/tests/root_abi_publication_support.rs"; \
	success="crates/forge/src/transition_identity/journal_coordinator/tests/root_abi_publication_success.rs"; \
	persistence="crates/forge/src/transition_identity/journal_coordinator/tests/root_abi_publication_persistence.rs"; \
	timeout 10s grep -Fqx 'mod root_abi_publication;' "$$coordinator"; \
	if timeout 10s grep -Fqx 'pub(crate) mod root_abi_publication;' "$$coordinator"; then \
		timeout 10s printf '%s\n' 'root ABI publication module visibility widened' >&2; exit 1; \
	else \
		status="$$?"; timeout 10s test "$$status" = 1; \
	fi; \
	timeout 10s grep -Fq 'pub(super) fn publish_root_abi(self)' "$$contract"; \
	if timeout 10s grep -Fq 'pub(crate) fn publish_root_abi(self)' "$$contract"; then \
		timeout 10s printf '%s\n' 'unwired RootLinksComplete transition became crate-wide' >&2; exit 1; \
	else \
		status="$$?"; timeout 10s test "$$status" = 1; \
	fi; \
	for field in \
		'    pub(super) coordinator: StatefulTransitionCoordinator,' \
		'    pub(super) metadata: CandidateMetadataProof,' \
		'    pub(super) provenance: db::state::MetadataProvenance,' \
		'    pub(super) authority: PublishedJournalRootAbiAuthority,' \
		'    pub(super) readiness: UsrExchangeReadiness,' \
		'    pub(super) record_binding: TransitionJournalRecordBinding,'; do \
		timeout 10s grep -Fqx "$$field" "$$contract"; \
	done; \
	if timeout 10s grep -nE 'Option<(StatefulTransitionCoordinator|CandidateMetadataProof|MetadataProvenance|PublishedJournalRootAbiAuthority|UsrExchangeReadiness|TransitionJournalRecordBinding)>' "$$contract"; then \
		timeout 10s printf '%s\n' 'RootLinksComplete made mandatory authority optional' >&2; exit 1; \
	else \
		status="$$?"; timeout 10s test "$$status" = 1; \
	fi; \
	timeout 10s test "$$( timeout 10s grep -Fc '.record_binding(cast, &coordinator.record)' "$$contract" )" = 1; \
	timeout 10s test "$$( timeout 10s grep -Fc '.publish_root_abi()' "$$contract" )" = 1; \
	timeout 10s test "$$( timeout 10s grep -Fc '.advance_record_binding(cast, record_binding, &complete)' "$$contract" )" = 1; \
	bind_line="$$( timeout 10s grep -nF '.record_binding(cast, &coordinator.record)' "$$contract" | timeout 10s cut -d: -f1 )"; \
	publish_line="$$( timeout 10s grep -nF '.publish_root_abi()' "$$contract" | timeout 10s cut -d: -f1 )"; \
	post_line="$$( timeout 10s grep -nF 'require_published_root_abi_sandwich(' "$$contract" | timeout 10s head -n 1 | timeout 10s cut -d: -f1 )"; \
	advance_line="$$( timeout 10s grep -nF '.advance_record_binding(cast, record_binding, &complete)' "$$contract" | timeout 10s cut -d: -f1 )"; \
	final_line="$$( timeout 10s grep -nF 'require_root_links_complete_sandwich(' "$$contract" | timeout 10s head -n 1 | timeout 10s cut -d: -f1 )"; \
	timeout 10s test "$$bind_line" -lt "$$publish_line"; \
	timeout 10s test "$$publish_line" -lt "$$post_line"; \
	timeout 10s test "$$post_line" -lt "$$advance_line"; \
	timeout 10s test "$$advance_line" -lt "$$final_line"; \
	for generation in \
		'        Operation::NewState => 10,' \
		'        Operation::ActiveReblit => 8,' \
		'        Operation::ActivateArchived => 6,'; do \
		timeout 10s grep -Fqx "$$generation" "$$contract"; \
	done; \
	if timeout 10s grep -nE 'journal\.(create|advance|delete)\(|renameat2|symlinkat|unlinkat|remove_(file|dir)|^[[:space:]]*(for|while|loop)[[:space:]]' "$$contract"; then \
		timeout 10s printf '%s\n' 'root ABI coordinator acquired an ordinary journal, raw filesystem, or retry primitive' >&2; exit 1; \
	else \
		status="$$?"; timeout 10s test "$$status" = 1; \
	fi; \
	failure_enum="$$( timeout 10s sed -n '/enum RootAbiPublicationFailure/,/^}/p' "$$contract" )"; \
	if timeout 10s grep -E '^[[:space:]]*(coordinator|authority|metadata|provenance|readiness|record_binding):' <<<"$$failure_enum"; then \
		timeout 10s printf '%s\n' 'root ABI publication failure retained reusable authority' >&2; exit 1; \
	fi; \
	timeout 10s grep -Fq 'pub(crate) fn publish_root_abi(self)' "$$authority"; \
	timeout 10s test "$$( timeout 10s grep -Fc 'let root_abi = root_abi.publish()?;' "$$authority" )" = 1; \
	if callsites="$$( timeout 10s rg -n '\.publish_root_abi\(\)' crates/forge/src --glob '*.rs' --glob '!**/journal_coordinator/root_abi_publication.rs' --glob '!**/tests/**' --glob '!**/tests.rs' --glob '!**/*_tests.rs' --glob '!**/*_tests/**' )"; then \
		timeout 10s printf '%s\n' 'RootLinksComplete gained a live callsite before startup dispatch exists:' "$$callsites" >&2; exit 1; \
	else \
		status="$$?"; timeout 10s test "$$status" = 1; \
	fi; \
	timeout 10s grep -Fqx '    ("bin", "usr/bin"),' "$$support"; \
	timeout 10s grep -Fqx '    ("sbin", "usr/sbin"),' "$$support"; \
	timeout 10s grep -Fqx '    ("lib", "usr/lib"),' "$$support"; \
	timeout 10s grep -Fqx '    ("lib32", "usr/lib32"),' "$$support"; \
	timeout 10s grep -Fqx '    ("lib64", "usr/lib"),' "$$support"; \
	timeout 10s grep -Fq 'for mask in 0..32' "$$success"; \
	for fault in temporary_sync update_exchange update_first_directory_sync displaced_unlink update_final_directory_sync; do \
		timeout 10s grep -Fq "arm_next_$${fault}_fault" "$$persistence"; \
	done; \
	timeout 10s grep -Fq 'assert_root_links_complete_restart_persists_rollback_decision(' "$$persistence"; \
	timeout 900s $(CARGO) test -p forge --lib \
		'transition_identity::journal_coordinator::tests::journal_coordinator_root_links_complete_' \
		-- --test-threads=1
