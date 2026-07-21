DISPOSABLE_VM_BOOT_STORAGE_TOP_DIR := $(abspath $(dir $(lastword $(MAKEFILE_LIST)))/../..)
DISPOSABLE_VM_BOOT_STORAGE_SCRIPT := $(DISPOSABLE_VM_BOOT_STORAGE_TOP_DIR)/misc/vm/disposable-uefi-boot-storage-campaign.sh
DISPOSABLE_VM_BOOT_STORAGE_EFFECTS := $(DISPOSABLE_VM_BOOT_STORAGE_TOP_DIR)/misc/vm/disposable-uefi-boot-storage-effects.sh

export VM_EXPECTED_HOSTNAME
export VM_EXPECTED_MACHINE_ID
export VM_EXPECTED_BOOT_ID
export VM_EXPECTED_VIRTUALIZATION
export VM_EXPECTED_COMMIT
export VM_TARGET_DISK
export VM_TARGET_STABLE_PATH
export VM_TARGET_DISKSEQ
export VM_TARGET_DISK_BYTES
export VM_EXPECTED_ROOT_DEVICE
export VM_EXPECTED_LIVE_ESP_DEVICE
export VM_EXPECTED_LIVE_ESP_MOUNTPOINT
export VM_FILESYSTEM_LABEL
export VM_PUBLICATION_PARENT
export VM_SNAPSHOT_CONFIRMATION
export VM_REMOTE_CONFIRMATION
export VM_COOPERATIVE_ROOT_CONFIRMATION
export VM_BOOT_STORAGE_CHALLENGE
export VM_DESTRUCTIVE_CONFIRMATION

.PHONY: disposable-vm-uefi-boot-storage-harness-test \
	disposable-vm-uefi-boot-storage-challenge \
	disposable-vm-uefi-boot-storage-admission \
	disposable-vm-uefi-boot-storage-campaign

define disposable_vm_boot_storage_common_arguments
	--expected-hostname "$${VM_EXPECTED_HOSTNAME-}" \
	--expected-machine-id "$${VM_EXPECTED_MACHINE_ID-}" \
	--expected-boot-id "$${VM_EXPECTED_BOOT_ID-}" \
	--expected-virtualization "$${VM_EXPECTED_VIRTUALIZATION-}" \
	--expected-commit "$${VM_EXPECTED_COMMIT-}" \
	--target-disk "$${VM_TARGET_DISK-}" \
	--target-stable-path "$${VM_TARGET_STABLE_PATH-}" \
	--target-diskseq "$${VM_TARGET_DISKSEQ-}" \
	--target-bytes "$${VM_TARGET_DISK_BYTES-}" \
	--expected-root-device "$${VM_EXPECTED_ROOT_DEVICE-}" \
	--expected-live-esp-device "$${VM_EXPECTED_LIVE_ESP_DEVICE-}" \
	--expected-live-esp-mountpoint "$${VM_EXPECTED_LIVE_ESP_MOUNTPOINT-}" \
	--filesystem-label "$${VM_FILESYSTEM_LABEL-}" \
	--publication-parent "$${VM_PUBLICATION_PARENT-}" \
	--snapshot-confirmation "$${VM_SNAPSHOT_CONFIRMATION-}" \
	--remote-confirmation "$${VM_REMOTE_CONFIRMATION-}" \
	--cooperative-root-confirmation "$${VM_COOPERATIVE_ROOT_CONFIRMATION-}"
endef

