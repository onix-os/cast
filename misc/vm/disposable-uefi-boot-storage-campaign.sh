#!/bin/sh

set -eu
PATH=/usr/sbin:/usr/bin:/sbin:/bin
LC_ALL=C
export PATH LC_ALL
runtime_root=/run/cast-vm-boot-storage
authorization_marker=$runtime_root/authorization-v1
consumed_marker=$runtime_root/authorization-v1.consumed
campaign_lock=$runtime_root/campaign-v1.lock
mount_root=$runtime_root/mount
challenge_max_age_seconds=300
root=$(CDPATH= cd -- "$(dirname -- "$0")/../.." && pwd -P)
carriage_return=$(printf '\r')
usage() {
    cat >&2 <<'EOF'
usage: disposable-uefi-boot-storage-campaign.sh <challenge|admit|campaign> \
  --expected-hostname NAME \
  --expected-machine-id 32-LOWER-HEX \
  --expected-boot-id UUID \
  --expected-virtualization KIND \
  --expected-commit GIT-OID \
  --target-disk CANONICAL-WHOLE-DISK \
  --target-stable-path STABLE-ID-OR-PATH-SYMLINK \
  --target-diskseq DECIMAL-DISKSEQ \
  --target-bytes DECIMAL-BYTES \
  --expected-root-device CANONICAL-BLOCK-DEVICE \
  --expected-live-esp-device CANONICAL-BLOCK-DEVICE \
  --expected-live-esp-mountpoint /boot-or-efi-mountpoint \
  --filesystem-label UPPERCASE-LABEL \
  --publication-parent SAFE-RELATIVE-PATH \
  --snapshot-confirmation snapshot-ready:BOOT-ID:GIT-OID \
  --remote-confirmation disposable-vm-remote-only \
  --cooperative-root-confirmation cooperative-guest-root-no-hotplug \
  [--campaign-profile gpt-boot-topologies] \
  [--challenge 64-LOWER-HEX] \
  [--destructive-confirmation PROFILE-BOUND-CONFIRMATION]

The challenge and admit modes do not mutate the target disk. Campaign consumes
the fresh challenge once, formats only the exact admitted whole disk as VFAT,
creates and persistence-checks the declared parent below the private /run
mount, and unmounts it. It never reboots the guest.

The optional gpt-boot-topologies profile is a distinct protocol selected only
by its dedicated wrapper. It repartitions the exact admitted disposable disk;
its confirmation is
repartition-gpt:STABLE-PATH:BYTES:DISKSEQ:BOOT-ID:gpt-boot-topologies.
EOF
}
die() {
    printf 'disposable VM boot-storage admission failed: %s\n' "$*" >&2
    exit 1
}
bad_usage() {
    printf 'disposable VM boot-storage argument error: %s\n' "$*" >&2
    usage
    exit 2
}
require_value() {
    option=$1
    count=$2
    [ "$count" -ge 2 ] || bad_usage "$option requires one value"
}
require_pattern() {
    label=$1
    value=$2
    pattern=$3
    case "$value" in
        *'
'* | *"$carriage_return"*) bad_usage "$label must be exactly one text line" ;;
    esac
    printf '%s\n' "$value" | grep -Eq "$pattern" \
        || bad_usage "$label has an invalid value"
}
require_canonical_device_path() {
    label=$1
    value=$2
    require_pattern "$label" "$value" '^/dev/[A-Za-z0-9._+-]+$'
}
validate_publication_parent() {
    case "$publication_parent" in
        '' | /* | */ | *'//'*)
            bad_usage '--publication-parent must be a nonempty relative path'
            ;;
    esac
    require_pattern '--publication-parent' "$publication_parent" \
        '^[A-Za-z0-9._+@=/:-]+$'
    old_ifs=$IFS
    IFS=/
    set -- $publication_parent
    IFS=$old_ifs
    [ "$#" -ge 1 ] || bad_usage '--publication-parent is empty'
    for component do
        case "$component" in
            '' | . | ..)
                bad_usage '--publication-parent contains an unsafe component'
                ;;
        esac
    done
}

mode=${1-}
case "$mode" in
    challenge | admit | campaign) shift ;;
    *) usage; exit 2 ;;
esac

expected_hostname=
expected_machine_id=
expected_boot_id=
expected_virtualization=
expected_commit=
target_disk=
target_stable_path=
target_diskseq=
target_bytes=
expected_root_device=
expected_live_esp_device=
expected_live_esp_mountpoint=
filesystem_label=
publication_parent=
snapshot_confirmation=
remote_confirmation=
cooperative_root_confirmation=
campaign_profile=
challenge=
destructive_confirmation=

