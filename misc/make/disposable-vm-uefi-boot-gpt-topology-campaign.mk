DISPOSABLE_VM_GPT_TOPOLOGY_TOP_DIR := $(abspath $(dir $(lastword $(MAKEFILE_LIST)))/../..)
DISPOSABLE_VM_GPT_TOPOLOGY_SCRIPT := $(DISPOSABLE_VM_GPT_TOPOLOGY_TOP_DIR)/misc/vm/disposable-uefi-boot-gpt-topology-campaign.sh
DISPOSABLE_VM_GPT_TOPOLOGY_BASE := $(DISPOSABLE_VM_GPT_TOPOLOGY_TOP_DIR)/misc/vm/disposable-uefi-boot-storage-campaign.sh
DISPOSABLE_VM_GPT_TOPOLOGY_EFFECTS := $(DISPOSABLE_VM_GPT_TOPOLOGY_TOP_DIR)/misc/vm/disposable-uefi-boot-gpt-topology-effects.sh
DISPOSABLE_VM_GPT_TOPOLOGY_RUNNER := $(DISPOSABLE_VM_GPT_TOPOLOGY_TOP_DIR)/misc/vm/run-disposable-uefi-boot-gpt-topology-test.sh

export VM_GPT_TOPOLOGY_DESTRUCTIVE_CONFIRMATION

.PHONY: forge-disposable-vm-gpt-boot-topology-code-test \
	forge-disposable-vm-gpt-boot-topology-test \
	disposable-vm-uefi-boot-gpt-topology-harness-test \
	disposable-vm-uefi-boot-gpt-topology-challenge \
	disposable-vm-uefi-boot-gpt-topology-admission \
	disposable-vm-uefi-boot-gpt-topology-campaign

forge-disposable-vm-gpt-boot-topology-code-test: forge-linux-descriptor-boot-publication-parent-test
	@set -eu; \
	mkdir -p "$(DISPOSABLE_VM_GPT_TOPOLOGY_TOP_DIR)/target"; \
	listed="$$(mktemp "$(DISPOSABLE_VM_GPT_TOPOLOGY_TOP_DIR)/target/gpt-boot-topology-test-list.XXXXXXXXXXXX")"; \
	trap 'rm -f -- "$$listed"' EXIT; \
	$(CARGO) test --manifest-path "$(DISPOSABLE_VM_GPT_TOPOLOGY_TOP_DIR)/Cargo.toml" \
		-p forge --lib -- --list >"$$listed"; \
	test "$$(grep -Fxc 'client::disposable_vm_gpt_topology_tests::disposable_vm_authenticates_gpt_boot_topology_and_publishes_real_leaves: test' "$$listed")" = 1

forge-disposable-vm-gpt-boot-topology-test:
	@sh "$(DISPOSABLE_VM_GPT_TOPOLOGY_RUNNER)"

