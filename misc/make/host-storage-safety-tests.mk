SHELL := /bin/bash

HOST_STORAGE_SAFETY_TOP_DIR := $(abspath $(dir $(lastword $(MAKEFILE_LIST)))/../..)

.PHONY: host-storage-safety-test

host-storage-safety-test:
	@set -euo pipefail; \
	timeout 10s rg --version >/dev/null; \
	timeout 10s find --version >/dev/null; \
	timeout 10s mkdir -p "$(HOST_STORAGE_SAFETY_TOP_DIR)/target"; \
	manifest="$$( timeout 10s mktemp "$(HOST_STORAGE_SAFETY_TOP_DIR)/target/host-storage-safety-files.XXXXXXXXXXXX" )"; \
	findings="$$( timeout 10s mktemp "$(HOST_STORAGE_SAFETY_TOP_DIR)/target/host-storage-safety-findings.XXXXXXXXXXXX" )"; \
	safe_probe="$$( timeout 10s mktemp "$(HOST_STORAGE_SAFETY_TOP_DIR)/target/host-storage-safety-safe.XXXXXXXXXXXX" )"; \
	unsafe_probe="$$( timeout 10s mktemp "$(HOST_STORAGE_SAFETY_TOP_DIR)/target/host-storage-safety-unsafe.XXXXXXXXXXXX" )"; \
	probe_findings="$$( timeout 10s mktemp "$(HOST_STORAGE_SAFETY_TOP_DIR)/target/host-storage-safety-probe-findings.XXXXXXXXXXXX" )"; \
	trap 'timeout 10s rm -f "$$manifest" "$$findings" "$$safe_probe" "$$unsafe_probe" "$$probe_findings"' EXIT; \
	command_pattern='(?x)(?:^|;[[:space:]]*|&&[[:space:]]*|\|\|[[:space:]]*|\|[[:space:]]+|\$$\([[:space:]]*)[[:space:]]*@?[-+]?[[:space:]]*(?:(?:if|then|elif|while|until|do|!|command|exec|sudo|doas)[[:space:]]+)*(?:(?:env)[[:space:]]+(?:[A-Za-z_][A-Za-z0-9_]*=[^[:space:]]+[[:space:]]+)*)?(?:(?:timeout)[[:space:]]+(?:--?[A-Za-z0-9_=.,-]+[[:space:]]+)*[0-9]+(?:\.[0-9]+)?[smhd]?[[:space:]]+)?(?:/[A-Za-z0-9_./-]+/)?(?:mount|umount|losetup|mkfs(?:\.[A-Za-z0-9_-]+)?|fsck(?:\.[A-Za-z0-9_-]+)?|e2fsck|resize2fs|tune2fs|fatlabel|dosfslabel|fdisk|sfdisk|cfdisk|parted|gdisk|sgdisk|wipefs|blkdiscard|blockdev|partprobe|kpartx|dmsetup|cryptsetup|pvcreate|vgcreate|lvcreate|mkswap|swapon|swapoff|blkid|lsblk|findmnt|udevadm(?![[:space:]]+verify(?=[[:space:];&|]|$$))|smartctl|nvme|hdparm)(?=[[:space:];&|]|$$)'; \
	rust_command_pattern='(?x)(?:std::process::|process::)?Command::new\([[:space:]]*"(?:/(?:usr/)?s?bin/)?(?:mount|umount|losetup|mkfs(?:\.[A-Za-z0-9_-]+)?|fsck(?:\.[A-Za-z0-9_-]+)?|e2fsck|resize2fs|tune2fs|fatlabel|dosfslabel|fdisk|sfdisk|cfdisk|parted|gdisk|sgdisk|wipefs|blkdiscard|blockdev|partprobe|kpartx|dmsetup|cryptsetup|pvcreate|vgcreate|lvcreate|mkswap|swapon|swapoff|blkid|lsblk|findmnt|udevadm|smartctl|nvme|hdparm)"[[:space:]]*\)'; \
	raw_device_pattern='(?x)/dev/(?:sd[a-z][0-9]*|hd[a-z][0-9]*|vd[a-z][0-9]*|xvd[a-z][0-9]*|nvme[0-9]+n[0-9]+(?:p[0-9]+)?|mmcblk[0-9]+(?:p[0-9]+)?|loop[0-9]+|md[0-9]+|dm-[0-9]+|nbd[0-9]+|zram[0-9]+|rbd[0-9]+|bcache[0-9]+|zd[0-9]+|sr[0-9]+|root|mapper/[A-Za-z0-9_.+:-]+|disk/by-(?:id|path|uuid|partuuid|label|partlabel)/[A-Za-z0-9_.+,:@=-]+)(?=$$|[^[:alnum:]_./:+@=-])'; \
	host_boot_pattern='(?x)(?<![[:alnum:]_./])(?<![$$})])/(?:boot|efi|esp)(?:/|(?![[:alnum:]_.+-]))'; \
	sys_redirect_pattern='(?x)(?:>>?|2>>?|&>)[[:space:]]*["\x27]?/sys(?:/|(?![[:alnum:]_.+-]))'; \
	scan_file() { \
		local file="$$1"; \
		local output="$$2"; \
		local scan_status=0; \
		if [[ "$$file" == *.rs ]]; then \
			timeout 10s rg --pcre2 -nH \
				-e "$$rust_command_pattern" \
				-e "$$raw_device_pattern" \
				-e "$$host_boot_pattern" \
				-- "$$file" >> "$$output" || scan_status="$$?"; \
		else \
			timeout 10s rg --pcre2 -nH \
				-e "$$command_pattern" \
				-e "$$rust_command_pattern" \
				-e "$$raw_device_pattern" \
				-e "$$host_boot_pattern" \
				-e "$$sys_redirect_pattern" \
				-- "$$file" >> "$$output" || scan_status="$$?"; \
		fi; \
		case "$$scan_status" in \
			0|1) ;; \
			*) timeout 10s printf 'host-storage safety scan failed for %s (status %s)\n' "$$file" "$$scan_status" >&2; return "$$scan_status" ;; \
		esac; \
	}; \
	{ \
		timeout 10s printf '%s\0' "$(HOST_STORAGE_SAFETY_TOP_DIR)/Makefile"; \
		timeout 20s find "$(HOST_STORAGE_SAFETY_TOP_DIR)/misc/make" -maxdepth 1 -type f -name '*.mk' ! -name 'host-storage-safety-tests.mk' -print0; \
		timeout 20s find "$(HOST_STORAGE_SAFETY_TOP_DIR)/misc/scripts" -type f -name '*.sh' -print0; \
		if timeout 10s test -d "$(HOST_STORAGE_SAFETY_TOP_DIR)/tests"; then \
			timeout 20s find "$(HOST_STORAGE_SAFETY_TOP_DIR)/tests" -type f \( -name '*.rs' -o -name '*.sh' -o -name '*.mk' -o -name 'Makefile' \) -print0; \
		fi; \
		timeout 20s find "$(HOST_STORAGE_SAFETY_TOP_DIR)/crates" -type f \( -path '*/tests/*.rs' -o -name 'tests.rs' -o -name '*_tests.rs' \) -print0; \
		cfg_status=0; \
		timeout 20s rg -l0 '#\[cfg\(test\)\]' "$(HOST_STORAGE_SAFETY_TOP_DIR)/crates" --glob '*.rs' || cfg_status="$$?"; \
		case "$$cfg_status" in 0|1) ;; *) exit "$$cfg_status" ;; esac; \
	} | timeout 30s sort -zu > "$$manifest"; \
	timeout 10s test -s "$$manifest"; \
	timeout 10s printf '%s\n' \
		'echo "container-mount-boundary-test"' \
		'let call = nix::mount::mount;' \
		'let source = b"/dev/disk\\134name";' \
		'dd if=/dev/zero bs=1 count=1 status=none' \
		'let payload = "usr/lib/systemd/boot/efi";' \
		'let link = "/sys/dev/block/8:1";' \
		'udevadm verify "$$fixture"' \
		'rg -n "(mount|umount)" parser.rs' > "$$safe_probe"; \
	scan_file "$$safe_probe" "$$probe_findings"; \
	if timeout 10s test -s "$$probe_findings"; then \
		timeout 10s printf 'host-storage matcher rejected its safe probes:\n' >&2; \
		timeout 10s sed -n '1,80p' "$$probe_findings" >&2; \
		exit 1; \
	fi; \
	timeout 10s printf '%s\n' \
		'mount /tmp/source /tmp/target' \
		'printf ready; umount /tmp/target' \
		'lsblk --json' \
		'let disk = "/dev/nvme0n1p1";' \
		'let esp = "/boot";' \
		'printf x > /sys/kernel/example' \
		'Command::new("wipefs");' > "$$unsafe_probe"; \
	: > "$$probe_findings"; \
	scan_file "$$unsafe_probe" "$$probe_findings"; \
	timeout 10s test "$$( timeout 10s wc -l < "$$probe_findings" )" = 7; \
	while IFS= read -r -d '' file; do \
		scan_file "$$file" "$$findings"; \
	done < "$$manifest"; \
	if timeout 10s test -s "$$findings"; then \
		timeout 10s printf 'host-storage safety violations found:\n' >&2; \
		timeout 10s sed -n '1,160p' "$$findings" >&2; \
		exit 1; \
	fi; \
	timeout 10s printf 'host-storage safety gate passed (%s source files inspected)\n' "$$( timeout 10s tr -cd '\0' < "$$manifest" | timeout 10s wc -c )"