seen_expected_hostname=0
seen_expected_machine_id=0
seen_expected_boot_id=0
seen_expected_virtualization=0
seen_expected_commit=0
seen_target_disk=0
seen_target_stable_path=0
seen_target_diskseq=0
seen_target_bytes=0
seen_expected_root_device=0
seen_expected_live_esp_device=0
seen_expected_live_esp_mountpoint=0
seen_filesystem_label=0
seen_publication_parent=0
seen_snapshot_confirmation=0
seen_remote_confirmation=0
seen_cooperative_root_confirmation=0
seen_campaign_profile=0
seen_challenge=0
seen_destructive_confirmation=0

while [ "$#" -gt 0 ]; do
    option=$1
    case "$option" in
        --expected-hostname)
            require_value "$option" "$#"
            [ "$seen_expected_hostname" -eq 0 ] || bad_usage "$option is duplicated"
            seen_expected_hostname=1
            expected_hostname=$2
            shift 2
            ;;
        --expected-machine-id)
            require_value "$option" "$#"
            [ "$seen_expected_machine_id" -eq 0 ] || bad_usage "$option is duplicated"
            seen_expected_machine_id=1
            expected_machine_id=$2
            shift 2
            ;;
        --expected-boot-id)
            require_value "$option" "$#"
            [ "$seen_expected_boot_id" -eq 0 ] || bad_usage "$option is duplicated"
            seen_expected_boot_id=1
            expected_boot_id=$2
            shift 2
            ;;
        --expected-virtualization)
            require_value "$option" "$#"
            [ "$seen_expected_virtualization" -eq 0 ] || bad_usage "$option is duplicated"
            seen_expected_virtualization=1
            expected_virtualization=$2
            shift 2
            ;;
        --expected-commit)
            require_value "$option" "$#"
            [ "$seen_expected_commit" -eq 0 ] || bad_usage "$option is duplicated"
            seen_expected_commit=1
            expected_commit=$2
            shift 2
            ;;
        --target-disk)
            require_value "$option" "$#"
            [ "$seen_target_disk" -eq 0 ] || bad_usage "$option is duplicated"
            seen_target_disk=1
            target_disk=$2
            shift 2
            ;;
        --target-stable-path)
            require_value "$option" "$#"
            [ "$seen_target_stable_path" -eq 0 ] || bad_usage "$option is duplicated"
            seen_target_stable_path=1
            target_stable_path=$2
            shift 2
            ;;
        --target-diskseq)
            require_value "$option" "$#"
            [ "$seen_target_diskseq" -eq 0 ] || bad_usage "$option is duplicated"
            seen_target_diskseq=1
            target_diskseq=$2
            shift 2
            ;;
        --target-bytes)
            require_value "$option" "$#"
            [ "$seen_target_bytes" -eq 0 ] || bad_usage "$option is duplicated"
            seen_target_bytes=1
            target_bytes=$2
            shift 2
            ;;
        --expected-root-device)
            require_value "$option" "$#"
            [ "$seen_expected_root_device" -eq 0 ] || bad_usage "$option is duplicated"
            seen_expected_root_device=1
            expected_root_device=$2
            shift 2
            ;;
        --expected-live-esp-device)
            require_value "$option" "$#"
            [ "$seen_expected_live_esp_device" -eq 0 ] || bad_usage "$option is duplicated"
            seen_expected_live_esp_device=1
            expected_live_esp_device=$2
            shift 2
            ;;
        --expected-live-esp-mountpoint)
            require_value "$option" "$#"
            [ "$seen_expected_live_esp_mountpoint" -eq 0 ] || bad_usage "$option is duplicated"
            seen_expected_live_esp_mountpoint=1
            expected_live_esp_mountpoint=$2
            shift 2
            ;;
        --filesystem-label)
            require_value "$option" "$#"
            [ "$seen_filesystem_label" -eq 0 ] || bad_usage "$option is duplicated"
            seen_filesystem_label=1
            filesystem_label=$2
            shift 2
            ;;
        --publication-parent)
            require_value "$option" "$#"
            [ "$seen_publication_parent" -eq 0 ] || bad_usage "$option is duplicated"
            seen_publication_parent=1
            publication_parent=$2
            shift 2
            ;;
        --snapshot-confirmation)
            require_value "$option" "$#"
            [ "$seen_snapshot_confirmation" -eq 0 ] || bad_usage "$option is duplicated"
            seen_snapshot_confirmation=1
            snapshot_confirmation=$2
            shift 2
            ;;
        --remote-confirmation)
            require_value "$option" "$#"
            [ "$seen_remote_confirmation" -eq 0 ] || bad_usage "$option is duplicated"
            seen_remote_confirmation=1
            remote_confirmation=$2
            shift 2
            ;;
        --cooperative-root-confirmation)
            require_value "$option" "$#"
            [ "$seen_cooperative_root_confirmation" -eq 0 ] \
                || bad_usage "$option is duplicated"
            seen_cooperative_root_confirmation=1
            cooperative_root_confirmation=$2
            shift 2
            ;;
        --campaign-profile)
            require_value "$option" "$#"
            [ "$seen_campaign_profile" -eq 0 ] \
                || bad_usage "$option is duplicated"
            seen_campaign_profile=1
            campaign_profile=$2
            shift 2
            ;;
        --challenge)
            require_value "$option" "$#"
            [ "$seen_challenge" -eq 0 ] || bad_usage "$option is duplicated"
            seen_challenge=1
            challenge=$2
            shift 2
            ;;
        --destructive-confirmation)
            require_value "$option" "$#"
            [ "$seen_destructive_confirmation" -eq 0 ] \
                || bad_usage "$option is duplicated"
            seen_destructive_confirmation=1
            destructive_confirmation=$2
            shift 2
            ;;
        *) bad_usage "unknown option: $option" ;;
    esac