disposable-vm-uefi-boot-gpt-topology-harness-test: \
	disposable-vm-uefi-boot-storage-harness-test \
	forge-linux-descriptor-boot-publication-parent-test \
	forge-disposable-vm-gpt-boot-topology-code-test
	@set -euo pipefail; \
	base="$(DISPOSABLE_VM_GPT_TOPOLOGY_BASE)"; \
	script="$(DISPOSABLE_VM_GPT_TOPOLOGY_SCRIPT)"; \
	effects="$(DISPOSABLE_VM_GPT_TOPOLOGY_EFFECTS)"; \
	runner="$(DISPOSABLE_VM_GPT_TOPOLOGY_RUNNER)"; \
	rust="$(DISPOSABLE_VM_GPT_TOPOLOGY_TOP_DIR)/crates/forge/src/client/boot/disposable_vm_gpt_topology_tests.rs"; \
	make_file="$(DISPOSABLE_VM_GPT_TOPOLOGY_TOP_DIR)/misc/make/disposable-vm-uefi-boot-gpt-topology-campaign.mk"; \
	fixture_root=/; fixture_live_mount="$${fixture_root}boot/efi"; \
	for file in "$$base" "$$script" "$$effects" "$$runner" "$$rust" "$$make_file"; do \
		test -f "$$file"; test ! -L "$$file"; test "$$(wc -l <"$$file")" -le 1000; \
	done; \
	sh -n "$$base"; sh -n "$$script"; sh -n "$$effects"; sh -n "$$runner"; \
	common=( \
		--expected-hostname fixture-vm \
		--expected-machine-id 00000000000000000000000000000001 \
		--expected-boot-id 00000000-0000-0000-0000-000000000001 \
		--expected-virtualization kvm \
		--expected-commit aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa \
		--target-disk /dev/cast-fixture-target \
		--target-stable-path /dev/disk/by-"path"/cast-fixture-target \
		--target-diskseq 7 \
		--target-bytes 34359738368 \
		--expected-root-device /dev/cast-fixture-root \
		--expected-live-esp-device /dev/cast-fixture-live-esp \
		--expected-live-esp-mountpoint "$$fixture_live_mount" \
		--filesystem-label CASTTEST \
		--publication-parent EFI/Linux \
		--snapshot-confirmation snapshot-ready:00000000-0000-0000-0000-000000000001:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa \
		--remote-confirmation disposable-vm-remote-only \
		--cooperative-root-confirmation cooperative-guest-root-no-hotplug \
	); \
	challenge=bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb; \
	fixture_stable=/dev/disk/by-"path"/cast-fixture-target; \
	old_confirmation="erase:$$fixture_stable:34359738368:7:00000000-0000-0000-0000-000000000001"; \
	gpt_confirmation="repartition-gpt:$$fixture_stable:34359738368:7:00000000-0000-0000-0000-000000000001:gpt-boot-topologies"; \
	output="$$(mktemp "$(DISPOSABLE_VM_GPT_TOPOLOGY_TOP_DIR)/target/gpt-profile-arguments.XXXXXXXXXXXX")"; \
	old_marker="$$(mktemp "$(DISPOSABLE_VM_GPT_TOPOLOGY_TOP_DIR)/target/gpt-old-marker.XXXXXXXXXXXX")"; \
	gpt_marker="$$(mktemp "$(DISPOSABLE_VM_GPT_TOPOLOGY_TOP_DIR)/target/gpt-new-marker.XXXXXXXXXXXX")"; \
	normalized_marker="$$(mktemp "$(DISPOSABLE_VM_GPT_TOPOLOGY_TOP_DIR)/target/gpt-normalized-marker.XXXXXXXXXXXX")"; \
	format_capture="$$(mktemp "$(DISPOSABLE_VM_GPT_TOPOLOGY_TOP_DIR)/target/gpt-format-capture.XXXXXXXXXXXX")"; \
	trap 'rm -f -- "$$output" "$$old_marker" "$$gpt_marker" "$$normalized_marker" "$$format_capture"' EXIT; \
	status=0; sh "$$base" challenge "$${common[@]}" --campaign-profile unknown \
		>"$$output" 2>&1 || status=$$?; \
	test "$$status" = 2; grep -Fq -- '--campaign-profile must be gpt-boot-topologies' "$$output"; \
	status=0; sh "$$base" campaign "$${common[@]}" --challenge "$$challenge" \
		--destructive-confirmation "$$gpt_confirmation" >"$$output" 2>&1 || status=$$?; \
	test "$$status" = 2; grep -Fq -- '--destructive-confirmation does not bind' "$$output"; \
	status=0; sh "$$script" campaign "$${common[@]}" --challenge "$$challenge" \
		--destructive-confirmation "$$old_confirmation" >"$$output" 2>&1 || status=$$?; \
	test "$$status" = 2; grep -Fq -- '--destructive-confirmation does not bind' "$$output"; \
	status=0; sh "$$base" campaign "$${common[@]}" --challenge "$$challenge" \
		--destructive-confirmation "$$old_confirmation" >"$$output" 2>&1 || status=$$?; \
	test "$$status" = 1; \
	status=0; sh "$$script" campaign "$${common[@]}" --challenge "$$challenge" \
		--destructive-confirmation "$$gpt_confirmation" >"$$output" 2>&1 || status=$$?; \
	test "$$status" = 1; \
	status=0; sh "$$script" challenge --target-disk /dev/"vda" >"$$output" 2>&1 || status=$$?; \
	test "$$status" = 2; grep -Fq 'refuses every primary live-system disk target' "$$output"; \
	marker_function="$$(sed -n '/^write_marker_body() {/,/^}/p' "$$base")"; \
	eval "$$marker_function"; \
	expected_hostname=fixture-vm; expected_machine_id=00000000000000000000000000000001; \
	expected_boot_id=00000000-0000-0000-0000-000000000001; expected_virtualization=kvm; \
	ssh_connection_hash=cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc; \
	expected_commit=aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa; target_disk=/dev/cast-fixture-target; \
	target_stable_path=$$fixture_stable; target_diskseq=7; target_bytes=34359738368; \
	expected_root_device=/dev/cast-fixture-root; expected_live_esp_device=/dev/cast-fixture-live-esp; \
	expected_live_esp_mountpoint=$$fixture_live_mount; filesystem_label=CASTTEST; publication_parent=EFI/Linux; \
	snapshot_confirmation=snapshot-ready:00000000-0000-0000-0000-000000000001:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa; \
	remote_confirmation=disposable-vm-remote-only; cooperative_root_confirmation=cooperative-guest-root-no-hotplug; \
	campaign_profile=; write_marker_body 17 "$$challenge" >"$$old_marker"; \
	campaign_profile=gpt-boot-topologies; write_marker_body 17 "$$challenge" >"$$gpt_marker"; \
	test "$$(wc -l <"$$old_marker")" = 21; test "$$(grep -Fxc protocol=1 "$$old_marker")" = 1; \
	test "$$(grep -Fc campaign_profile= "$$old_marker")" = 0; \
	test "$$(wc -l <"$$gpt_marker")" = 22; test "$$(grep -Fxc protocol=2 "$$gpt_marker")" = 1; \
	test "$$(grep -Fxc campaign_profile=gpt-boot-topologies "$$gpt_marker")" = 1; \
	sed -e 's/^protocol=2$$/protocol=1/' -e '/^campaign_profile=gpt-boot-topologies$$/d' \
		"$$gpt_marker" >"$$normalized_marker"; \
	cmp -s "$$old_marker" "$$normalized_marker"; \
	format_function="$$(sed -n '/^format_partition() {/,/^}/p' "$$effects")"; \
	eval "$$format_function"; \
	verify_gpt_whole_disk_identity() ( partition_device=clobbered; partition_number=99; partition_type=clobbered; expected_start=99; expected_size=99; expected_devnum=99:99; expected_partuuid=clobbered; filesystem_label=clobbered; ); \
	reauthenticate_generated_partition() { printf 'reauth:%s\n' "$$*" >>"$$format_capture"; }; \
	authenticate_partition_filesystem() { printf 'filesystem:%s\n' "$$*" >>"$$format_capture"; }; \
	run_bounded() { shift; "$$@"; }; die() { printf '%s\n' "$$*" >&2; return 1; }; \
	mkfs_command=true; esp_formatted=0; xbootldr_formatted=0; filesystem_label=CASTTEST; \
	format_partition /dev/cast-fixture-p1 CASTESP 1 type-a 2048 524288 1:1 uuid-a; \
	format_partition /dev/cast-fixture-p2 CASTXBOOT 2 type-b 526336 1048576 1:2 uuid-b; \
	test "$$filesystem_label" = CASTTEST; test "$$esp_formatted" = 1; test "$$xbootldr_formatted" = 1; \
	test "$$(grep -Fxc 'reauth:/dev/cast-fixture-p1 1 type-a 2048 524288 1:1 uuid-a 0' "$$format_capture")" = 2; \
	test "$$(grep -Fxc 'filesystem:/dev/cast-fixture-p1 CASTESP' "$$format_capture")" = 2; \
	test "$$(grep -Fxc 'reauth:/dev/cast-fixture-p2 2 type-b 526336 1048576 1:2 uuid-b 0' "$$format_capture")" = 2; \
	test "$$(grep -Fxc 'filesystem:/dev/cast-fixture-p2 CASTXBOOT' "$$format_capture")" = 2; \
	grep -Fq "die 'target whole disk already has a partition'" "$$base"; \
	grep -Fq "die 'target disk is the live root parent disk'" "$$base"; \
	grep -Fq "die 'target disk is the live ESP parent disk'" "$$base"; \
	grep -Fq 'live_system_disk=/dev/"vda"' "$$script"; \
	grep -Fq '"$$live_system_disk" | "$$live_system_disk"[0-9]*)' "$$script"; \
	grep -Fq 'live_system_disk=/dev/"vda"' "$$effects"; \
	grep -Fq '"$$live_system_disk" | "$$live_system_disk"[0-9]*)' "$$effects"; \
	grep -Fq 'verify_empty_kernel_directory "$$BLOCK_SYS_PATH/holders"' "$$effects"; \
	grep -Fq 'verify_empty_kernel_directory "$$BLOCK_SYS_PATH/slaves"' "$$effects"; \
	grep -Fq 'verify_partition_geometry 1 2048 524288' "$$effects"; \
	grep -Fq 'verify_partition_geometry 2 526336 1048576' "$$effects"; \
	grep -Fq 'PART_ENTRY_TYPE' "$$effects"; \
	grep -Fq 'PART_ENTRY_UUID' "$$effects"; \
	grep -Fq 'require_gpt_repartition_safe' "$$effects"; \
	grep -Fq '"$$partition_auth_mount_count"' "$$effects"; \
	grep -Fq '"$$fsck_partition_devnum" "$$fsck_partition_partuuid" 0' "$$effects"; \
	grep -Fq '"$$mount_partition_devnum" "$$mount_partition_partuuid" 1' "$$effects"; \
	grep -Fq 'run_bounded 120s "$$sfdisk_command"' "$$effects"; \
	grep -Fq 'run_bounded 120s "$$mkfs_command" --mbr=n' "$$effects"; \
	grep -Fq 'run_bounded 120s "$$fsck_fat_command" -n' "$$effects"; \
	grep -Fq 'verify_gpt_whole_disk_identity() (' "$$effects"; \
	grep -Fq 'verify_partition_geometry() (' "$$effects"; \
	if grep -Eq '^[[:space:]]*(expected_hostname|expected_machine_id|expected_boot_id|expected_virtualization|expected_commit|target_disk|target_stable_path|target_diskseq|target_bytes|expected_root_device|expected_live_esp_device|expected_live_esp_mountpoint|filesystem_label|publication_parent|snapshot_confirmation|remote_confirmation|cooperative_root_confirmation|campaign_profile|challenge|destructive_confirmation)[[:space:]]*=' "$$effects"; then exit 1; else status=$$?; test "$$status" = 1; fi; \
	if rg -n 'create_unclaimed_boot_layout_foundation|mkdir[^\n]*(EFI|Linux|loader|entries)' "$$effects"; then exit 1; else status=$$?; test "$$status" = 1; fi; \
	test "$$(grep -Fc 'run_alias_topology' "$$effects")" = 2; \
	test "$$(grep -Fc 'run_distinct_topology' "$$effects")" = 2; \
	run_campaign="$$(sed -n '/^run_campaign() {/,/^}/p' "$$effects")"; \
	build_line="$$(grep -nF '    prepare_boot_file_publication_runner' <<<"$$run_campaign" | cut -d: -f1)"; \
	last_admission_line="$$(grep -nF '    verify_target_disk' <<<"$$run_campaign" | tail -n 1 | cut -d: -f1)"; \
	marker_line="$$(grep -nF '    verify_marker "$$consumed_marker"' <<<"$$run_campaign" | sed -n '2p' | cut -d: -f1)"; \
	terminal_marker_line="$$(grep -nF '    verify_marker "$$consumed_marker"' <<<"$$run_campaign" | tail -n 1 | cut -d: -f1)"; \
	effect_line="$$(grep -nF '    destructive_started=1' <<<"$$run_campaign" | cut -d: -f1)"; \
	alias_line="$$(grep -nF '    run_alias_topology' <<<"$$run_campaign" | cut -d: -f1)"; \
	distinct_line="$$(grep -nF '    run_distinct_topology' <<<"$$run_campaign" | cut -d: -f1)"; \
	test "$$build_line" -lt "$$last_admission_line"; test "$$last_admission_line" -lt "$$marker_line"; \
	test "$$marker_line" -lt "$$effect_line"; test "$$effect_line" -lt "$$alias_line"; \
	test "$$alias_line" -lt "$$distinct_line"; test "$$distinct_line" -lt "$$terminal_marker_line"; \
	grep -Fq 'PreparedActiveReblitMountedBootTopology::prepare(&installation)' "$$rust"; \
	grep -Fq 'view.destination_device(), target.destination.raw_device()' "$$rust"; \
	grep -Fq 'view.destination_inode(), target.destination.inode()' "$$rust"; \
	grep -Fq 'view.destination_mount_id(), target.mount_id' "$$rust"; \
	grep -Fq 'retain_boot_publication_parent_until(parent_components, deadline())' "$$rust"; \
	grep -Fq '&["EFI", "Linux"]' "$$rust"; \
	grep -Fq '&["loader", "entries"]' "$$rust"; \
	grep -Fq 'parent.matches_leaf_evidence(&publication)' "$$rust"; \
	grep -Fq 'parent.root_device(), target.destination.raw_device()' "$$rust"; \
	grep -Fq 'parent.root_inode(), target.destination.inode()' "$$rust"; \
	grep -Fq 'parent.root_mount_id(), target.mount_id' "$$rust"; \
	grep -Fq 'Path::new("/var/tmp").join(format!("cast-vm-boot-storage-{expected_boot_id}-{challenge}"))' "$$rust"; \
	grep -Fq 'assert_eq!(installation_root, build_root.join("topology-installation"));' "$$rust"; \
	publish_function="$$(sed -n '/^fn publish(/,/^#\[test\]/p' "$$rust")"; \
	grep -Fq 'let publication = parent' <<<"$$publish_function"; \
	if grep -Fq 'view.publish_immutable_boot_file_until' <<<"$$publish_function"; then exit 1; else status=$$?; test "$$status" = 1; fi; \
	grep -Fq '/usr/bin/timeout --signal=TERM --kill-after=5s 180s' "$$runner"; \
	runtime_recipe="$$(sed -n '/^forge-disposable-vm-gpt-boot-topology-test:/,/^[^[:space:]].*:/p' "$$make_file" | sed '$$d')"; \
	test "$$(grep -Fc 'run-disposable-uefi-boot-gpt-topology-test.sh' <<<"$$runtime_recipe")" = 0; \
	test "$$(grep -Fc '$$(DISPOSABLE_VM_GPT_TOPOLOGY_RUNNER)' <<<"$$runtime_recipe")" = 1; \
	if rg -n 'cargo|nix|build' <<<"$$runtime_recipe"; then exit 1; else status=$$?; test "$$status" = 1; fi; \
	if grep -Eq '^[[:space:]]*(ssh|virsh|reboot|shutdown|poweroff)([[:space:]]|$$)' \
		"$$script" "$$effects" "$$runner"; then exit 1; fi; \
	echo 'Disposable VM GPT ESP/XBOOTLDR harness static checks passed.'

disposable-vm-uefi-boot-gpt-topology-challenge:
	@sh "$(DISPOSABLE_VM_GPT_TOPOLOGY_SCRIPT)" challenge \
		$(disposable_vm_boot_storage_common_arguments)

disposable-vm-uefi-boot-gpt-topology-admission:
	@sh "$(DISPOSABLE_VM_GPT_TOPOLOGY_SCRIPT)" admit \
		$(disposable_vm_boot_storage_common_arguments) \
		--challenge "$${VM_BOOT_STORAGE_CHALLENGE-}"

disposable-vm-uefi-boot-gpt-topology-campaign:
	@sh "$(DISPOSABLE_VM_GPT_TOPOLOGY_SCRIPT)" campaign \
		$(disposable_vm_boot_storage_common_arguments) \
		--challenge "$${VM_BOOT_STORAGE_CHALLENGE-}" \
		--destructive-confirmation "$${VM_GPT_TOPOLOGY_DESTRUCTIVE_CONFIRMATION-}"
