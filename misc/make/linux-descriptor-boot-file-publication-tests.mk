DESCRIPTOR_BOOT_FILE_PUBLICATION_TOP_DIR := $(abspath $(dir $(lastword $(MAKEFILE_LIST)))/../..)
CARGO ?= cargo

.PHONY: forge-linux-descriptor-boot-file-publication-test \
	forge-linux-descriptor-boot-file-publication-vfat-build \
	forge-linux-descriptor-boot-file-publication-vfat-test

forge-linux-descriptor-boot-file-publication-test: host-storage-safety-test forge-linux-descriptor-boot-namespace-production-test forge-active-reblit-boot-publication-plan-test
	@set -euo pipefail; \
	mkdir -p "$(DESCRIPTOR_BOOT_FILE_PUBLICATION_TOP_DIR)/target"; \
	listed="$$( mktemp "$(DESCRIPTOR_BOOT_FILE_PUBLICATION_TOP_DIR)/target/linux-descriptor-boot-file-publication-test-list.XXXXXXXXXXXX" )"; \
	trap 'rm -f "$$listed"' EXIT; \
	$(CARGO) test --manifest-path "$(DESCRIPTOR_BOOT_FILE_PUBLICATION_TOP_DIR)/Cargo.toml" -p forge --lib -- --list | tee "$$listed" >/dev/null; \
	grep -q . "$$listed"; \
	prefix='linux_fs::tests::descriptor_boot_file_publication::'; \
	test "$$( grep -Ec "^$$prefix.*: test$$" "$$listed" )" = 16; \
	for name in \
		canonical_request_cannot_alias_private_stage_namespace_case_insensitively \
		stop_after_exclusive_creation_preserves_empty_mode_0644_residue_and_retry_refuses \
		stop_mid_multichunk_write_preserves_partial_mode_0644_residue_and_retry_refuses \
		stop_after_final_write_preserves_exact_mode_0644_residue_then_same_inode_resume \
		generated_source_publishes_once_and_exact_destination_is_idempotent \
		sealed_source_streams_multiple_chunks_without_exposing_its_descriptor \
		different_canonical_destination_is_preserved_and_refused \
		exact_private_residue_is_resumed_without_replacement_or_deletion \
		different_and_foreign_private_residue_are_preserved_and_refused \
		exact_bytes_with_wrong_effective_mode_are_not_adopted \
		source_sha256_mismatch_fails_before_private_publication \
		error_reported_after_single_move_is_reconciled_as_published \
		durability_suffix_failures_leave_an_exact_idempotent_canonical_leaf \
		retained_attachment_replacement_fails_before_mutating_either_directory \
		same_credential_private_name_substitution_fails_without_validated_evidence \
		disposable_vm_vfat_publishes_and_revalidates_one_real_leaf; do \
		grep -Fqx "$$prefix$$name: test" "$$listed"; \
	done; \
	module="$(DESCRIPTOR_BOOT_FILE_PUBLICATION_TOP_DIR)/crates/forge/src/linux_fs/mount_namespace/attachment/boot_file_publication.rs"; \
	module_dir="$(DESCRIPTOR_BOOT_FILE_PUBLICATION_TOP_DIR)/crates/forge/src/linux_fs/mount_namespace/attachment/boot_file_publication"; \
	bridge="$(DESCRIPTOR_BOOT_FILE_PUBLICATION_TOP_DIR)/crates/forge/src/linux_fs/descriptor_boot_namespace/production/retained/publication_source.rs"; \
	reserved_names="$(DESCRIPTOR_BOOT_FILE_PUBLICATION_TOP_DIR)/crates/forge/src/linux_fs/boot_file_publication_name.rs"; \
	plan_policy="$(DESCRIPTOR_BOOT_FILE_PUBLICATION_TOP_DIR)/crates/forge/src/client/boot/active_reblit_publication_plan/path_policy.rs"; \
	tests="$(DESCRIPTOR_BOOT_FILE_PUBLICATION_TOP_DIR)/crates/forge/src/linux_fs/tests/descriptor_boot_file_publication.rs"; \
	grep -Fq 'impl RevalidatedTaskRootedAttachment' "$$module"; \
	grep -Fq 'pub(crate) fn publish_immutable_boot_file_until' "$$module"; \
	test "$$( grep -Fc 'renameat2_noreplace_once(parent, private_name, parent, canonical_name)' "$$module" )" = 1; \
	grep -Fq 'BoundRetainedBootFileSource::bind_until' "$$module"; \
	grep -Fq 'BootNamespaceDestinationState::Absent' "$$module"; \
	grep -Fq 'BootNamespaceDestinationState::Exact' "$$module"; \
	grep -Fq 'BootNamespaceDestinationState::Different' "$$module"; \
	grep -Fq 'synchronizing the exact private boot-file leaf' "$$module"; \
	grep -Fq 'canonical' "$$module"; \
	grep -Fq 'parent' "$$module"; \
	grep -Fq 'sync_filesystem_until(parent, deadline)' "$$module"; \
	grep -Fq 'metadata.permissions().mode() & 0o7777 != 0o644' "$$module_dir/destination.rs"; \
	grep -Fq 'sha256.update(&buffer[..found]);' "$$module_dir/effect.rs"; \
	grep -Fq 'xxh3.update(&buffer[..found]);' "$$module_dir/effect.rs"; \
	grep -Fq 'terminally_revalidate_expected_streams' "$$bridge"; \
	grep -Fq 'non-crash-recoverable foundation' "$$module"; \
	grep -Fq 'mode-0644 deterministic residue' "$$module"; \
	grep -Fq 'no standalone reboot' "$$module"; \
	grep -Fq 'later current-plan' "$$module"; \
	grep -Fq 'VFAT mounted with `fmask=0133` already expose 0644' "$$module"; \
	grep -Fq 'Streaming never changes' "$$module"; \
	grep -Fq 'same credentials' "$$module"; \
	grep -Fq 'poisoning it' "$$module"; \
	grep -Fq 'AfterExclusiveCreation' "$$module_dir/effect.rs"; \
	grep -Fq 'MidMultiChunkWrite' "$$module_dir/effect.rs"; \
	grep -Fq 'AfterFinalWriteBeforeSourceValidation' "$$module_dir/effect.rs"; \
	grep -Fq 'before_private_name_rename();' "$$module"; \
	grep -Fq 'arm_retained_boot_file_private_name_substitution' "$$module_dir/effect.rs"; \
	grep -Fq 'ReservedPrivatePublicationLeaf' "$$module_dir/error.rs"; \
	grep -Fq 'is_retained_boot_file_private_component(request.canonical_leaf())' "$$module"; \
	test "$$( grep -Fc 'nix::libc::fchmod(file.as_raw_fd(), 0o644)' "$$module_dir/destination.rs" )" = 1; \
	if rg -n 'fchmod' "$$module_dir/effect.rs"; then exit 1; else status="$$?"; test "$$status" = 1; fi; \
	if rg -n '0o600|0600' "$$module_dir" "$$module"; then exit 1; else status="$$?"; test "$$status" = 1; fi; \
	grep -Fq 'RETAINED_BOOT_FILE_PRIVATE_PREFIX: &str = ".cast-payload-"' "$$reserved_names"; \
	grep -Fq 'eq_ignore_ascii_case(RETAINED_BOOT_FILE_PRIVATE_PREFIX.as_bytes())' "$$reserved_names"; \
	grep -Fq 'ReservedPrivatePublicationComponent' "$$plan_policy"; \
	if rg -n 'remove_(file|dir)\(|create_dir\(|mkdirat\(|RENAME_EXCHANGE|mount_partitions\(' "$$module" "$$module_dir" "$$bridge"; then exit 1; else status="$$?"; test "$$status" = 1; fi; \
	if rg -n 'pub\(crate\) fn (?:publish|create|rename)[[:alnum:]_]*\(' "$$module_dir" "$$bridge"; then exit 1; else status="$$?"; test "$$status" = 1; fi; \
	for file in "$$module" "$$module_dir"/*.rs "$$bridge" "$$reserved_names" "$$plan_policy" "$$tests" "$(DESCRIPTOR_BOOT_FILE_PUBLICATION_TOP_DIR)/misc/make/linux-descriptor-boot-file-publication-tests.mk"; do \
		test "$$( wc -l < "$$file" )" -le 1000; \
	done; \
	timeout 1200s $(CARGO) test --manifest-path "$(DESCRIPTOR_BOOT_FILE_PUBLICATION_TOP_DIR)/Cargo.toml" -p forge --lib "$$prefix" -- --test-threads=1

forge-linux-descriptor-boot-file-publication-vfat-build:
	@set -euo pipefail; \
	test "$${CAST_VM_BOOT_PUBLICATION_CONFIRMATION-}" = disposable-vm-vfat-publisher-only; \
	test "$${CAST_VM_BOOT_PUBLICATION_CONSUMED_MARKER-}" = /run/cast-vm-boot-storage/authorization-v1.consumed; \
	test -n "$${CAST_VM_BOOT_PUBLICATION_EXPECTED_HOSTNAME-}"; \
	test -n "$${CAST_VM_BOOT_PUBLICATION_EXPECTED_MACHINE_ID-}"; \
	test -n "$${CAST_VM_BOOT_PUBLICATION_EXPECTED_BOOT_ID-}"; \
	test -n "$${CAST_VM_BOOT_PUBLICATION_EXPECTED_VIRTUALIZATION-}"; \
	test -n "$${CAST_VM_BOOT_PUBLICATION_EXPECTED_SSH_SHA256-}"; \
	test -n "$${SSH_CONNECTION-}"; \
	source_root="$${CAST_VM_BOOT_PUBLICATION_SOURCE_ROOT-}"; \
	case "$$source_root" in /nix/store/?*) ;; *) exit 2 ;; esac; \
	test "$(DESCRIPTOR_BOOT_FILE_PUBLICATION_TOP_DIR)" = "$$source_root"; \
	test "$(CURDIR)" = "$$source_root"; \
	trusted_tools='/usr/bin/id /usr/bin/stat /usr/bin/cat /usr/bin/systemd-detect-virt /usr/bin/readlink /usr/bin/sha256sum /usr/bin/mktemp /usr/bin/chmod /usr/bin/mv /usr/bin/rm /usr/bin/find'; \
	for tool in $$trusted_tools; do \
		test -f "$$tool" && test -x "$$tool"; \
	done; \
	for tool in $$trusted_tools; do \
		tool_owner="$$(/usr/bin/stat -Lc '%u' -- "$$tool")"; \
		tool_mode="$$(/usr/bin/stat -Lc '%a' -- "$$tool")"; \
		test "$$tool_owner" = 0; \
		(( (8#$$tool_mode & 0022) == 0 )); \
	done; \
	test "$$(/usr/bin/id -u)" = 0; \
	test -d "$$source_root" && test ! -L "$$source_root"; \
	test "$$source_root" = "$$(/usr/bin/readlink -e -- "$$source_root")"; \
	IFS=: read -r source_owner source_group source_mode source_type \
		<<<"$$(/usr/bin/stat -Lc '%u:%g:%a:%F' -- "$$source_root")"; \
	test "$$source_owner" = 0 && test "$$source_group" = 0; \
	test "$$source_type" = directory; \
	(( (8#$$source_mode & 0022) == 0 )); \
	test -d /sys/firmware/efi && test ! -L /sys/firmware/efi; \
	test "$$(/usr/bin/cat /proc/sys/kernel/hostname)" = "$${CAST_VM_BOOT_PUBLICATION_EXPECTED_HOSTNAME}"; \
	test "$$(/usr/bin/cat /etc/machine-id)" = "$${CAST_VM_BOOT_PUBLICATION_EXPECTED_MACHINE_ID}"; \
	test "$$(/usr/bin/cat /proc/sys/kernel/random/boot_id)" = "$${CAST_VM_BOOT_PUBLICATION_EXPECTED_BOOT_ID}"; \
	detected_virtualization="$$(/usr/bin/systemd-detect-virt --vm)"; \
	test -n "$$detected_virtualization" && test "$$detected_virtualization" != none; \
	test "$$detected_virtualization" = "$${CAST_VM_BOOT_PUBLICATION_EXPECTED_VIRTUALIZATION}"; \
	marker="$${CAST_VM_BOOT_PUBLICATION_CONSUMED_MARKER}"; \
	test -f "$$marker" && test ! -L "$$marker"; \
	test "$$(/usr/bin/stat -Lc '%u:%g:%a:%F:%h' -- "$$marker")" = '0:0:600:regular file:1'; \
	marker_challenge=; marker_challenge_count=0; \
	while IFS= read -r marker_line; do \
		case "$$marker_line" in challenge=*) \
			marker_challenge="$${marker_line#challenge=}"; \
			marker_challenge_count=$$((marker_challenge_count + 1));; \
		esac; \
	done <"$$marker"; \
	test "$$marker_challenge_count" = 1; \
	[[ "$$marker_challenge" =~ ^[0-9a-f]{64}$$ ]]; \
	expected_build_root="/var/tmp/cast-vm-boot-storage-$${CAST_VM_BOOT_PUBLICATION_EXPECTED_BOOT_ID}-$$marker_challenge"; \
	test "$${CAST_VM_BOOT_PUBLICATION_BUILD_ROOT-}" = "$$expected_build_root"; \
	test "$${CARGO_TARGET_DIR-}" = "$$expected_build_root/target"; \
	test "$${CARGO_HOME-}" = "$$expected_build_root/cargo-home"; \
	case "$${CARGO_TARGET_DIR}" in /run/cast-vm-boot-storage/mount|/run/cast-vm-boot-storage/mount/*) exit 2 ;; esac; \
	for directory in "$$expected_build_root" "$${CARGO_TARGET_DIR}" "$${CARGO_HOME}"; do \
		test -d "$$directory" && test ! -L "$$directory"; \
		test "$$(/usr/bin/stat -Lc '%u:%g:%a:%F' -- "$$directory")" = '0:0:700:directory'; \
	done; \
	test -z "$$(/usr/bin/find "$${CARGO_TARGET_DIR}" -mindepth 1 -print -quit)"; \
	test -z "$$(/usr/bin/find "$${CARGO_HOME}" -mindepth 1 -print -quit)"; \
	develop_profile="$${CAST_VM_BOOT_PUBLICATION_DEVELOP_PROFILE-}"; \
	test "$$develop_profile" = "$$expected_build_root/nix-develop-profile"; \
	test -L "$$develop_profile"; \
	test "$$(/usr/bin/stat -c '%u:%g:%F:%h' -- "$$develop_profile")" = '0:0:symbolic link:1'; \
	develop_profile_store="$$(/usr/bin/readlink -e -- "$$develop_profile")"; \
	case "$$develop_profile_store" in /nix/store/?*) ;; *) exit 2 ;; esac; \
	test -f "$$develop_profile_store" && test ! -L "$$develop_profile_store"; \
	IFS=: read -r profile_owner profile_group profile_mode profile_type \
		<<<"$$(/usr/bin/stat -Lc '%u:%g:%a:%F' -- "$$develop_profile_store")"; \
	test "$$profile_owner" = 0 && test "$$profile_group" = 0; \
	test "$$profile_type" = 'regular file'; \
	(( (8#$$profile_mode & 0022) == 0 )); \
	manifest="$${CAST_VM_BOOT_PUBLICATION_BINARY_MANIFEST-}"; \
	test "$$manifest" = "$$expected_build_root/forge-libtest-manifest-v1"; \
	test ! -e "$$manifest" && test ! -L "$$manifest"; \
	test "$(CARGO)" = cargo; \
	cargo_path="$$(command -v cargo)"; \
	jq_path="$$(command -v jq)"; \
	case "$$cargo_path" in /nix/store/*/bin/cargo) ;; *) exit 2 ;; esac; \
	case "$$jq_path" in /nix/store/*/bin/jq) ;; *) exit 2 ;; esac; \
	for tool in "$$cargo_path" "$$jq_path"; do \
		test -f "$$tool" && test -x "$$tool"; \
		tool_owner="$$(/usr/bin/stat -Lc '%u' -- "$$tool")"; \
		tool_mode="$$(/usr/bin/stat -Lc '%a' -- "$$tool")"; \
		test "$$tool_owner" = 0; \
		(( (8#$$tool_mode & 0022) == 0 )); \
	done; \
	cargo_messages="$$(/usr/bin/mktemp "$$expected_build_root/.forge-libtest-cargo-json.XXXXXXXXXXXX")"; \
	cargo_candidates="$$(/usr/bin/mktemp "$$expected_build_root/.forge-libtest-candidates.XXXXXXXXXXXX")"; \
	manifest_temporary=; \
	trap '/usr/bin/rm -f -- "$$cargo_messages" "$$cargo_candidates" "$${manifest_temporary-}"' EXIT; \
	cd /; \
	$(CARGO) test --locked \
		--manifest-path "$$source_root/Cargo.toml" \
		-p forge --lib --no-run --message-format=json >"$$cargo_messages"; \
	"$$jq_path" -r --arg manifest "$$source_root/crates/forge/Cargo.toml" \
		'select(.reason == "compiler-artifact" \
			and .manifest_path == $$manifest \
			and .target.name == "forge" \
			and .target.kind == ["lib"] \
			and .profile.test == true \
			and (.executable | type == "string")) | .executable' \
		"$$cargo_messages" >"$$cargo_candidates"; \
	mapfile -t candidate_lines <"$$cargo_candidates"; \
	test "$${#candidate_lines[@]}" = 1; \
	executable="$${candidate_lines[0]}"; \
	test -n "$$executable"; \
	case "$$executable" in "$${CARGO_TARGET_DIR}"/?*) ;; *) exit 2 ;; esac; \
	case "$$executable" in *$$'\t'*|*$$'\n'*) exit 2 ;; esac; \
	test -f "$$executable" && test ! -L "$$executable" && test -x "$$executable"; \
	test "$$executable" = "$$(/usr/bin/readlink -e -- "$$executable")"; \
	IFS=: read -r executable_owner executable_group executable_mode executable_type executable_links \
		<<<"$$(/usr/bin/stat -Lc '%u:%g:%a:%F:%h' -- "$$executable")"; \
	test "$$executable_owner" = 0 && test "$$executable_group" = 0; \
	test "$$executable_type" = 'regular file' && test "$$executable_links" = 1; \
	(( (8#$$executable_mode & 0022) == 0 )); \
	executable_sha256="$$(/usr/bin/sha256sum -- "$$executable")"; \
	executable_sha256="$${executable_sha256%% *}"; \
	[[ "$$executable_sha256" =~ ^[0-9a-f]{64}$$ ]]; \
	manifest_temporary="$$(/usr/bin/mktemp "$$expected_build_root/.forge-libtest-manifest-v1.XXXXXXXXXXXX")"; \
	test "$$(/usr/bin/stat -Lc '%u:%g:%a:%F:%h' -- "$$manifest_temporary")" = '0:0:600:regular file:1'; \
	printf 'protocol=1\tsource_root=%s\tdevelop_profile=%s\tdevelop_profile_store=%s\ttarget_root=%s\texecutable=%s\tsha256=%s\n' \
		"$$source_root" "$$develop_profile" "$$develop_profile_store" \
		"$${CARGO_TARGET_DIR}" "$$executable" "$$executable_sha256" \
		>"$$manifest_temporary"; \
	/usr/bin/chmod 0600 -- "$$manifest_temporary"; \
	test "$$(/usr/bin/stat -Lc '%u:%g:%a:%F:%h' -- "$$manifest_temporary")" = '0:0:600:regular file:1'; \
	/usr/bin/mv -T -- "$$manifest_temporary" "$$manifest"; \
	manifest_temporary=; \
	test -f "$$manifest" && test ! -L "$$manifest"; \
	test "$$(/usr/bin/stat -Lc '%u:%g:%a:%F:%h' -- "$$manifest")" = '0:0:600:regular file:1'; \
	mapfile -t manifest_lines <"$$manifest"; \
	test "$${#manifest_lines[@]}" = 1; \
	expected_manifest_line="$$(printf 'protocol=1\tsource_root=%s\tdevelop_profile=%s\tdevelop_profile_store=%s\ttarget_root=%s\texecutable=%s\tsha256=%s' \
		"$$source_root" "$$develop_profile" "$$develop_profile_store" \
		"$${CARGO_TARGET_DIR}" "$$executable" "$$executable_sha256")"; \
	test "$${manifest_lines[0]}" = "$$expected_manifest_line"; \
	test "$$(/usr/bin/stat -Lc '%s' -- "$$manifest")" = "$$(( $${#expected_manifest_line} + 1 ))"; \
	/usr/bin/rm -f -- "$$cargo_messages" "$$cargo_candidates"; \
	trap - EXIT

forge-linux-descriptor-boot-file-publication-vfat-test:
	@set -euo pipefail; \
	test "$${CAST_VM_BOOT_PUBLICATION_CONFIRMATION-}" = disposable-vm-vfat-publisher-only; \
	case "$${CAST_VM_BOOT_PUBLICATION_PHASE-}" in publish|revalidate) ;; *) exit 2 ;; esac; \
	case "$${CAST_VM_BOOT_PUBLICATION_PARENT-}" in /run/cast-vm-boot-storage/mount/?*) ;; *) exit 2 ;; esac; \
	test "$${CAST_VM_BOOT_PUBLICATION_CONSUMED_MARKER-}" = /run/cast-vm-boot-storage/authorization-v1.consumed; \
	test -n "$${CAST_VM_BOOT_PUBLICATION_EXPECTED_HOSTNAME-}"; \
	test -n "$${CAST_VM_BOOT_PUBLICATION_EXPECTED_MACHINE_ID-}"; \
	test -n "$${CAST_VM_BOOT_PUBLICATION_EXPECTED_BOOT_ID-}"; \
	test -n "$${CAST_VM_BOOT_PUBLICATION_EXPECTED_VIRTUALIZATION-}"; \
	test -n "$${CAST_VM_BOOT_PUBLICATION_EXPECTED_TARGET_DEVNUM-}"; \
	test -n "$${CAST_VM_BOOT_PUBLICATION_EXPECTED_SSH_SHA256-}"; \
	test -n "$${SSH_CONNECTION-}"; \
	source_root="$${CAST_VM_BOOT_PUBLICATION_SOURCE_ROOT-}"; \
	case "$$source_root" in /nix/store/?*) ;; *) exit 2 ;; esac; \
	test "$(DESCRIPTOR_BOOT_FILE_PUBLICATION_TOP_DIR)" = "$$source_root"; \
	test "$(CURDIR)" = "$$source_root"; \
	trusted_tools='/usr/bin/id /usr/bin/stat /usr/bin/cat /usr/bin/systemd-detect-virt /usr/bin/readlink /usr/bin/sha256sum'; \
	for tool in $$trusted_tools; do \
		test -f "$$tool" && test -x "$$tool"; \
	done; \
	for tool in $$trusted_tools; do \
		tool_owner="$$(/usr/bin/stat -Lc '%u' -- "$$tool")"; \
		tool_mode="$$(/usr/bin/stat -Lc '%a' -- "$$tool")"; \
		test "$$tool_owner" = 0; \
		(( (8#$$tool_mode & 0022) == 0 )); \
	done; \
	test "$$(/usr/bin/id -u)" = 0; \
	test -d "$$source_root" && test ! -L "$$source_root"; \
	test "$$source_root" = "$$(/usr/bin/readlink -e -- "$$source_root")"; \
	IFS=: read -r source_owner source_group source_mode source_type \
		<<<"$$(/usr/bin/stat -Lc '%u:%g:%a:%F' -- "$$source_root")"; \
	test "$$source_owner" = 0 && test "$$source_group" = 0; \
	test "$$source_type" = directory; \
	(( (8#$$source_mode & 0022) == 0 )); \
	test -d /sys/firmware/efi && test ! -L /sys/firmware/efi; \
	test "$$(/usr/bin/cat /proc/sys/kernel/hostname)" = "$${CAST_VM_BOOT_PUBLICATION_EXPECTED_HOSTNAME}"; \
	test "$$(/usr/bin/cat /etc/machine-id)" = "$${CAST_VM_BOOT_PUBLICATION_EXPECTED_MACHINE_ID}"; \
	test "$$(/usr/bin/cat /proc/sys/kernel/random/boot_id)" = "$${CAST_VM_BOOT_PUBLICATION_EXPECTED_BOOT_ID}"; \
	detected_virtualization="$$(/usr/bin/systemd-detect-virt --vm)"; \
	test -n "$$detected_virtualization" && test "$$detected_virtualization" != none; \
	test "$$detected_virtualization" = "$${CAST_VM_BOOT_PUBLICATION_EXPECTED_VIRTUALIZATION}"; \
	marker="$${CAST_VM_BOOT_PUBLICATION_CONSUMED_MARKER}"; \
	test -f "$$marker" && test ! -L "$$marker"; \
	test "$$(/usr/bin/stat -Lc '%u:%g:%a:%F:%h' -- "$$marker")" = '0:0:600:regular file:1'; \
	marker_challenge=; marker_challenge_count=0; \
	while IFS= read -r marker_line; do \
		case "$$marker_line" in challenge=*) \
			marker_challenge="$${marker_line#challenge=}"; \
			marker_challenge_count=$$((marker_challenge_count + 1));; \
		esac; \
	done <"$$marker"; \
	test "$$marker_challenge_count" = 1; \
	[[ "$$marker_challenge" =~ ^[0-9a-f]{64}$$ ]]; \
	expected_build_root="/var/tmp/cast-vm-boot-storage-$${CAST_VM_BOOT_PUBLICATION_EXPECTED_BOOT_ID}-$$marker_challenge"; \
	test "$${CAST_VM_BOOT_PUBLICATION_BUILD_ROOT-}" = "$$expected_build_root"; \
	test "$${CARGO_TARGET_DIR-}" = "$$expected_build_root/target"; \
	test "$${CARGO_HOME-}" = "$$expected_build_root/cargo-home"; \
	case "$${CARGO_TARGET_DIR}" in /run/cast-vm-boot-storage/mount|/run/cast-vm-boot-storage/mount/*) exit 2 ;; esac; \
	for directory in "$$expected_build_root" "$${CARGO_TARGET_DIR}" "$${CARGO_HOME}"; do \
		test -d "$$directory" && test ! -L "$$directory"; \
		test "$$(/usr/bin/stat -Lc '%u:%g:%a:%F' -- "$$directory")" = '0:0:700:directory'; \
	done; \
	develop_profile="$${CAST_VM_BOOT_PUBLICATION_DEVELOP_PROFILE-}"; \
	test "$$develop_profile" = "$$expected_build_root/nix-develop-profile"; \
	test -L "$$develop_profile"; \
	test "$$(/usr/bin/stat -c '%u:%g:%F:%h' -- "$$develop_profile")" = '0:0:symbolic link:1'; \
	develop_profile_store="$$(/usr/bin/readlink -e -- "$$develop_profile")"; \
	case "$$develop_profile_store" in /nix/store/?*) ;; *) exit 2 ;; esac; \
	test -f "$$develop_profile_store" && test ! -L "$$develop_profile_store"; \
	IFS=: read -r profile_owner profile_group profile_mode profile_type \
		<<<"$$(/usr/bin/stat -Lc '%u:%g:%a:%F' -- "$$develop_profile_store")"; \
	test "$$profile_owner" = 0 && test "$$profile_group" = 0; \
	test "$$profile_type" = 'regular file'; \
	(( (8#$$profile_mode & 0022) == 0 )); \
	manifest="$${CAST_VM_BOOT_PUBLICATION_BINARY_MANIFEST-}"; \
	test "$$manifest" = "$$expected_build_root/forge-libtest-manifest-v1"; \
	test -f "$$manifest" && test ! -L "$$manifest"; \
	test "$$(/usr/bin/stat -Lc '%u:%g:%a:%F:%h' -- "$$manifest")" = '0:0:600:regular file:1'; \
	mapfile -t manifest_lines <"$$manifest"; \
	test "$${#manifest_lines[@]}" = 1; \
	IFS=$$'\t' read -r manifest_protocol manifest_source manifest_profile manifest_profile_store \
		manifest_target manifest_executable manifest_sha256 manifest_extra \
		<<<"$${manifest_lines[0]}"; \
	test -z "$${manifest_extra-}"; \
	test "$$manifest_protocol" = protocol=1; \
	test "$$manifest_source" = "source_root=$$source_root"; \
	test "$$manifest_profile" = "develop_profile=$$develop_profile"; \
	test "$$manifest_profile_store" = "develop_profile_store=$$develop_profile_store"; \
	test "$$manifest_target" = "target_root=$${CARGO_TARGET_DIR}"; \
	case "$$manifest_executable" in executable=*) executable="$${manifest_executable#executable=}" ;; *) exit 2 ;; esac; \
	case "$$manifest_sha256" in sha256=*) executable_sha256="$${manifest_sha256#sha256=}" ;; *) exit 2 ;; esac; \
	[[ "$$executable_sha256" =~ ^[0-9a-f]{64}$$ ]]; \
	expected_manifest_line="$$(printf 'protocol=1\tsource_root=%s\tdevelop_profile=%s\tdevelop_profile_store=%s\ttarget_root=%s\texecutable=%s\tsha256=%s' \
		"$$source_root" "$$develop_profile" "$$develop_profile_store" \
		"$${CARGO_TARGET_DIR}" "$$executable" "$$executable_sha256")"; \
	test "$${manifest_lines[0]}" = "$$expected_manifest_line"; \
	test "$$(/usr/bin/stat -Lc '%s' -- "$$manifest")" = "$$(( $${#expected_manifest_line} + 1 ))"; \
	case "$$executable" in "$${CARGO_TARGET_DIR}"/?*) ;; *) exit 2 ;; esac; \
	case "$$executable" in *$$'\t'*|*$$'\n'*) exit 2 ;; esac; \
	test -f "$$executable" && test ! -L "$$executable" && test -x "$$executable"; \
	test "$$executable" = "$$(/usr/bin/readlink -e -- "$$executable")"; \
	IFS=: read -r executable_owner executable_group executable_mode executable_type executable_links \
		<<<"$$(/usr/bin/stat -Lc '%u:%g:%a:%F:%h' -- "$$executable")"; \
	test "$$executable_owner" = 0 && test "$$executable_group" = 0; \
	test "$$executable_type" = 'regular file' && test "$$executable_links" = 1; \
	(( (8#$$executable_mode & 0022) == 0 )); \
	observed_sha256="$$(/usr/bin/sha256sum -- "$$executable")"; \
	observed_sha256="$${observed_sha256%% *}"; \
	test "$$observed_sha256" = "$$executable_sha256"; \
	[[ "$${CAST_VM_BOOT_PUBLICATION_EXPECTED_TARGET_DEVNUM}" =~ ^[0-9]+:[0-9]+$$ ]]; \
	mount_matches=0; \
	while IFS=' ' read -r -a mount_fields; do \
		if [[ "$${mount_fields[4]-}" = /run/cast-vm-boot-storage/mount ]]; then \
			mount_matches=$$((mount_matches + 1)); \
			test "$${mount_fields[2]-}" = "$${CAST_VM_BOOT_PUBLICATION_EXPECTED_TARGET_DEVNUM}"; \
			test "$${mount_fields[3]-}" = /; \
			mount_separator=-1; \
			for ((field = 6; field < $${#mount_fields[@]}; field += 1)); do \
				if [[ "$${mount_fields[$$field]}" = - ]]; then mount_separator=$$field; break; fi; \
			done; \
			test "$$mount_separator" -ge 0; \
			test "$${mount_fields[$$((mount_separator + 1))]-}" = vfat; \
		fi; \
	done </proc/self/mountinfo; \
	test "$$mount_matches" = 1; \
	test_name='linux_fs::tests::descriptor_boot_file_publication::disposable_vm_vfat_publishes_and_revalidates_one_real_leaf'; \
	cd /; \
	test_status=0; \
	"$$executable" "$$test_name" --ignored --exact --test-threads=1 || test_status=$$?; \
	observed_sha256="$$(/usr/bin/sha256sum -- "$$executable")"; \
	observed_sha256="$${observed_sha256%% *}"; \
	test "$$observed_sha256" = "$$executable_sha256"; \
	test "$$test_status" = 0