done

for seen in \
    "$seen_expected_hostname" "$seen_expected_machine_id" \
    "$seen_expected_boot_id" "$seen_expected_virtualization" \
    "$seen_expected_commit" "$seen_target_disk" "$seen_target_stable_path" \
    "$seen_target_diskseq" "$seen_target_bytes" "$seen_expected_root_device" \
    "$seen_expected_live_esp_device" "$seen_expected_live_esp_mountpoint" \
    "$seen_filesystem_label" "$seen_publication_parent" \
    "$seen_snapshot_confirmation" "$seen_remote_confirmation" \
    "$seen_cooperative_root_confirmation"
do
    [ "$seen" -eq 1 ] || bad_usage 'every common option is mandatory'
done

case "$mode" in
    challenge)
        [ "$seen_challenge" -eq 0 ] \
            || bad_usage '--challenge is not accepted while creating a challenge'
        [ "$seen_destructive_confirmation" -eq 0 ] \
            || bad_usage '--destructive-confirmation is campaign-only'
        ;;
    admit)
        [ "$seen_challenge" -eq 1 ] || bad_usage '--challenge is required'
        [ "$seen_destructive_confirmation" -eq 0 ] \
            || bad_usage '--destructive-confirmation is campaign-only'
        ;;
    campaign)
        [ "$seen_challenge" -eq 1 ] || bad_usage '--challenge is required'
        [ "$seen_destructive_confirmation" -eq 1 ] \
            || bad_usage '--destructive-confirmation is required'
        ;;
esac

require_pattern '--expected-hostname' "$expected_hostname" \
    '^[A-Za-z0-9][A-Za-z0-9._-]*$'
require_pattern '--expected-machine-id' "$expected_machine_id" \
    '^[0-9a-f]{32}$'
require_pattern '--expected-boot-id' "$expected_boot_id" \
    '^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$'
require_pattern '--expected-virtualization' "$expected_virtualization" \
    '^[a-z0-9][a-z0-9_-]*$'
require_pattern '--expected-commit' "$expected_commit" \
    '^([0-9a-f]{40}|[0-9a-f]{64})$'
require_canonical_device_path '--target-disk' "$target_disk"
case "$target_stable_path" in
    /dev/disk/by-id/*) target_stable_name=${target_stable_path#/dev/disk/by-id/} ;;
    /dev/disk/by-path/*) target_stable_name=${target_stable_path#/dev/disk/by-path/} ;;
    *) bad_usage '--target-stable-path must be below /dev/disk/by-id or /dev/disk/by-path' ;;
esac
case "$target_stable_name" in
    '' | */* | *[!A-Za-z0-9._:+@=-]*)
        bad_usage '--target-stable-path has an unsafe final component'
        ;;
esac
require_pattern '--target-diskseq' "$target_diskseq" '^[1-9][0-9]*$'
require_pattern '--target-bytes' "$target_bytes" '^[1-9][0-9]*$'
[ "$(printf '%s' "$target_bytes" | wc -c)" -le 19 ] \
    || bad_usage '--target-bytes exceeds the supported decimal range'
require_canonical_device_path '--expected-root-device' "$expected_root_device"
require_canonical_device_path '--expected-live-esp-device' \
    "$expected_live_esp_device"
case "$expected_live_esp_mountpoint" in
    /boot | /boot/efi | /efi | /esp) ;;
    *)
        bad_usage '--expected-live-esp-mountpoint must be /boot, /boot/efi, /efi, or /esp'
        ;;
esac
require_pattern '--filesystem-label' "$filesystem_label" '^[A-Z0-9_-]+$'
label_length=$(printf '%s' "$filesystem_label" | wc -c)
[ "$label_length" -ge 1 ] && [ "$label_length" -le 11 ] \
    || bad_usage '--filesystem-label must contain 1 through 11 bytes'
