#!/bin/sh

set -eu
PATH=/usr/sbin:/usr/bin:/sbin:/bin
LC_ALL=C
export PATH LC_ALL

die() {
    printf 'disposable VM GPT topology runner rejected invocation: %s\n' "$*" >&2
    exit 2
}

required() {
    variable=$1
    eval "value=\${$variable-}"
    [ -n "$value" ] || die "$variable is unset"
    case "$value" in *'
'* | *''*) die "$variable is not one line" ;; esac
    printf '%s\n' "$value"
}

trusted_tool() {
    tool=$1
    [ -f "$tool" ] && [ -x "$tool" ] \
        || die "trusted tool is unavailable: $tool"
    metadata=$(/usr/bin/stat -Lc '%u:%a:%F' -- "$tool") \
        || die "cannot inspect trusted tool: $tool"
    owner=${metadata%%:*}
    remainder=${metadata#*:}
    mode=${remainder%%:*}
    kind=${remainder#*:}
    [ "$owner" = 0 ] && [ "$kind" = 'regular file' ] \
        && [ $((0$mode & 0022)) -eq 0 ] \
        || die "trusted tool metadata is unsafe: $tool"
}

for tool in \
    /usr/bin/id /usr/bin/stat /usr/bin/cat /usr/bin/readlink \
    /usr/bin/sha256sum /usr/bin/timeout /usr/bin/systemd-detect-virt \
    /usr/bin/grep /usr/bin/sed /usr/bin/awk
do
    trusted_tool "$tool"
done

[ "$(/usr/bin/id -u)" = 0 ] || die 'guest root is required'
[ -n "${SSH_CONNECTION-}" ] || die 'remote SSH context is required'
[ -d /sys/firmware/efi ] && [ ! -L /sys/firmware/efi ] \
    || die 'UEFI guest evidence is absent'
[ "${CAST_VM_GPT_TOPOLOGY_CONFIRMATION-}" = disposable-vm-gpt-topology-only ] \
    || die 'dedicated GPT topology confirmation is absent'
[ -z "${CAST_VM_BOOT_PUBLICATION_PARENT-}" ] \
    || die 'legacy whole-device publication parent is forbidden'

kind=$(required CAST_VM_GPT_TOPOLOGY_KIND)
case "$kind" in alias | distinct) ;; *) die 'invalid topology kind' ;; esac
phase=$(required CAST_VM_GPT_TOPOLOGY_PHASE)
case "$phase" in publish | revalidate) ;; *) die 'invalid topology phase' ;; esac

expected_hostname=$(required CAST_VM_BOOT_PUBLICATION_EXPECTED_HOSTNAME)
expected_machine_id=$(required CAST_VM_BOOT_PUBLICATION_EXPECTED_MACHINE_ID)
expected_boot_id=$(required CAST_VM_BOOT_PUBLICATION_EXPECTED_BOOT_ID)
expected_virtualization=$(required CAST_VM_BOOT_PUBLICATION_EXPECTED_VIRTUALIZATION)
expected_ssh_sha256=$(required CAST_VM_BOOT_PUBLICATION_EXPECTED_SSH_SHA256)
expected_target_disk=$(required CAST_VM_BOOT_PUBLICATION_EXPECTED_TARGET_DISK)
expected_target_stable_path=$(required CAST_VM_BOOT_PUBLICATION_EXPECTED_TARGET_STABLE_PATH)
expected_target_diskseq=$(required CAST_VM_BOOT_PUBLICATION_EXPECTED_TARGET_DISKSEQ)
expected_target_bytes=$(required CAST_VM_BOOT_PUBLICATION_EXPECTED_TARGET_BYTES)
[ "$(/usr/bin/cat /proc/sys/kernel/hostname)" = "$expected_hostname" ] \
    || die 'guest hostname changed'
[ "$(/usr/bin/cat /etc/machine-id)" = "$expected_machine_id" ] \
    || die 'guest machine ID changed'
[ "$(/usr/bin/cat /proc/sys/kernel/random/boot_id)" = "$expected_boot_id" ] \
    || die 'guest boot ID changed'
[ "$(/usr/bin/systemd-detect-virt --vm)" = "$expected_virtualization" ] \
    || die 'guest virtualization kind changed'
