DISPOSABLE_VM_GPT_AGGREGATE_TOP_DIR := $(abspath $(dir $(lastword $(MAKEFILE_LIST)))/../..)
DISPOSABLE_VM_GPT_AGGREGATE_SCRIPT := $(DISPOSABLE_VM_GPT_AGGREGATE_TOP_DIR)/misc/vm/disposable-uefi-boot-gpt-aggregate-campaign.sh
DISPOSABLE_VM_GPT_AGGREGATE_BASE := $(DISPOSABLE_VM_GPT_AGGREGATE_TOP_DIR)/misc/vm/disposable-uefi-boot-storage-campaign.sh
DISPOSABLE_VM_GPT_AGGREGATE_EFFECTS := $(DISPOSABLE_VM_GPT_AGGREGATE_TOP_DIR)/misc/vm/disposable-uefi-boot-gpt-topology-effects.sh
DISPOSABLE_VM_GPT_AGGREGATE_RUNNER := $(DISPOSABLE_VM_GPT_AGGREGATE_TOP_DIR)/misc/vm/run-disposable-uefi-boot-gpt-aggregate-test.sh
DISPOSABLE_VM_GPT_AGGREGATE_RUST := $(DISPOSABLE_VM_GPT_AGGREGATE_TOP_DIR)/crates/forge/src/client/boot/disposable_vm_gpt_aggregate_publication_tests.rs

export VM_GPT_AGGREGATE_DESTRUCTIVE_CONFIRMATION

.PHONY: forge-disposable-vm-gpt-aggregate-code-test \
	forge-disposable-vm-gpt-aggregate-publication-test \
	disposable-vm-uefi-boot-gpt-aggregate-harness-test \
	disposable-vm-uefi-boot-gpt-aggregate-challenge \
	disposable-vm-uefi-boot-gpt-aggregate-admission \
	disposable-vm-uefi-boot-gpt-aggregate-campaign

forge-disposable-vm-gpt-aggregate-code-test: forge-active-reblit-boot-immutable-publication-attempt-test
	@set -eu; \
	mkdir -p "$(DISPOSABLE_VM_GPT_AGGREGATE_TOP_DIR)/target"; \
	listed="$$(mktemp "$(DISPOSABLE_VM_GPT_AGGREGATE_TOP_DIR)/target/gpt-aggregate-test-list.XXXXXXXXXXXX")"; \
	trap 'rm -f -- "$$listed"' EXIT; \
	$(CARGO) test --manifest-path "$(DISPOSABLE_VM_GPT_AGGREGATE_TOP_DIR)/Cargo.toml" \
		-p forge --lib -- --list >"$$listed"; \
	test "$$(grep -Fxc 'client::disposable_vm_gpt_aggregate_publication_tests::disposable_vm_receipt_bound_aggregate_publication: test' "$$listed")" = 1

forge-disposable-vm-gpt-aggregate-publication-test:
	@sh "$(DISPOSABLE_VM_GPT_AGGREGATE_RUNNER)"