validate_publication_parent
[ "$snapshot_confirmation" = "snapshot-ready:$expected_boot_id:$expected_commit" ] \
    || bad_usage '--snapshot-confirmation does not bind this boot and commit'
[ "$remote_confirmation" = disposable-vm-remote-only ] \
    || bad_usage '--remote-confirmation must be disposable-vm-remote-only'
[ "$cooperative_root_confirmation" = cooperative-guest-root-no-hotplug ] \
    || bad_usage '--cooperative-root-confirmation must be cooperative-guest-root-no-hotplug'
if [ "$seen_campaign_profile" -eq 1 ]; then
    [ "$campaign_profile" = gpt-boot-topologies ] \
        || bad_usage '--campaign-profile must be gpt-boot-topologies'
fi
if [ "$seen_challenge" -eq 1 ]; then
    require_pattern '--challenge' "$challenge" '^[0-9a-f]{64}$'
fi
if [ "$mode" = campaign ]; then
    if [ "$campaign_profile" = gpt-boot-topologies ]; then
        expected_destructive_confirmation="repartition-gpt:$target_stable_path:$target_bytes:$target_diskseq:$expected_boot_id:gpt-boot-topologies"
    else
        expected_destructive_confirmation="erase:$target_stable_path:$target_bytes:$target_diskseq:$expected_boot_id"
    fi
    [ "$destructive_confirmation" = "$expected_destructive_confirmation" ] \
        || bad_usage '--destructive-confirmation does not bind the exact target, size, and boot'
fi

require_regular_root_file() {
    path=$1
    label=$2
    [ ! -L "$path" ] || die "$label is a symlink"
    [ -f "$path" ] || die "$label is not a regular file"
    owner=$(stat -c '%u' "$path") || die "cannot inspect $label ownership"
    [ "$owner" = 0 ] || die "$label is not root-owned"
}

read_one_line() {
    path=$1
    label=$2
    require_regular_root_file "$path" "$label"
    physical_lines=$(awk 'END { print NR }' "$path") \
        || die "cannot count $label lines"
    [ "$physical_lines" = 1 ] || die "$label must contain exactly one physical line"
    value=$(cat "$path") || die "cannot read $label"
    case "$value" in
        '' | *'
'* | *"$carriage_return"*) die "$label is empty or contains invalid line bytes" ;;
    esac
    printf '%s\n' "$value"
}

