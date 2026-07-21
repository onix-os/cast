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

disposable-vm-uefi-boot-storage-harness-test:
	@set -eu; \
	script="$(DISPOSABLE_VM_BOOT_STORAGE_SCRIPT)"; \
	effects="$(DISPOSABLE_VM_BOOT_STORAGE_EFFECTS)"; \
	test -f "$$script"; \
	test ! -L "$$script"; \
	test -f "$$effects"; \
	test ! -L "$$effects"; \
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
	grep -Fq '"$$mount_command" -i -t vfat' "$$effects"; \
	grep -Fq '"$$umount_command" -i --' "$$effects"; \
	grep -Fq 'destructive_started=1' "$$effects"; \
	grep -Fq 'campaign_complete=1' "$$effects"; \
	grep -Fq 'mount_is_exact_identity' "$$effects"; \
	test "$$(grep -Fc 'verify_init_mount_namespace' "$$effects")" -ge 4; \
	awk '/verify_marker "\$$consumed_marker"/ { marker = NR } /destructive_started=1/ { started = NR } /run_bounded 120s "\$$mkfs_command"/ { effect = NR } END { exit !(marker > 0 && started == marker + 1 && effect == started + 1) }' "$$effects"; \
	lock_cleanup_line="$$(grep -nF 'rmdir -- "$$campaign_lock"' "$$effects" | cut -d: -f1)"; \
	marker_cleanup_line="$$(grep -nF 'rm -f -- "$$consumed_marker"' "$$effects" | cut -d: -f1)"; \
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