disposable-vm-uefi-boot-gpt-aggregate-harness-test: \
	disposable-vm-uefi-boot-gpt-topology-harness-test \
	forge-disposable-vm-gpt-aggregate-code-test
	@set -euo pipefail; \
	base="$(DISPOSABLE_VM_GPT_AGGREGATE_BASE)"; \
	script="$(DISPOSABLE_VM_GPT_AGGREGATE_SCRIPT)"; \
	effects="$(DISPOSABLE_VM_GPT_AGGREGATE_EFFECTS)"; \
	runner="$(DISPOSABLE_VM_GPT_AGGREGATE_RUNNER)"; \
	rust="$(DISPOSABLE_VM_GPT_AGGREGATE_RUST)"; \
	make_file="$(DISPOSABLE_VM_GPT_AGGREGATE_TOP_DIR)/misc/make/disposable-vm-uefi-boot-gpt-aggregate-campaign.mk"; \
	for file in "$$base" "$$script" "$$effects" "$$runner" "$$rust" "$$make_file"; do \
		test -f "$$file"; test ! -L "$$file"; test "$$(wc -l <"$$file")" -le 1000; \
	done; \
	sh -n "$$base"; sh -n "$$script"; sh -n "$$effects"; sh -n "$$runner"; \
	for file in "$$base" "$$script" "$$effects" "$$runner" "$$rust"; do \
		grep -Fq 'gpt-receipt-bound-aggregate-v1' "$$file"; \
	done; \
	grep -Fq 'marker_protocol=3' "$$base"; \
	grep -Fq 'publish-aggregate-gpt:$$target_stable_path:$$target_bytes:$$target_diskseq:$$expected_boot_id:$$expected_commit:gpt-receipt-bound-aggregate-v1' "$$base"; \
	grep -Fq 'exec "$$base" "$$@" --campaign-profile gpt-receipt-bound-aggregate-v1' "$$script"; \
	for file in "$$runner" "$$effects" "$$rust"; do \
		grep -Fq 'disposable-vm-gpt-receipt-bound-aggregate-only' "$$file"; \
		grep -Fq 'CAST_VM_GPT_AGGREGATE_FIXTURE_PARENT' "$$file"; \
		grep -Fq 'CAST_VM_GPT_TOPOLOGY_PHASE' "$$file"; \
		grep -Fq 'CAST_VM_BOOT_PUBLICATION_EXPECTED_COMMIT' "$$file"; \
	done; \
	grep -Fq "test_name='client::disposable_vm_gpt_aggregate_publication_tests::disposable_vm_receipt_bound_aggregate_publication'" "$$runner"; \
	grep -Fq '/usr/bin/timeout --signal=TERM --kill-after=5s 180s' "$$runner"; \
	for file in "$$effects" "$$make_file"; do \
		grep -Fq 'forge-disposable-vm-gpt-aggregate-publication-test' "$$file"; \
	done; \
	profile_function="$$(sed -n '/^configure_gpt_publication_profile() {/,/^}/p' "$$effects")"; \
	eval "$$profile_function"; \
	campaign_profile=gpt-boot-topologies; configure_gpt_publication_profile; \
	test "$$topology_test_confirmation" = disposable-vm-gpt-topology-only; \
	test "$$topology_test_make_target" = forge-disposable-vm-gpt-boot-topology-test; \
	campaign_profile=gpt-receipt-bound-aggregate-v1; configure_gpt_publication_profile; \
	test "$$topology_test_confirmation" = disposable-vm-gpt-receipt-bound-aggregate-only; \
	test "$$topology_test_make_target" = forge-disposable-vm-gpt-aggregate-publication-test; \
	test "$$(grep -Fc 'stage_active_reblit_boot_sync(' "$$rust")" = 1; \
	test "$$(grep -Fc 'attempt_immutable_boot_publication(' "$$rust")" = 1; \
	stage_line="$$(grep -nF 'stage_active_reblit_boot_sync(' "$$rust" | cut -d: -f1)"; \
	attempt_line="$$(grep -nF 'attempt_immutable_boot_publication(' "$$rust" | cut -d: -f1)"; \
	test "$$stage_line" -lt "$$attempt_line"; \
	if rg -n 'stage_with_retained_stores|publish_immutable_boot_file_until|retain_boot_publication_parent_until|[.]synchronize_boot[[:space:]]*[(]|[.]promote[A-Za-z0-9_]*[[:space:]]*[(]|[.]clear_pending[A-Za-z0-9_]*[[:space:]]*[(]|\bBootSyncComplete\b' "$$rust"; then \
		echo 'aggregate VM test bypasses the high-level one-shot authority or performs a forbidden terminal effect' >&2; \
		exit 1; \
	else status="$$?"; test "$$status" = 1; fi; \
	grep -Fq 'run_gpt_topology_test alias publish' "$$effects"; \
	grep -Fq 'run_gpt_topology_test alias revalidate' "$$effects"; \
	grep -Fq 'run_gpt_topology_test distinct publish' "$$effects"; \
	grep -Fq 'run_gpt_topology_test distinct revalidate' "$$effects"; \
	run_alias="$$(sed -n '/^run_alias_topology() {/,/^}/p' "$$effects")"; \
	alias_publish_line="$$(grep -nF '    run_gpt_topology_test alias publish' <<<"$$run_alias" | cut -d: -f1)"; \
	alias_unmount_line="$$(grep -nF '    unmount_gpt_partition "$$esp_mount"' <<<"$$run_alias" | sed -n '1p' | cut -d: -f1)"; \
	alias_remount_line="$$(grep -nF '    mount_gpt_partition "$$esp_device"' <<<"$$run_alias" | sed -n '2p' | cut -d: -f1)"; \
	alias_revalidate_line="$$(grep -nF '    run_gpt_topology_test alias revalidate' <<<"$$run_alias" | cut -d: -f1)"; \
	test "$$alias_publish_line" -lt "$$alias_unmount_line"; test "$$alias_unmount_line" -lt "$$alias_remount_line"; \
	test "$$alias_remount_line" -lt "$$alias_revalidate_line"; \
	run_distinct="$$(sed -n '/^run_distinct_topology() {/,/^}/p' "$$effects")"; \
	distinct_publish_line="$$(grep -nF '    run_gpt_topology_test distinct publish' <<<"$$run_distinct" | cut -d: -f1)"; \
	distinct_unmount_line="$$(grep -nF '    unmount_gpt_partition "$$xbootldr_mount"' <<<"$$run_distinct" | sed -n '1p' | cut -d: -f1)"; \
	distinct_remount_line="$$(grep -nF '    mount_gpt_partition "$$xbootldr_device"' <<<"$$run_distinct" | sed -n '2p' | cut -d: -f1)"; \
	distinct_revalidate_line="$$(grep -nF '    run_gpt_topology_test distinct revalidate' <<<"$$run_distinct" | cut -d: -f1)"; \
	test "$$distinct_publish_line" -lt "$$distinct_unmount_line"; test "$$distinct_unmount_line" -lt "$$distinct_remount_line"; \
	test "$$distinct_remount_line" -lt "$$distinct_revalidate_line"; \
	run_campaign="$$(sed -n '/^run_campaign() {/,/^}/p' "$$effects")"; \
	prepare_line="$$(grep -nF '    prepare_boot_file_publication_runner' <<<"$$run_campaign" | cut -d: -f1)"; \
	fixture_line="$$(grep -nF '    prepare_aggregate_fixture_parents' <<<"$$run_campaign" | cut -d: -f1)"; \
	started_line="$$(grep -nF '    destructive_started=1' <<<"$$run_campaign" | cut -d: -f1)"; \
	test "$$prepare_line" -lt "$$fixture_line"; test "$$fixture_line" -lt "$$started_line"; \
	post_start="$$(tail -n +"$$started_line" <<<"$$run_campaign")"; \
	if rg -n 'cargo|nix|build|prepare_boot_file_publication_runner' <<<"$$post_start"; then exit 1; else status="$$?"; test "$$status" = 1; fi; \
	runtime_recipe="$$(sed -n '/^forge-disposable-vm-gpt-aggregate-publication-test:/,/^[^[:space:]].*:/p' "$$make_file" | sed '$$d')"; \
	test "$$(grep -Fc '$$(DISPOSABLE_VM_GPT_AGGREGATE_RUNNER)' <<<"$$runtime_recipe")" = 1; \
	if rg -n 'cargo|nix|build' <<<"$$runtime_recipe"; then exit 1; else status="$$?"; test "$$status" = 1; fi; \
	if grep -Eq '^[[:space:]]*(ssh|virsh|reboot|shutdown|poweroff)([[:space:]]|$$)' "$$script" "$$effects" "$$runner"; then exit 1; fi; \
	echo 'Disposable VM receipt-bound aggregate GPT harness static checks passed.'

disposable-vm-uefi-boot-gpt-aggregate-challenge:
	@sh "$(DISPOSABLE_VM_GPT_AGGREGATE_SCRIPT)" challenge \
		$(disposable_vm_boot_storage_common_arguments)

disposable-vm-uefi-boot-gpt-aggregate-admission:
	@sh "$(DISPOSABLE_VM_GPT_AGGREGATE_SCRIPT)" admit \
		$(disposable_vm_boot_storage_common_arguments) \
		--challenge "$${VM_BOOT_STORAGE_CHALLENGE-}"

disposable-vm-uefi-boot-gpt-aggregate-campaign:
	@sh "$(DISPOSABLE_VM_GPT_AGGREGATE_SCRIPT)" campaign \
		$(disposable_vm_boot_storage_common_arguments) \
		--challenge "$${VM_BOOT_STORAGE_CHALLENGE-}" \
		--destructive-confirmation "$${VM_GPT_AGGREGATE_DESTRUCTIVE_CONFIRMATION-}"
