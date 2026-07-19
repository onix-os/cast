SHELL := /bin/bash

AUTH_MOUNTINFO_TOP_DIR := $(abspath $(dir $(lastword $(MAKEFILE_LIST)))/../..)
CARGO ?= cargo

.PHONY: forge-linux-authenticated-mountinfo-snapshot-test

forge-linux-authenticated-mountinfo-snapshot-test: host-storage-safety-test
	@set -euo pipefail; \
	timeout 10s mkdir -p "$(AUTH_MOUNTINFO_TOP_DIR)/target"; \
	listed="$$( timeout 10s mktemp "$(AUTH_MOUNTINFO_TOP_DIR)/target/linux-authenticated-mountinfo-test-list.XXXXXXXXXXXX" )"; \
	trap 'timeout 10s rm -f "$$listed"' EXIT; \
	timeout 300s $(CARGO) test --manifest-path "$(AUTH_MOUNTINFO_TOP_DIR)/Cargo.toml" -p forge --lib -- --list | timeout 300s tee "$$listed" >/dev/null; \
	timeout 10s grep -q . "$$listed"; \
	prefix='linux_fs::tests::authenticated_mountinfo_snapshot::'; \
	timeout 10s test "$$( timeout 10s grep -Ec "^$$prefix.*: test$$" "$$listed" )" = 17; \
	for name in \
		bounds::zero_limits_and_expired_deadline_fail_before_fixture_hooks \
		bounds::exact_work_and_descriptor_budgets_admit_n_and_reject_n_minus_one \
		bounds::injected_inner_reader_failure_propagates_without_fallback \
		bounds::deadline_expiring_at_terminal_checkpoint_rejects_snapshot \
		malformed::empty_unterminated_and_nul_snapshots_fail_closed \
		malformed::oversized_cursor_snapshot_is_rejected_without_truncation \
		malformed::pure_file_classifier_accepts_only_stable_regular_procfs_identity \
		malformed::pure_file_classifier_rejects_nonproc_wrong_kind_and_zero_identity \
		malformed::pure_file_classifier_rejects_identity_and_terminal_kind_changes \
		races::namespace_and_root_replacements_after_cursor_read_fail_closed \
		races::namespace_and_root_replacements_at_synthetic_file_rebind_fail_closed \
		races::namespace_and_root_replacements_at_both_outer_anchor_edges_fail_closed \
		races::task_tree_replacement_before_closing_anchor_fails_closed \
		stable::stable_cursor_snapshot_retains_exact_bytes_and_parsed_values \
		stable::cursor_fixture_executes_the_exact_inner_checkpoint_schedule \
		stable::production_reader_rejects_a_fixture_anchor_before_live_access \
		stable::unrelated_mount_table_churn_does_not_require_whole_snapshot_equality; do \
		timeout 10s grep -Fqx "$$prefix$$name: test" "$$listed"; \
	done; \
	root="$(AUTH_MOUNTINFO_TOP_DIR)/crates/forge/src/linux_fs/mount_namespace.rs"; \
	module="$(AUTH_MOUNTINFO_TOP_DIR)/crates/forge/src/linux_fs/mount_namespace/mountinfo_snapshot.rs"; \
	core="$(AUTH_MOUNTINFO_TOP_DIR)/crates/forge/src/linux_fs/mount_namespace/mountinfo_snapshot"; \
	tests="$(AUTH_MOUNTINFO_TOP_DIR)/crates/forge/src/linux_fs/tests/authenticated_mountinfo_snapshot"; \
	timeout 10s grep -Fq 'pub(crate) fn read_current_thread_mountinfo_until' "$$module"; \
	timeout 10s grep -Fq 'pub(crate) struct AuthenticatedMountInfoSnapshot' "$$module"; \
	timeout 10s grep -Fq 'PhantomData<Rc<()>>' "$$module"; \
	timeout 10s grep -Fq 'Cursor::new(bytes)' "$$core/capture.rs"; \
	timeout 10s grep -Fq 'c"mountinfo"' "$$core/filesystem.rs"; \
	timeout 10s grep -Fq 'nix::libc::O_NONBLOCK' "$$core/filesystem.rs"; \
	timeout 10s grep -Fq 'nix::libc::O_NOFOLLOW' "$$core/filesystem.rs"; \
	timeout 10s grep -Fq 'controlled_resolution()' "$$core/filesystem.rs"; \
	timeout 10s grep -Fq 'validate_mountinfo_file_authentication' "$$core/filesystem.rs"; \
	timeout 10s grep -Fq 'read_mountinfo_snapshot_bounded_until' "$$core/capture.rs"; \
	timeout 10s grep -Fq 'opening.current.snapshot()' "$$module"; \
	timeout 10s grep -Fq 'closing.current.snapshot()' "$$module"; \
	timeout 10s grep -Fq 'const OPENING_AND_CLOSING_ANCHOR_ALLOWANCES: usize = 2;' "$$module"; \
	timeout 10s grep -Fq 'const EXACT_THREAD_CONTEXT_ALLOWANCES: usize = 1;' "$$module"; \
	timeout 10s grep -Fq 'MOUNTINFO_READ_PARSE_WORK_BOUND' "$$module"; \
	timeout 10s grep -Fq 'authenticate_mountinfo_file(&mountinfo, operation)' "$$core/capture.rs"; \
	timeout 10s grep -Fq 'file_identity, after_read, "mountinfo descriptor around bounded read"' "$$core/capture.rs"; \
	timeout 10s grep -Fq 'open_mountinfo(&exact.thread, operation)' "$$core/capture.rs"; \
	timeout 10s grep -Fq 'operation.checkpoint()?' "$$module"; \
	snapshot_decl="$$( timeout 10s sed -n '/pub(crate) struct AuthenticatedMountInfoSnapshot {/,/^}/p' "$$module" )"; \
	if timeout 10s rg -n 'File|Path|Fd|fd:|descriptor|closure' <<<"$$snapshot_decl"; then exit 1; else status="$$?"; timeout 10s test "$$status" = 1; fi; \
	if timeout 10s rg -n 'impl[[:space:]]+(Clone|Copy|PartialEq|Eq)[[:space:]]+for[[:space:]]+AuthenticatedMountInfoSnapshot' "$$module"; then exit 1; else status="$$?"; timeout 10s test "$$status" = 1; fi; \
	if timeout 10s rg -n 'unsafe[[:space:]]+impl[[:space:]]+(Send|Sync)|pub\(crate\).*(File|OwnedFd|RawFd|AsRawFd|raw_fd|as_raw_fd)|read_dir|canonicalize|std::env|std::process|process::Command|Command::new|OpenOptions|create_dir|(?:std::fs::|fs::)?write\(|write_all|set_len|remove_(file|dir)|rename\(|nix::mount|nix::sched::setns|nix::sched::unshare|nix::unistd::chroot|libc::(?:setns|unshare|chroot|pivot_root|mount|umount2|open_tree|move_mount)' "$$module" "$$core"; then exit 1; else status="$$?"; timeout 10s test "$$status" = 1; fi; \
	test_path_pattern='(?x)[/](?:proc|sys|dev|run)(?:[/]|(?![[:alnum:]_.+-]))|(?<![[:alnum:]_./])[/](?:boot|efi|esp)(?:[/]|(?![[:alnum:]_.+-]))'; \
	if timeout 10s rg --pcre2 -n "$$test_path_pattern" "$$tests" "$(AUTH_MOUNTINFO_TOP_DIR)/crates/forge/src/linux_fs/tests/authenticated_mountinfo_snapshot.rs"; then exit 1; else status="$$?"; timeout 10s test "$$status" = 1; fi; \
	if timeout 10s rg -n 'PreparedMountNamespaceAnchor::prepare|authenticated_current_thread_procfs|descriptor_mount_id|setns|unshare|chroot|pivot_root|open_tree|move_mount|nix::mount|libc::mount|libc::umount2?|read_dir|canonicalize|std::process|process::Command|Command::new|blkid|lsblk|findmnt|udevadm|smartctl|hdparm' "$$tests" "$(AUTH_MOUNTINFO_TOP_DIR)/crates/forge/src/linux_fs/tests/authenticated_mountinfo_snapshot.rs"; then exit 1; else status="$$?"; timeout 10s test "$$status" = 1; fi; \
	for file in \
		"$$root" \
		"$$module" \
		"$$core"/*.rs \
		"$(AUTH_MOUNTINFO_TOP_DIR)/crates/forge/src/linux_fs/tests/authenticated_mountinfo_snapshot.rs" \
		"$$tests"/*.rs \
		"$(AUTH_MOUNTINFO_TOP_DIR)/misc/make/linux-authenticated-mountinfo-snapshot-tests.mk"; do \
		timeout 10s test "$$( timeout 10s wc -l < "$$file" )" -le 1000; \
	done; \
	timeout 1200s $(CARGO) test --manifest-path "$(AUTH_MOUNTINFO_TOP_DIR)/Cargo.toml" -p forge --lib "$$prefix" -- --test-threads=1