command_path() {
    name=$1
    resolved=$(command -v "$name") || die "required command is unavailable: $name"
    case "$resolved" in
        /*) ;;
        *) die "required command did not resolve absolutely: $name" ;;
    esac
    [ ! -L "$resolved" ] || resolved=$(readlink -f -- "$resolved") \
        || die "cannot resolve required command: $name"
    [ -f "$resolved" ] && [ -x "$resolved" ] \
        || die "required command is unsafe: $resolved"
    command_owner=$(stat -Lc '%u' "$resolved") \
        || die "cannot inspect required command ownership: $resolved"
    command_mode=$(stat -Lc '%a' "$resolved") \
        || die "cannot inspect required command mode: $resolved"
    require_pattern 'required command mode' "$command_mode" '^[0-7]{3,4}$'
    [ "$command_owner" = 0 ] && [ $((0$command_mode & 0022)) -eq 0 ] \
        || die "required command is not trusted root-owned code: $resolved"
    printf '%s\n' "$resolved"
}

verify_init_mount_namespace() {
    self_mount_namespace=$(readlink /proc/self/ns/mnt) \
        || die 'cannot inspect the campaign mount namespace'
    init_mount_namespace=$(readlink /proc/1/ns/mnt) \
        || die 'cannot inspect the init mount namespace'
    [ "$self_mount_namespace" = "$init_mount_namespace" ] \
        || die 'effect phase must run in the guest init mount namespace'
}

current_uptime_seconds() {
    uptime=$(cat /proc/uptime) || die 'cannot read kernel uptime'
    uptime=${uptime%% *}
    uptime=${uptime%%.*}
    require_pattern 'kernel uptime' "$uptime" '^[0-9]+$'
    printf '%s\n' "$uptime"
}

verify_guest_identity() {
    [ "$(id -u)" = 0 ] || die 'root authority is required inside the guest'
    [ -n "${SSH_CONNECTION-}" ] \
        || die 'the campaign must run in an explicitly remote SSH session'
    case "$SSH_CONNECTION" in
        *'
'* | *"$carriage_return"*) die 'SSH_CONNECTION must be one line' ;;
    esac
    case "$root/" in
        /tmp/* | /run/* | /dev/shm/*)
            die 'the checkout must live on persistent guest storage'
            ;;
    esac
    [ -d /sys/firmware/efi ] && [ ! -L /sys/firmware/efi ] \
        || die 'the guest did not boot through UEFI'

    actual_hostname=$(read_one_line /proc/sys/kernel/hostname 'kernel hostname')
    [ "$actual_hostname" = "$expected_hostname" ] \
        || die 'hostname does not match the explicit guest identity'
    actual_machine_id=$(read_one_line /etc/machine-id 'machine ID')
    [ "$actual_machine_id" = "$expected_machine_id" ] \
        || die 'machine ID does not match the explicit guest identity'
    actual_boot_id=$(read_one_line /proc/sys/kernel/random/boot_id 'boot ID')
    [ "$actual_boot_id" = "$expected_boot_id" ] \
        || die 'boot ID changed; issue a new challenge for this boot'

    detect_virt=$(command_path systemd-detect-virt)
    actual_virtualization=$($detect_virt --vm) \
        || die 'the operating system is not reporting a virtual machine'
    [ "$actual_virtualization" != none ] \
        || die 'the operating system is not reporting a virtual machine'
    [ "$actual_virtualization" = "$expected_virtualization" ] \
        || die 'virtualization kind does not match the explicit claim'

    git_command=$(command_path git)
    actual_commit=$("$git_command" -c safe.directory="$root" \
        -C "$root" rev-parse --verify HEAD) \
        || die 'cannot resolve the guest checkout commit'
    [ "$actual_commit" = "$expected_commit" ] \
        || die 'guest checkout commit does not match the explicit commit'
    [ -z "$("$git_command" -c safe.directory="$root" \
        -C "$root" status --porcelain=v1 --untracked-files=all)" ] \
        || die 'guest checkout must be completely clean'

    sha256sum_command=$(command_path sha256sum)
    ssh_connection_hash=$(printf '%s' "$SSH_CONNECTION" \
        | "$sha256sum_command" | awk '{ print $1 }')
    require_pattern 'SSH connection hash' "$ssh_connection_hash" '^[0-9a-f]{64}$'
}

BLOCK_DEVNUM=
BLOCK_SYS_PATH=
BLOCK_DISK_DEVNUM=
BLOCK_DISK_SYS_PATH=
inspect_block_device() {
    path=$1
    label=$2
    [ ! -L "$path" ] || die "$label must be a canonical non-symlink device path"
    [ -b "$path" ] || die "$label is not a block device"
    [ "$(readlink -f -- "$path")" = "$path" ] \
        || die "$label is not a canonical device path"
    node_metadata=$(stat -Lc '%u:%F' "$path") \
        || die "cannot inspect $label"
    [ "$node_metadata" = '0:block special file' ] \
        || die "$label is not a root-owned block device"
    hex_dev=$(stat -Lc '%t:%T' "$path") || die "cannot inspect $label identity"
    require_pattern "$label device identity" "$hex_dev" \
        '^[0-9a-fA-F]+:[0-9a-fA-F]+$'
    hex_major=${hex_dev%%:*}
    hex_minor=${hex_dev#*:}
    decimal_major=$((0x$hex_major))
    decimal_minor=$((0x$hex_minor))
    BLOCK_DEVNUM=$decimal_major:$decimal_minor
    sys_link=/sys/dev/block/$BLOCK_DEVNUM
    [ -L "$sys_link" ] || die "$label has no kernel block identity"
    BLOCK_SYS_PATH=$(readlink -f -- "$sys_link") \
        || die "cannot resolve $label kernel identity"
    case "$BLOCK_SYS_PATH" in
        /sys/devices/*) ;;
        *) die "$label kernel identity escaped sysfs devices" ;;
    esac
    [ -r "$BLOCK_SYS_PATH/dev" ] \
        || die "$label kernel identity has no device number"
    [ "$(cat "$BLOCK_SYS_PATH/dev")" = "$BLOCK_DEVNUM" ] \
        || die "$label kernel identity changed"
    if [ -f "$BLOCK_SYS_PATH/partition" ]; then
        BLOCK_DISK_SYS_PATH=$(dirname -- "$BLOCK_SYS_PATH")
    else
        BLOCK_DISK_SYS_PATH=$BLOCK_SYS_PATH
    fi
    [ -r "$BLOCK_DISK_SYS_PATH/dev" ] \
        || die "$label has no parent whole-disk identity"
    BLOCK_DISK_DEVNUM=$(cat "$BLOCK_DISK_SYS_PATH/dev") \
        || die "cannot read $label parent whole-disk identity"
    require_pattern "$label parent whole-disk identity" "$BLOCK_DISK_DEVNUM" \
        '^[0-9]+:[0-9]+$'
}

mount_device_for() {
    mountpoint=$1
    devices=$(awk -v mountpoint="$mountpoint" '$5 == mountpoint { print $3 }' \
        /proc/self/mountinfo)
    [ -n "$devices" ] || die "required mountpoint is absent: $mountpoint"
    case "$devices" in
        *'
'*) die "required mountpoint is ambiguous: $mountpoint" ;;
    esac
    require_pattern "mount identity for $mountpoint" "$devices" '^[0-9]+:[0-9]+$'
    printf '%s\n' "$devices"
}

target_mount_count() {
    awk -v device="$target_devnum" '$3 == device { count += 1 } END { print count + 0 }' \
        /proc/self/mountinfo
}

campaign_mountpoint_count() {
    awk -v mountpoint="$mount_root" \
        '$5 == mountpoint { count += 1 } END { print count + 0 }' \
        /proc/self/mountinfo
}

verify_no_target_swap() {
    first=1
    while read -r source type size used priority; do
        if [ "$first" -eq 1 ]; then
            first=0
            continue
        fi
        [ -n "$source" ] || continue
        if [ -b "$source" ]; then
            inspect_block_device "$source" 'active swap device'
            [ "$BLOCK_DEVNUM" != "$target_devnum" ] \
                || die 'target disk is active swap'
            [ "$BLOCK_DISK_DEVNUM" != "$target_disk_devnum" ] \
                || die 'a target-disk descendant is active swap'
        fi
    done </proc/swaps
}

verify_empty_kernel_directory() {
    directory=$1
    label=$2
    for entry in "$directory"/*; do
        if [ -e "$entry" ] || [ -L "$entry" ]; then
            die "$label is not empty"
        fi
    done
}

verify_target_disk() {
    inspect_block_device "$target_disk" 'target disk'
    target_devnum=$BLOCK_DEVNUM
    target_sys_path=$BLOCK_SYS_PATH
    target_disk_devnum=$BLOCK_DISK_DEVNUM
    [ "$target_sys_path" = "$BLOCK_DISK_SYS_PATH" ] \
        || die 'target is a partition rather than a whole disk'
    grep -Fqx 'DEVTYPE=disk' "$target_sys_path/uevent" \
        || die 'target kernel identity is not a whole disk'
    [ "$(cat "$target_sys_path/ro")" = 0 ] \
        || die 'target whole disk is read-only'

    for child in "$target_sys_path"/*; do
        if [ -f "$child/partition" ]; then
            die 'target whole disk already has a partition'
        fi
    done
    verify_empty_kernel_directory "$target_sys_path/holders" \
        'target holder set'
    verify_empty_kernel_directory "$target_sys_path/slaves" \
        'target slave set'

    [ -L "$target_stable_path" ] || die 'target stable path is not a symlink'
    stable_metadata=$(stat -c '%u:%F:%h' "$target_stable_path") \
        || die 'cannot inspect target stable path'
    [ "$stable_metadata" = '0:symbolic link:1' ] \
        || die 'target stable path is not root-owned and singly linked'
    [ "$(readlink -f -- "$target_stable_path")" = "$target_disk" ] \
        || die 'target stable path does not resolve to the target disk'
    [ -r "$target_sys_path/diskseq" ] \
        || die 'target kernel identity has no disk sequence'
    [ "$(cat "$target_sys_path/diskseq")" = "$target_diskseq" ] \
        || die 'target disk sequence does not match the exact expectation'

    sectors=$(cat "$target_sys_path/size") || die 'cannot read target disk size'
    require_pattern 'target disk sector count' "$sectors" '^[0-9]+$'
    [ "$sectors" -le 18014398509481983 ] \
        || die 'target disk sector count exceeds the supported range'
    actual_target_bytes=$((sectors * 512))
    [ "$actual_target_bytes" = "$target_bytes" ] \
        || die 'target disk byte size does not match the exact expectation'

    inspect_block_device "$expected_root_device" 'expected root device'
    root_devnum=$BLOCK_DEVNUM
    root_disk_devnum=$BLOCK_DISK_DEVNUM
    actual_root_devnum=$(mount_device_for /)
    [ "$actual_root_devnum" = "$root_devnum" ] \
        || die 'declared root device does not own the live root mount'

    inspect_block_device "$expected_live_esp_device" 'expected live ESP device'
    live_esp_devnum=$BLOCK_DEVNUM
    live_esp_disk_devnum=$BLOCK_DISK_DEVNUM
    [ -f "$BLOCK_SYS_PATH/partition" ] \
        || die 'declared live ESP device is not a partition'
    actual_live_esp_devnum=$(mount_device_for "$expected_live_esp_mountpoint")
    [ "$actual_live_esp_devnum" = "$live_esp_devnum" ] \
        || die 'declared live ESP device does not own its declared mountpoint'

    [ "$target_devnum" != "$root_devnum" ] \
        || die 'target disk is the live root device'
    [ "$target_devnum" != "$live_esp_devnum" ] \
        || die 'target disk is the live ESP device'
    [ "$target_disk_devnum" != "$root_disk_devnum" ] \
        || die 'target disk is the live root parent disk'
    [ "$target_disk_devnum" != "$live_esp_disk_devnum" ] \
        || die 'target disk is the live ESP parent disk'
    [ "$(target_mount_count)" = 0 ] \
        || die 'target whole disk is already mounted'
    verify_no_target_swap

    # Recheck the stable alias and disk sequence after every other observation.
    [ "$(readlink -f -- "$target_stable_path")" = "$target_disk" ] \
        || die 'target stable path changed during admission'
    [ "$(cat "$target_sys_path/diskseq")" = "$target_diskseq" ] \
        || die 'target disk sequence changed during admission'
    inspect_block_device "$target_disk" 'target disk closing observation'
    [ "$BLOCK_DEVNUM" = "$target_devnum" ] \
        && [ "$BLOCK_SYS_PATH" = "$target_sys_path" ] \
        || die 'target disk identity changed during admission'
    [ "$(cat "$BLOCK_SYS_PATH/diskseq")" = "$target_diskseq" ] \
        || die 'target disk sequence changed at the closing observation'
    [ "$(mount_device_for /)" = "$root_devnum" ] \
        || die 'live root ownership changed during admission'
    [ "$(mount_device_for "$expected_live_esp_mountpoint")" = "$live_esp_devnum" ] \
        || die 'live ESP ownership changed during admission'
}

ensure_runtime_root() {
    [ -d /run ] && [ ! -L /run ] || die '/run is unavailable or unsafe'
    [ "$(stat -c '%u:%F' /run)" = '0:directory' ] \
        || die '/run is not a root-owned directory'
    if [ ! -e "$runtime_root" ] && [ ! -L "$runtime_root" ]; then
        install -d -m 700 -o 0 -g 0 -- "$runtime_root" \
            || die 'cannot create the private runtime root'
    fi
    [ -d "$runtime_root" ] && [ ! -L "$runtime_root" ] \
        || die 'private runtime root is unavailable or unsafe'
    [ "$(stat -c '%u:%g:%a:%F' "$runtime_root")" = '0:0:700:directory' ] \
        || die 'private runtime root metadata is unsafe'
}

verify_runtime_inventory() {
    allowed=$1
    for entry in "$runtime_root"/* "$runtime_root"/.[!.]* "$runtime_root"/..?*; do
        if [ ! -e "$entry" ] && [ ! -L "$entry" ]; then
            continue
        fi
        case "$allowed:$entry" in
            empty:*) die 'private runtime root is not empty' ;;
            marker:"$authorization_marker") ;;
            campaign:"$consumed_marker" | campaign:"$campaign_lock" \
                | campaign:"$mount_root") ;;
            *) die "unexpected private runtime entry: $entry" ;;
        esac
    done
}

write_marker_body() {
    issued=$1
    marker_challenge=$2
    if [ "$campaign_profile" = gpt-boot-topologies ]; then
        marker_protocol=2
    else
        marker_protocol=1
    fi
    cat <<EOF
protocol=$marker_protocol
hostname=$expected_hostname
machine_id=$expected_machine_id
boot_id=$expected_boot_id
virtualization=$expected_virtualization
ssh_connection_sha256=$ssh_connection_hash
commit=$expected_commit
target_disk=$target_disk
target_stable_path=$target_stable_path
target_diskseq=$target_diskseq
target_bytes=$target_bytes
root_device=$expected_root_device
live_esp_device=$expected_live_esp_device
live_esp_mountpoint=$expected_live_esp_mountpoint
filesystem_label=$filesystem_label
publication_parent=$publication_parent
snapshot_confirmation=$snapshot_confirmation
remote_confirmation=$remote_confirmation
cooperative_root_confirmation=$cooperative_root_confirmation
EOF
    if [ "$campaign_profile" = gpt-boot-topologies ]; then
        printf 'campaign_profile=%s\n' "$campaign_profile"
    fi
    cat <<EOF
issued_uptime_seconds=$issued
challenge=$marker_challenge
EOF
}

verify_marker() {
    marker_path=$1
    [ ! -L "$marker_path" ] && [ -f "$marker_path" ] \
        || die 'authorization marker is unavailable or unsafe'
    [ "$(stat -c '%F:%u:%g:%a:%h' "$marker_path")" \
        = 'regular file:0:0:600:1' ] \
        || die 'authorization marker metadata is unsafe'

    exec 3<"$marker_path"
    if [ "$campaign_profile" = gpt-boot-topologies ]; then
        expected_marker_protocol=2
    else
        expected_marker_protocol=1
    fi
    IFS= read -r line <&3 && [ "$line" = "protocol=$expected_marker_protocol" ] \
        || die 'authorization marker protocol is invalid'
    for expected_line in \
        "hostname=$expected_hostname" \
        "machine_id=$expected_machine_id" \
        "boot_id=$expected_boot_id" \
        "virtualization=$expected_virtualization" \
        "ssh_connection_sha256=$ssh_connection_hash" \
        "commit=$expected_commit" \
        "target_disk=$target_disk" \
        "target_stable_path=$target_stable_path" \
        "target_diskseq=$target_diskseq" \
        "target_bytes=$target_bytes" \
        "root_device=$expected_root_device" \
        "live_esp_device=$expected_live_esp_device" \
        "live_esp_mountpoint=$expected_live_esp_mountpoint" \
        "filesystem_label=$filesystem_label" \
        "publication_parent=$publication_parent" \
        "snapshot_confirmation=$snapshot_confirmation" \
        "remote_confirmation=$remote_confirmation" \
        "cooperative_root_confirmation=$cooperative_root_confirmation"
    do
        IFS= read -r line <&3 && [ "$line" = "$expected_line" ] \
            || die 'authorization marker is not bound to this exact invocation'
    done
    if [ "$campaign_profile" = gpt-boot-topologies ]; then
        IFS= read -r line <&3 && [ "$line" = "campaign_profile=$campaign_profile" ] \
            || die 'authorization marker campaign profile is invalid'
    fi
    IFS= read -r line <&3 || die 'authorization marker omits its issue time'
    case "$line" in
        issued_uptime_seconds=*) issued=${line#issued_uptime_seconds=} ;;
        *) die 'authorization marker issue time is malformed' ;;
    esac
    require_pattern 'authorization marker issue time' "$issued" '^[0-9]+$'
    IFS= read -r line <&3 || die 'authorization marker omits its challenge'
    [ "$line" = "challenge=$challenge" ] \
        || die 'authorization marker challenge does not match'
    if IFS= read -r line <&3; then
        die 'authorization marker contains trailing data'
    fi
    exec 3<&-

    now=$(current_uptime_seconds)
    [ "$now" -ge "$issued" ] || die 'authorization marker issue time is in the future'
    age=$((now - issued))
    [ "$age" -le "$challenge_max_age_seconds" ] \
        || die 'authorization challenge expired; inspect and remove the stale marker before rearming'
}

create_challenge() {
    ensure_runtime_root
    verify_runtime_inventory empty
    random_challenge=$(od -An -N32 -tx1 /dev/urandom | tr -d ' \n') \
        || die 'cannot create a fresh authorization challenge'
    require_pattern 'fresh authorization challenge' "$random_challenge" \
        '^[0-9a-f]{64}$'
    issued=$(current_uptime_seconds)
    temporary_marker=$(mktemp "$runtime_root/.authorization-v1.XXXXXXXXXXXX") \
        || die 'cannot create a private authorization marker'
    trap 'rm -f -- "$temporary_marker"' EXIT
    trap 'exit 129' HUP
    trap 'exit 130' INT
    trap 'exit 143' TERM
    chmod 600 "$temporary_marker" \
        || die 'cannot restrict the private authorization marker'
    write_marker_body "$issued" "$random_challenge" >"$temporary_marker" \
        || die 'cannot write the private authorization marker'
    [ "$(stat -c '%F:%u:%g:%a:%h' "$temporary_marker")" \
        = 'regular file:0:0:600:1' ] \
        || die 'private authorization marker metadata is unsafe'
    ln -- "$temporary_marker" "$authorization_marker" \
        || die 'an authorization marker already exists'
    rm -f -- "$temporary_marker"
    trap - EXIT HUP INT TERM
    challenge=$random_challenge
    verify_marker "$authorization_marker"
    verify_runtime_inventory marker
    printf 'VM_BOOT_STORAGE_CHALLENGE=%s\n' "$random_challenge"
}

admit_challenge() {
    ensure_runtime_root
    verify_runtime_inventory marker
    verify_marker "$authorization_marker"
    verify_target_disk
    printf '%s\n' \
        'Disposable VM boot-storage admission passed without disk mutation.' \
        "Target: $target_stable_path -> $target_disk ($target_bytes bytes; diskseq $target_diskseq)" \
        "Guest: $expected_hostname / $expected_machine_id / $expected_boot_id" \
        'The challenge remains armed; campaign will consume it exactly once.'
}

verify_guest_identity
case "$mode" in
    challenge) verify_target_disk; create_challenge ;;
    admit) admit_challenge ;;
    campaign)
        if [ "$campaign_profile" = gpt-boot-topologies ]; then
            effects_script=$root/misc/vm/disposable-uefi-boot-gpt-topology-effects.sh
        else
            effects_script=$root/misc/vm/disposable-uefi-boot-storage-effects.sh
        fi
        [ -f "$effects_script" ] && [ ! -L "$effects_script" ] \
            || die 'campaign effect implementation is unavailable or unsafe'
        . "$effects_script"
        run_campaign
        ;;
esac