observed_ssh_sha256=$(printf '%s' "$SSH_CONNECTION" | /usr/bin/sha256sum)
observed_ssh_sha256=${observed_ssh_sha256%% *}
[ "$observed_ssh_sha256" = "$expected_ssh_sha256" ] \
    || die 'SSH connection identity changed'

marker=$(required CAST_VM_BOOT_PUBLICATION_CONSUMED_MARKER)
[ "$marker" = /run/cast-vm-boot-storage/authorization-v1.consumed ] \
    || die 'consumed marker path is not fixed'
[ -f "$marker" ] && [ ! -L "$marker" ] \
    || die 'consumed marker is unavailable'
[ "$(/usr/bin/stat -Lc '%u:%g:%a:%F:%h' -- "$marker")" \
    = '0:0:600:regular file:1' ] \
    || die 'consumed marker metadata is unsafe'
[ "$(/usr/bin/grep -Fxc 'protocol=2' "$marker")" = 1 ] \
    || die 'consumed marker protocol is not GPT-specific'
[ "$(/usr/bin/grep -Fxc 'campaign_profile=gpt-boot-topologies' "$marker")" = 1 ] \
    || die 'consumed marker profile is not GPT-specific'
[ "$(/usr/bin/grep -Fxc "hostname=$expected_hostname" "$marker")" = 1 ] \
    && [ "$(/usr/bin/grep -Fxc "machine_id=$expected_machine_id" "$marker")" = 1 ] \
    && [ "$(/usr/bin/grep -Fxc "boot_id=$expected_boot_id" "$marker")" = 1 ] \
    && [ "$(/usr/bin/grep -Fxc "virtualization=$expected_virtualization" "$marker")" = 1 ] \
    && [ "$(/usr/bin/grep -Fxc "ssh_connection_sha256=$expected_ssh_sha256" "$marker")" = 1 ] \
    || die 'consumed marker guest identity binding changed'
[ "$(/usr/bin/grep -Fxc "target_disk=$expected_target_disk" "$marker")" = 1 ] \
    && [ "$(/usr/bin/grep -Fxc "target_stable_path=$expected_target_stable_path" "$marker")" = 1 ] \
    && [ "$(/usr/bin/grep -Fxc "target_diskseq=$expected_target_diskseq" "$marker")" = 1 ] \
    && [ "$(/usr/bin/grep -Fxc "target_bytes=$expected_target_bytes" "$marker")" = 1 ] \
    || die 'consumed marker target-disk binding changed'
challenge=$(/usr/bin/sed -n 's/^challenge=//p' "$marker")
case "$challenge" in
    *[!0-9a-f]* | '') die 'consumed marker challenge is invalid' ;;
esac
[ "${#challenge}" = 64 ] || die 'consumed marker challenge length is invalid'

build_root=$(required CAST_VM_BOOT_PUBLICATION_BUILD_ROOT)
[ "$build_root" = "/var/tmp/cast-vm-boot-storage-$expected_boot_id-$challenge" ] \
    || die 'private build root is not marker-bound'
target_root=$(required CARGO_TARGET_DIR)
cargo_home=$(required CARGO_HOME)
[ "$target_root" = "$build_root/target" ] \
    && [ "$cargo_home" = "$build_root/cargo-home" ] \
    || die 'Cargo paths escaped the private build root'
installation=$(required CAST_VM_GPT_TOPOLOGY_INSTALLATION)
[ "$installation" = "$build_root/topology-installation" ] \
    || die 'topology installation escaped the private build root'
[ -d "$installation" ] && [ ! -L "$installation" ] \
    || die 'topology installation is unavailable'