disposable-vm-uefi-boot-storage-harness-test: forge-linux-descriptor-boot-file-publication-cargo-filter-test
	@set -eu; \
	script="$(DISPOSABLE_VM_BOOT_STORAGE_SCRIPT)"; \
	effects="$(DISPOSABLE_VM_BOOT_STORAGE_EFFECTS)"; \
	publisher_tests="$(DISPOSABLE_VM_BOOT_STORAGE_TOP_DIR)/crates/forge/src/linux_fs/tests/descriptor_boot_file_publication.rs"; \
	publisher_make="$(DISPOSABLE_VM_BOOT_STORAGE_TOP_DIR)/misc/make/linux-descriptor-boot-file-publication-tests.mk"; \
	test -f "$$script"; \
	test ! -L "$$script"; \
	test -f "$$effects"; \
	test ! -L "$$effects"; \
	test -f "$$publisher_tests"; \
	test -f "$$publisher_make"; \
	sh -n "$$script"; \
	sh -n "$$effects"; \
	test "$$(wc -l <"$$script")" -le 1000; \
	test "$$(wc -l <"$$effects")" -le 1000; \
	mkdir -p "$(DISPOSABLE_VM_BOOT_STORAGE_TOP_DIR)/target"; \
	output="$$(mktemp "$(DISPOSABLE_VM_BOOT_STORAGE_TOP_DIR)/target/disposable-vm-boot-storage-usage.XXXXXXXXXXXX")"; \
	trap 'rm -f -- "$$output"' EXIT; \
	status=0; \
	sh "$$script" >"$$output" 2>&1 || status="$$?"; \
	test "$$status" = 2; \
	grep -Fq 'usage: disposable-uefi-boot-storage-campaign.sh' "$$output"; \
	grep -Fq 'runtime_root=/run/cast-vm-boot-storage' "$$script"; \
	grep -Fq 'verify_guest_identity' "$$script"; \
	grep -Fq 'verify_target_disk' "$$script"; \
	grep -Fq 'verify_marker' "$$script"; \
	grep -Fq 'verify_init_mount_namespace' "$$script"; \
	grep -Fq 'PATH=/usr/sbin:/usr/bin:/sbin:/bin' "$$script"; \
	grep -Fq 'command_owner' "$$script"; \
	grep -Fq 'physical_lines=' "$$script"; \
	test "$$(grep -Fc '[ "$$BLOCK_DEVNUM" != "$$target_devnum" ]' "$$script")" = 1; \
	test "$$(grep -Fc '[ "$$BLOCK_DISK_DEVNUM" != "$$target_disk_devnum" ]' "$$script")" = 1; \
	grep -Fq 'run_bounded 120s "$$mkfs_command"' "$$effects"; \
	test "$$(grep -Fc -- '--mbr=n' "$$effects")" = 1; \
	grep -Fq '"$$mount_command" -i -t vfat' "$$effects"; \
	grep -Fq '"$$umount_command" -i --' "$$effects"; \
	grep -Fq 'uid=0 | gid=0) ;;' "$$effects"; \
	grep -Fq 'uid=* | gid=*) return 1 ;;' "$$effects"; \
	grep -Fq "mount_root_metadata=\$$(stat -Lc '%u:%g:%F' -- \"\$$mount_root\")" "$$effects"; \
	grep -Fq "[ \"\$$mount_root_metadata\" = '0:0:directory' ]" "$$effects"; \
	grep -Fq 'destructive_started=1' "$$effects"; \
	grep -Fq 'campaign_complete=1' "$$effects"; \
	grep -Fq 'mount_is_exact_identity' "$$effects"; \
	grep -Fq 'nix_command=$$(command_path nix)' "$$effects"; \
	grep -Fq 'make_command=$$(command_path make)' "$$effects"; \
	grep -Fq 'env_command=$$(command_path env)' "$$effects"; \
	grep -Fq 'rm_command=$$(command_path rm)' "$$effects"; \
	grep -Fq 'jq_command=$$(command_path jq)' "$$effects"; \
	grep -Fq 'readlink_command=$$(command_path readlink)' "$$effects"; \
	grep -Fq 'stat_command=$$(command_path stat)' "$$effects"; \
	grep -Fq 'git_command=$$(command_path git)' "$$effects"; \
	grep -Fq 'tar_command=$$(command_path tar)' "$$effects"; \
	grep -Fq 'publication_build_parent=/var/tmp' "$$effects"; \
	grep -Fq 'publication_build_root=$$publication_build_parent/cast-vm-boot-storage-$$expected_boot_id-$$challenge' "$$effects"; \
	grep -Fq 'publication_develop_profile=$$publication_build_root/nix-develop-profile' "$$effects"; \
	grep -Fq 'publication_binary_manifest=$$publication_build_root/forge-libtest-manifest-v1' "$$effects"; \
	grep -Fq 'publication_source_archive=$$publication_build_root/source-$$expected_commit.tar' "$$effects"; \
	grep -Fq 'publication_staged_source=$$publication_build_root/source' "$$effects"; \
	grep -Fq 'fresh private publication build root already exists' "$$effects"; \
	grep -Fq '"$$mkdir_command" -m 700 -- "$$publication_staged_source"' "$$effects"; \
	test "$$(grep -Fc 'GIT_NO_REPLACE_OBJECTS=1 "$$git_command"' "$$effects")" = 3; \
	test "$$(grep -Fc 'archive --format=tar' "$$effects")" = 1; \
	grep -Fq -- '--output="$$publication_source_archive" "$$expected_commit"' "$$effects"; \
	grep -Fq '"$$tar_command" --extract --file "$$publication_source_archive"' "$$effects"; \
	grep -Fq -- '--directory "$$publication_staged_source" --no-same-owner' "$$effects"; \
	grep -Fq 'staged publication source unexpectedly contains .git' "$$effects"; \
	grep -Fq 'staged publication source unexpectedly contains top-level target' "$$effects"; \
	grep -Fq '"$$env_command" -i' "$$effects"; \
	grep -Fq '"CARGO_TARGET_DIR=$$publication_test_target"' "$$effects"; \
	grep -Fq '"CARGO_HOME=$$publication_cargo_home"' "$$effects"; \
	grep -Fq '"CAST_VM_BOOT_PUBLICATION_BUILD_ROOT=$$publication_build_root"' "$$effects"; \
	grep -Fq 'flake metadata --json --no-write-lock-file --no-update-lock-file' "$$effects"; \
	grep -Fq '"path:$$publication_staged_source" >"$$publication_flake_metadata"' "$$effects"; \
	grep -Fq '.resolved.type == "path"' "$$effects"; \
	grep -Fq '.locked.type == "path"' "$$effects"; \
	grep -Fq '.resolved.path == $$staged_source' "$$effects"; \
	grep -Fq '.locked.path == $$staged_source' "$$effects"; \
	grep -Fq '.locked.narHash | type == "string"' "$$effects"; \
	grep -Fq '((.revision? // null) == null)' "$$effects"; \
	grep -Fq 'develop --profile "$$publication_develop_profile"' "$$effects"; \
	grep -Fq '"path:$$publication_source_root" --command' "$$effects"; \
	grep -Fq 'profile_type=$$profile_remainder' "$$effects"; \
	grep -Fq "profile_type\" = 'regular file' ]" "$$effects"; \
	grep -Fq '"$$make_command" -C "$$publication_source_root"' "$$effects"; \
	test "$$(grep -Fc 'forge-linux-descriptor-boot-file-publication-vfat-build' "$$effects")" = 1; \
	test "$$(grep -Fc 'run_boot_file_publication_test publish' "$$effects")" = 1; \
	test "$$(grep -Fc 'run_boot_file_publication_test revalidate' "$$effects")" = 1; \
	grep -Fq 'publisher test lost the exact admitted VFAT mount policy before invocation' "$$effects"; \
	grep -Fq 'publisher test lost the exact admitted VFAT mount policy after invocation' "$$effects"; \
	grep -Fq 'forge-linux-descriptor-boot-file-publication-vfat-test' "$$effects"; \
	grep -Fq '#[ignore = "requires the guarded disposable-VM VFAT campaign"]' "$$publisher_tests"; \
	grep -Fq 'DISPOSABLE_VM_PARENT_PREFIX: &str = "/run/cast-vm-boot-storage/mount/"' "$$publisher_tests"; \
	grep -Fq 'assert_disposable_vm_identity_and_marker(publication_parent)' "$$publisher_tests"; \
	grep -Fq 'assert_disposable_vm_mount_policy(&expected_target_devnum)' "$$publisher_tests"; \
	grep -Fq 'forge-linux-descriptor-boot-file-publication-vfat-build:' "$$publisher_make"; \
	grep -Fq 'forge-linux-descriptor-boot-file-publication-vfat-test:' "$$publisher_make"; \
	grep -Fq 'forge-linux-descriptor-boot-file-publication-cargo-filter-test:' "$$publisher_make"; \
	grep -Fq "'\$$(DESCRIPTOR_BOOT_FILE_PUBLICATION_CARGO_ARTIFACT_FILTER)'" "$$publisher_make"; \
	test "$$(grep -Fc "'\$$(DESCRIPTOR_BOOT_FILE_PUBLICATION_CARGO_ARTIFACT_FILTER)'" "$$publisher_make")" = 2; \
	test "$$(grep -Fc "trusted_tools='/usr/bin/id /usr/bin/stat /usr/bin/cat /usr/bin/systemd-detect-virt" "$$publisher_make")" = 2; \
	grep -Fq '/usr/bin/systemd-detect-virt --vm' "$$publisher_make"; \
	grep -Fq "'0:0:600:regular file:1'" "$$publisher_make"; \
	grep -Fq 'expected_build_root="/var/tmp/cast-vm-boot-storage-' "$$publisher_make"; \
	grep -Fq 'forge-libtest-manifest-v1' "$$publisher_make"; \
	test "$$(grep -Fc "profile_type\" = 'regular file'" "$$publisher_make")" = 2; \
	grep -Fq -- '--lib --no-run --message-format=json' "$$publisher_make"; \
	grep -Fq '"$$$$executable" "$$$$test_name" --ignored --exact --test-threads=1' "$$publisher_make"; \
	grep -Fq 'done </proc/self/mountinfo' "$$publisher_make"; \
	grep -Eq '^[[:space:]]*cd /; \\$$' "$$publisher_make"; \
	build_recipe="$$(sed -n '/^forge-linux-descriptor-boot-file-publication-vfat-build:/,/^forge-linux-descriptor-boot-file-publication-vfat-test:/p' "$$publisher_make" | sed '$$d')"; \
	run_recipe="$$(sed -n '/^forge-linux-descriptor-boot-file-publication-vfat-test:/,$$p' "$$publisher_make")"; \
	test "$$(grep -Fc '$$(CARGO) test --locked' <<<"$$build_recipe")" = 1; \
	test "$$(grep -Fc -- '--no-run --message-format=json' <<<"$$build_recipe")" = 1; \
	test "$$(grep -Fc '"$$$$executable" "$$$$test_name" --ignored --exact --test-threads=1' <<<"$$run_recipe")" = 1; \
	if rg -n '\$\(CARGO\)|cargo test|nix develop|nix_command' <<<"$$run_recipe"; then exit 1; else status="$$?"; test "$$status" = 1; fi; \
	if grep -Fq 'target_devnum=$$target_devnum' "$$script"; then exit 1; fi; \
	if grep -Fq '$$root/target' "$$effects"; then exit 1; fi; \
	if grep -Fq 'path:$$root' "$$effects"; then exit 1; fi; \
	if rg -n '\.revision ==|\.locked\.rev ==' "$$effects"; then exit 1; else status="$$?"; test "$$status" = 1; fi; \
	test "$$(grep -Fc 'flake metadata --json --no-write-lock-file --no-update-lock-file' "$$effects")" = 1; \
	test "$$(grep -Fc '"path:$$publication_staged_source"' "$$effects")" = 1; \
	test "$$(grep -Fc 'develop --profile "$$publication_develop_profile"' "$$effects")" = 1; \
	test "$$(grep -Fc '"path:$$publication_source_root" --command' "$$effects")" = 1; \
	prepare_runner="$$(sed -n '/^prepare_boot_file_publication_runner() {/,/^}/p' "$$effects")"; \
	resolve_line="$$(grep -nF '    resolve_immutable_publication_source' <<<"$$prepare_runner" | cut -d: -f1)"; \
	develop_line="$$(grep -nF '        develop --profile "$$publication_develop_profile"' <<<"$$prepare_runner" | cut -d: -f1)"; \
	build_line="$$(grep -nF '        forge-linux-descriptor-boot-file-publication-vfat-build' <<<"$$prepare_runner" | cut -d: -f1)"; \
	test "$$(grep -Fc '    resolve_immutable_publication_source' <<<"$$prepare_runner")" = 1; \
	test "$$resolve_line" -lt "$$develop_line"; \
	test "$$develop_line" -lt "$$build_line"; \
	if rg -n 'run_bounded.*(nix_command|make_command)' "$$effects"; then exit 1; else status="$$?"; test "$$status" = 1; fi; \
	test "$$(grep -Fc 'verify_init_mount_namespace' "$$effects")" -ge 4; \
	run_campaign="$$(sed -n '/^run_campaign() {/,/^}/p' "$$effects")"; \
	prepare_line="$$(grep -nF '    prepare_boot_file_publication_runner' <<<"$$run_campaign" | cut -d: -f1)"; \
	last_guest_line="$$(grep -nF '    verify_guest_identity' <<<"$$run_campaign" | tail -n 1 | cut -d: -f1)"; \
	last_disk_line="$$(grep -nF '    verify_target_disk' <<<"$$run_campaign" | sed -n '2p' | cut -d: -f1)"; \
	marker_line="$$(grep -nF '    verify_marker "$$consumed_marker"' <<<"$$run_campaign" | tail -n 1 | cut -d: -f1)"; \
	started_line="$$(grep -nF '    destructive_started=1' <<<"$$run_campaign" | cut -d: -f1)"; \
	mkfs_line="$$(grep -nF '    run_bounded 120s "$$mkfs_command"' <<<"$$run_campaign" | cut -d: -f1)"; \
	publish_line="$$(grep -nF '    run_boot_file_publication_test publish' <<<"$$run_campaign" | cut -d: -f1)"; \
	revalidate_line="$$(grep -nF '    run_boot_file_publication_test revalidate' <<<"$$run_campaign" | cut -d: -f1)"; \
	complete_line="$$(grep -nF '    campaign_complete=1' <<<"$$run_campaign" | cut -d: -f1)"; \
	test "$$(grep -Fc '    prepare_boot_file_publication_runner' <<<"$$run_campaign")" = 1; \
	test "$$prepare_line" -lt "$$last_guest_line"; \
	test "$$last_guest_line" -lt "$$last_disk_line"; \
	test "$$last_disk_line" -lt "$$marker_line"; \
	test "$$marker_line" -lt "$$started_line"; \
	test "$$started_line" -lt "$$mkfs_line"; \
	test "$$mkfs_line" -lt "$$publish_line"; \
	test "$$publish_line" -lt "$$revalidate_line"; \
	test "$$revalidate_line" -lt "$$complete_line"; \
	lock_cleanup_line="$$(grep -nF 'rmdir -- "$$campaign_lock"' "$$effects" | cut -d: -f1)"; \
	build_cleanup_line="$$(grep -nF '"$$rm_command" -rf --one-file-system -- "$$publication_build_root"' "$$effects" | cut -d: -f1)"; \
	marker_cleanup_line="$$(grep -nF 'rm -f -- "$$consumed_marker"' "$$effects" | cut -d: -f1)"; \
	test "$$build_cleanup_line" -lt "$$lock_cleanup_line"; \
	test "$$lock_cleanup_line" -lt "$$marker_cleanup_line"; \
	grep -Fq 'No reboot was requested or performed.' "$$effects"; \
	if grep -Fq -- '--foreground' "$$effects"; then \
		echo 'destructive child bounds must own their process group' >&2; \
		exit 1; \
	fi; \
	if grep -Eq '^[[:space:]]*(ssh|virsh|reboot|shutdown|poweroff)([[:space:]]|$$)' "$$script" "$$effects"; then \
		echo 'VM harness must not orchestrate SSH, a hypervisor, or reboot' >&2; \
		exit 1; \
	fi; \
	echo 'Disposable VM UEFI boot-storage harness static checks passed.'

disposable-vm-uefi-boot-storage-challenge:
	@"$(DISPOSABLE_VM_BOOT_STORAGE_SCRIPT)" challenge \
	$(disposable_vm_boot_storage_common_arguments)

disposable-vm-uefi-boot-storage-admission:
	@"$(DISPOSABLE_VM_BOOT_STORAGE_SCRIPT)" admit \
	$(disposable_vm_boot_storage_common_arguments) \
	--challenge "$${VM_BOOT_STORAGE_CHALLENGE-}"

disposable-vm-uefi-boot-storage-campaign:
	@"$(DISPOSABLE_VM_BOOT_STORAGE_SCRIPT)" campaign \
	$(disposable_vm_boot_storage_common_arguments) \
	--challenge "$${VM_BOOT_STORAGE_CHALLENGE-}" \
	--destructive-confirmation "$${VM_DESTRUCTIVE_CONFIRMATION-}"
