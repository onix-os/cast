SHELL := /bin/bash

GPT_PARTITION_DEVICE_TOP_DIR := $(abspath $(dir $(lastword $(MAKEFILE_LIST)))/../..)
CARGO ?= cargo

.PHONY: forge-linux-gpt-partition-device-test

forge-linux-gpt-partition-device-test: host-storage-safety-test
	@set -euo pipefail; \
	timeout 10s mkdir -p "$(GPT_PARTITION_DEVICE_TOP_DIR)/target"; \
	listed="$$( timeout 10s mktemp "$(GPT_PARTITION_DEVICE_TOP_DIR)/target/linux-gpt-partition-device-test-list.XXXXXXXXXXXX" )"; \
	trap 'timeout 10s rm -f "$$listed"' EXIT; \
	timeout 300s $(CARGO) test --manifest-path "$(GPT_PARTITION_DEVICE_TOP_DIR)/Cargo.toml" -p forge --lib -- --list | timeout 300s tee "$$listed" >/dev/null; \
	timeout 10s grep -q . "$$listed"; \
	prefix='linux_fs::tests::gpt_partition_device::'; \
	timeout 10s test "$$( timeout 10s grep -Ec "^$$prefix.*: test$$" "$$listed" )" = 35; \
	for name in \
		stable::stable_read_only_parent_retains_only_exact_closed_scalars \
		stable::four_kib_logical_blocks_reconcile_with_fixed_512_sector_sysfs_units \
		rejection::every_opening_closing_identity_or_geometry_change_is_rejected \
		rejection::non_block_and_write_capable_descriptors_fail_before_second_observation \
		rejection::zero_containing_device_inode_mount_id_and_length_are_not_admitted_as_identity \
		rejection::parent_rdev_must_match_the_authenticated_sysfs_parent_exactly \
		rejection::gpt_uuid_and_partition_number_are_rechecked \
		rejection::gpt_image_size_and_logical_block_size_are_bound_to_the_observation \
		rejection::observer_errors_propagate_without_retry_or_extra_calls \
		geometry::logical_block_size_and_device_length_are_strictly_bounded_and_aligned \
		geometry::sysfs_and_gpt_start_or_size_disagreement_is_rejected \
		geometry::every_sector_to_byte_multiplication_overflow_fails_closed \
		geometry::partition_range_must_fit_without_overflow_inside_parent_length \
		bounds::observation_limit_admits_exact_n_and_rejects_n_minus_one \
		bounds::work_limit_admits_exact_n_and_rejects_n_minus_one \
		bounds::zero_and_above_production_limits_fail_before_any_observation \
		bounds::expired_initial_deadline_fails_before_any_observation \
		bounds::deadline_expiring_after_opening_observation_prevents_further_work \
		bounds::deadline_equality_is_admitted_by_the_injected_clock \
		live::exact_64_bit_linux_block_ioctl_numbers_are_sealed \
		live::injected_one_shot_observation_returns_only_exact_closed_scalars \
		live::write_capable_and_path_only_descriptors_stop_before_block_queries \
		live::ordinary_file_is_rejected_without_any_storage_discovery_or_block_query \
		live::every_injected_syscall_error_propagates_after_exactly_one_attempt \
		live::deadlines_are_checked_before_work_and_after_each_kernel_call \
		live::positional_reads_are_capped_and_never_cross_authenticated_length \
		live::image_rejects_zero_unaddressable_and_expired_authority \
		live_authentication::live_coordinator_orders_rebind_and_three_observations_around_exact_gpt_passes \
		live_authentication::parent_name_rebind_failure_prevents_interpass_observation_and_pass_two \
		live_authentication::wrong_sysfs_parent_is_rejected_before_any_gpt_source_read \
		live_authentication::interpass_observation_error_propagates_without_pass_two_or_retry \
		live_authentication::interpass_identity_or_geometry_drift_prevents_every_pass_two_read \
		live_authentication::closing_drift_is_rejected_without_any_hidden_reconciliation_observation \
		live_authentication::interpass_timeout_propagates_and_prevents_pass_two_and_closing_work \
		live_authentication::expired_initial_deadline_fails_before_observation_rebind_or_read; do \
		timeout 10s grep -Fqx "$$prefix$$name: test" "$$listed"; \
	done; \
	module="$(GPT_PARTITION_DEVICE_TOP_DIR)/crates/forge/src/linux_fs/gpt_partition_device.rs"; \
	module_dir="$(GPT_PARTITION_DEVICE_TOP_DIR)/crates/forge/src/linux_fs/gpt_partition_device"; \
	live_dir="$$module_dir/live"; \
	tests="$(GPT_PARTITION_DEVICE_TOP_DIR)/crates/forge/src/linux_fs/tests/gpt_partition_device.rs"; \
	tests_dir="$(GPT_PARTITION_DEVICE_TOP_DIR)/crates/forge/src/linux_fs/tests/gpt_partition_device"; \
	timeout 10s grep -Fqx 'pub(crate) mod gpt_partition_device;' "$(GPT_PARTITION_DEVICE_TOP_DIR)/crates/forge/src/linux_fs.rs"; \
	timeout 10s grep -Fqx 'mod gpt_partition_device;' "$(GPT_PARTITION_DEVICE_TOP_DIR)/crates/forge/src/linux_fs/tests.rs"; \
	for part in budget geometry input live observation stable; do timeout 10s grep -Fqx "mod $$part;" "$$module"; done; \
	for part in bounds geometry live live_authentication rejection stable support; do timeout 10s grep -Fqx "mod $$part;" "$$tests"; done; \
	for part in abi authentication image observation syscalls; do timeout 10s grep -Fqx "mod $$part;" "$$module_dir/live.rs"; done; \
	timeout 10s grep -Fq 'pub(in crate::linux_fs) struct ReconciledGptPartitionDeviceEvidence {' "$$module"; \
	timeout 10s grep -Fq 'pub(in crate::linux_fs) fn reconcile_gpt_partition_device_evidence_until(' "$$module"; \
	timeout 10s grep -Fq 'pub(in crate::linux_fs) trait BlockDeviceObserver {' "$$module_dir/observation.rs"; \
	timeout 10s grep -Fq 'pub(in crate::linux_fs) struct BlockDeviceObservation {' "$$module_dir/observation.rs"; \
	timeout 10s grep -Fq 'pub(super) const MAX_OBSERVATION_CALLS: usize = 2;' "$$module_dir/budget.rs"; \
	timeout 10s grep -Fq 'pub(super) const MAX_WORK_UNITS: usize = 45;' "$$module_dir/budget.rs"; \
	timeout 10s grep -Fq 'const SYSFS_SECTOR_BYTES: u64 = 512;' "$$module_dir/geometry.rs"; \
	timeout 10s grep -Fq 'if opening != closing {' "$$module_dir/stable.rs"; \
	timeout 10s grep -Fq 'pub(in crate::linux_fs) struct RetainedBlockDeviceObserver' "$$live_dir/observation.rs"; \
	timeout 10s grep -Fq 'pub(in crate::linux_fs) struct LiveAuthenticatedGptPartitionDeviceEvidence {' "$$live_dir/authentication.rs"; \
	timeout 10s grep -Fq 'pub(in crate::linux_fs) fn authenticate_retained_gpt_partition_device_with_interpass_until(' "$$live_dir/authentication.rs"; \
	timeout 10s grep -Fq 'let observed = observer.observe_until(received_deadline)?;' "$$live_dir/authentication.rs"; \
	timeout 10s grep -Fq 'stable::reconcile_observations_until(opening, closing, expected, &validated, deadline)?;' "$$live_dir/authentication.rs"; \
	timeout 10s grep -Fq 'impl GptPartitionRoleImage for RetainedReadOnlyBlockImage' "$$live_dir/image.rs"; \
	timeout 10s grep -Fq 'const MAX_POSITIONAL_READ_BYTES: usize = 64 * 1024;' "$$live_dir/image.rs"; \
	timeout 10s grep -Fq 'pub(super) const BLKSSZGET_REQUEST: nix::libc::c_ulong = 0x0000_1268;' "$$live_dir/abi.rs"; \
	timeout 10s grep -Fq 'pub(super) const BLKGETSIZE64_REQUEST: nix::libc::c_ulong = 0x8008_1272;' "$$live_dir/abi.rs"; \
	if timeout 10s rg -n 'BLKGETDISKSEQ|retry_interrupted|pwrite|write_all|OpenOptions|PathBuf|Command::new|std::process|/(?:dev|proc|sys)/' "$$module_dir/live.rs" "$$live_dir"/*.rs; then exit 1; else status="$$?"; timeout 10s test "$$status" = 1; fi; \
	if timeout 10s rg -n '\bunsafe\b' "$$module_dir/live.rs" "$$live_dir/abi.rs" "$$live_dir/authentication.rs" "$$live_dir/image.rs" "$$live_dir/observation.rs"; then exit 1; else status="$$?"; timeout 10s test "$$status" = 1; fi; \
	witness_decl="$$( timeout 10s sed -n '/pub(in crate::linux_fs) struct ReconciledGptPartitionDeviceEvidence {/,/^}/p' "$$module" )"; \
	timeout 10s test -n "$$witness_decl"; \
	if timeout 10s rg -n '\b(Vec|String|File|Path|Fd|Observer|Observation)\b|descriptor:|path:|image:|buffer:' <<<"$$witness_decl"; then exit 1; else status="$$?"; timeout 10s test "$$status" = 1; fi; \
	live_witness_decl="$$( timeout 10s sed -n '/pub(in crate::linux_fs) struct LiveAuthenticatedGptPartitionDeviceEvidence {/,/^}/p' "$$live_dir/authentication.rs" )"; \
	timeout 10s test -n "$$live_witness_decl"; \
	if timeout 10s rg -n '\b(Vec|String|File|Path|Fd|Observer|Observation)\b|descriptor:|path:|image:|buffer:' <<<"$$live_witness_decl"; then exit 1; else status="$$?"; timeout 10s test "$$status" = 1; fi; \
	shopt -s nullglob; \
	sources=( "$$module" "$$module_dir"/*.rs "$$live_dir"/*.rs "$$tests" "$$tests_dir"/*.rs ); \
	timeout 10s test "$${#sources[@]}" = 20; \
	pure_sources=( "$$module" "$$module_dir/budget.rs" "$$module_dir/geometry.rs" "$$module_dir/input.rs" "$$module_dir/observation.rs" "$$module_dir/stable.rs" "$$tests" "$$tests_dir/bounds.rs" "$$tests_dir/geometry.rs" "$$tests_dir/rejection.rs" "$$tests_dir/stable.rs" "$$tests_dir/support.rs" ); \
	if timeout 10s rg -n 'std::fs|std::os::(?:fd|unix::io)|\b(File|Path|PathBuf|OpenOptions|OwnedFd|RawFd|BorrowedFd|AsFd|AsRawFd)\b|nix::|libc::|unsafe|ioctl[[:space:]]*\(|BLK[A-Z_]+|/(?:dev|proc|sys)(?:/|"|`)|std::process|process::Command|Command::new|mount\(|umount|pwrite|write_all|set_len' "$${pure_sources[@]}"; then exit 1; else status="$$?"; timeout 10s test "$$status" = 1; fi; \
	for file in "$${sources[@]}" "$(GPT_PARTITION_DEVICE_TOP_DIR)/misc/make/linux-gpt-partition-device-tests.mk"; do \
		timeout 10s test "$$( timeout 10s wc -l < "$$file" )" -le 1000; \
	done; \
	timeout 1200s $(CARGO) test --manifest-path "$(GPT_PARTITION_DEVICE_TOP_DIR)/Cargo.toml" -p forge --lib "$$prefix" -- --test-threads=1