source_root=$(required CAST_VM_BOOT_PUBLICATION_SOURCE_ROOT)
case "$source_root" in /nix/store/*-source) ;; *) die 'source root is not immutable Nix storage' ;; esac
[ "$source_root" = "$(/usr/bin/readlink -e -- "$source_root")" ] \
    || die 'source root does not resolve exactly'
[ "$(pwd -P)" = "$source_root" ] \
    || die 'runner current directory is not the immutable source root'
develop_profile=$(required CAST_VM_BOOT_PUBLICATION_DEVELOP_PROFILE)
[ "$develop_profile" = "$build_root/nix-develop-profile" ] \
    && [ -L "$develop_profile" ] \
    || die 'develop profile is not the fixed private symlink'
profile_store=$(/usr/bin/readlink -e -- "$develop_profile") \
    || die 'cannot resolve develop profile'
case "$profile_store" in /nix/store/*) ;; *) die 'develop profile escaped Nix storage' ;; esac
[ -f "$profile_store" ] && [ ! -L "$profile_store" ] \
    || die 'develop profile target is not a regular file'

manifest=$(required CAST_VM_BOOT_PUBLICATION_BINARY_MANIFEST)
[ "$manifest" = "$build_root/forge-libtest-manifest-v1" ] \
    || die 'libtest manifest path escaped the private build root'
[ -f "$manifest" ] && [ ! -L "$manifest" ] \
    || die 'libtest manifest is unavailable'
[ "$(/usr/bin/stat -Lc '%u:%g:%a:%F:%h' -- "$manifest")" \
    = '0:0:600:regular file:1' ] \
    || die 'libtest manifest metadata is unsafe'
[ "$(/usr/bin/awk 'END { print NR }' "$manifest")" = 1 ] \
    || die 'libtest manifest is not exactly one line'
manifest_line=$(/usr/bin/cat "$manifest")
old_ifs=$IFS
IFS=$(printf '\t')
set -- $manifest_line
IFS=$old_ifs
[ "$#" = 7 ] || die 'libtest manifest field count changed'
[ "$1" = protocol=1 ] \
    && [ "$2" = "source_root=$source_root" ] \
    && [ "$3" = "develop_profile=$develop_profile" ] \
    && [ "$4" = "develop_profile_store=$profile_store" ] \
    && [ "$5" = "target_root=$target_root" ] \
    || die 'libtest manifest binding changed'
case "$6" in executable=*) executable=${6#executable=} ;; *) die 'manifest executable is absent' ;; esac
case "$7" in sha256=*) executable_sha256=${7#sha256=} ;; *) die 'manifest SHA-256 is absent' ;; esac
case "$executable" in "$target_root"/*) ;; *) die 'libtest escaped target root' ;; esac
case "$executable_sha256" in
    *[!0-9a-f]* | '') die 'manifest SHA-256 is invalid' ;;
esac
[ "${#executable_sha256}" = 64 ] || die 'manifest SHA-256 length is invalid'
[ -f "$executable" ] && [ ! -L "$executable" ] && [ -x "$executable" ] \
    || die 'libtest executable is unsafe'
[ "$executable" = "$(/usr/bin/readlink -e -- "$executable")" ] \
    || die 'libtest executable does not resolve exactly'
observed_sha256=$(/usr/bin/sha256sum -- "$executable")
observed_sha256=${observed_sha256%% *}
[ "$observed_sha256" = "$executable_sha256" ] \
    || die 'libtest executable digest changed before invocation'

esp_mount=$(required CAST_VM_GPT_TOPOLOGY_ESP_MOUNT)
esp_devnum=$(required CAST_VM_GPT_TOPOLOGY_ESP_DEVNUM)
esp_partuuid=$(required CAST_VM_GPT_TOPOLOGY_ESP_PARTUUID)
[ "$esp_mount" = /run/cast-vm-boot-storage/mount/esp ] \
    || die 'ESP mount selector is not fixed'
case "$esp_devnum" in *:*) ;; *) die 'ESP device number is invalid' ;; esac
case "$esp_partuuid" in *-*-*-*-*) ;; *) die 'ESP PARTUUID is invalid' ;; esac
if [ "$kind" = distinct ]; then
    [ "$(required CAST_VM_GPT_TOPOLOGY_XBOOTLDR_MOUNT)" \
        = /run/cast-vm-boot-storage/mount/xbootldr ] \
        || die 'XBOOTLDR mount selector is not fixed'
    required CAST_VM_GPT_TOPOLOGY_XBOOTLDR_DEVNUM >/dev/null
    required CAST_VM_GPT_TOPOLOGY_XBOOTLDR_PARTUUID >/dev/null
fi

test_name='client::disposable_vm_gpt_topology_tests::disposable_vm_authenticates_gpt_boot_topology_and_publishes_real_leaves'
cd /
test_status=0
/usr/bin/timeout --signal=TERM --kill-after=5s 180s \
    "$executable" "$test_name" --ignored --exact --test-threads=1 \
    || test_status=$?
observed_sha256=$(/usr/bin/sha256sum -- "$executable")
observed_sha256=${observed_sha256%% *}
[ "$observed_sha256" = "$executable_sha256" ] \
    || die 'libtest executable digest changed after invocation'
[ "$test_status" = 0 ] || exit "$test_status"
