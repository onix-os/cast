. "$root/misc/vm/disposable-uefi-boot-storage-effects.sh"

esp_mount=$mount_root/esp
xbootldr_mount=$mount_root/xbootldr
topology_installation=$publication_build_root/topology-installation
topology_source=$topology_installation/etc/cast/boot-topology.glu
aggregate_fixture_root=$publication_build_root/gpt-aggregate-fixtures
partition_layout=$publication_build_root/gpt-layout.sfdisk
esp_type=c12a7328-f81f-11d2-ba4b-00a0c93ec93b
xbootldr_type=bc13c2ff-59e6-4262-a352-b275fd6f7172
sfdisk_command=
partprobe_command=
udevadm_command=
blkid_command=
fsck_fat_command=
esp_device=
esp_devnum=
esp_partuuid=
xbootldr_device=
xbootldr_devnum=
xbootldr_partuuid=
esp_mounted=0
xbootldr_mounted=0
expected_partition_layout=empty
esp_formatted=0
xbootldr_formatted=0
live_system_disk=/dev/"vda"
topology_test_confirmation=
topology_test_make_target=

configure_gpt_publication_profile() {
    case "$campaign_profile" in
        gpt-boot-topologies)
            topology_test_confirmation=disposable-vm-gpt-topology-only
            topology_test_make_target=forge-disposable-vm-gpt-boot-topology-test
            ;;
        gpt-receipt-bound-aggregate-v1)
            topology_test_confirmation=disposable-vm-gpt-receipt-bound-aggregate-only
            topology_test_make_target=forge-disposable-vm-gpt-aggregate-publication-test
            ;;
        *) die 'GPT effects require an exact supported marker-bound campaign profile' ;;
    esac
}

prepare_aggregate_fixture_parents() {
    [ "$campaign_profile" = gpt-receipt-bound-aggregate-v1 ] || return 0
    for fixture_directory in \
        "$aggregate_fixture_root" \
        "$aggregate_fixture_root/alias" \
        "$aggregate_fixture_root/distinct"
    do
        [ ! -e "$fixture_directory" ] && [ ! -L "$fixture_directory" ] \
            || die 'aggregate fixture directory is not fresh'
        "$mkdir_command" -m 700 -- "$fixture_directory" \
            || die 'cannot create private aggregate fixture directory'
        [ "$(stat -Lc '%u:%g:%a:%F' -- "$fixture_directory")" \
            = '0:0:700:directory' ] \
            || die 'aggregate fixture directory metadata is unsafe'
    done
}

load_gpt_effect_commands() {
    load_effect_commands
    sfdisk_command=$(command_path sfdisk)
    partprobe_command=$(command_path partprobe)
    udevadm_command=$(command_path udevadm)
    blkid_command=$(command_path blkid)
    fsck_fat_command=$(command_path fsck.fat)
}

mountpoint_count() (
    mountpoint_count_path=$1
    awk -v mountpoint="$mountpoint_count_path" \
        '$5 == mountpoint { count += 1 } END { print count + 0 }' \
        /proc/self/mountinfo
)

device_mount_count() (
    device_mount_count_devnum=$1
    awk -v device="$device_mount_count_devnum" \
        '$3 == device { count += 1 } END { print count + 0 }' \
        /proc/self/mountinfo
)

gpt_mount_is_exact_identity() (
    gpt_identity_mountpoint=$1
    gpt_identity_devnum=$2
    gpt_identity_evidence=$(awk -v mountpoint="$gpt_identity_mountpoint" '
        $5 == mountpoint {
            for (i = 1; i <= NF; i += 1) {
                if ($i == "-") print $3 "|" $(i + 1)
            }
        }
    ' /proc/self/mountinfo)
    [ "$gpt_identity_evidence" = "$gpt_identity_devnum|vfat" ] \
        && [ "$(mountpoint_count "$gpt_identity_mountpoint")" = 1 ] \
        && [ "$(device_mount_count "$gpt_identity_devnum")" = 1 ]
)

gpt_mount_is_exact_target() (
    gpt_target_mountpoint=$1
    gpt_target_devnum=$2
    gpt_target_evidence=$(awk -v mountpoint="$gpt_target_mountpoint" '
        $5 == mountpoint {
            for (i = 1; i <= NF; i += 1) {
                if ($i == "-") print $3 "|" $6 "|" $(i + 1) "|" $(i + 3)
            }
        }
    ' /proc/self/mountinfo)
    [ -n "$gpt_target_evidence" ] || return 1
    case "$gpt_target_evidence" in *'
'*) return 1 ;; esac
    gpt_target_old_ifs=$IFS
    IFS='|'
    set -- $gpt_target_evidence
    IFS=$gpt_target_old_ifs
    [ "$#" -eq 4 ] || return 1
    [ "$1" = "$gpt_target_devnum" ] || return 1
    for gpt_target_required in rw nosuid nodev noexec nosymfollow; do
        case ",$2," in *,$gpt_target_required,*) ;; *) return 1 ;; esac
    done
    [ "$3" = vfat ] || return 1
    for gpt_target_required in rw fmask=0133 dmask=0022; do
        case ",$4," in *,$gpt_target_required,*) ;; *) return 1 ;; esac
    done
    gpt_target_old_ifs=$IFS
    IFS=,
    set -- $4
    IFS=$gpt_target_old_ifs
    for gpt_target_option do
        case "$gpt_target_option" in
            uid=0 | gid=0) ;;
            uid=* | gid=*) return 1 ;;
        esac
    done
    [ "$(stat -Lc '%u:%g:%F' -- "$gpt_target_mountpoint")" = '0:0:directory' ] \
        && [ "$(mountpoint_count "$gpt_target_mountpoint")" = 1 ] \
        && [ "$(device_mount_count "$gpt_target_devnum")" = 1 ]
)

cleanup_gpt_mount() (
    cleanup_mountpoint=$1
    cleanup_devnum=$2
    if gpt_mount_is_exact_identity "$cleanup_mountpoint" "$cleanup_devnum"; then
        run_bounded 30s "$umount_command" -i -- "$cleanup_mountpoint" \
            || return 1
    elif [ "$(mountpoint_count "$cleanup_mountpoint")" != 0 ] \
        || [ "$(device_mount_count "$cleanup_devnum")" != 0 ]; then
        return 1
    fi
    [ "$(mountpoint_count "$cleanup_mountpoint")" = 0 ] \
        && [ "$(device_mount_count "$cleanup_devnum")" = 0 ]
)

cleanup_gpt_campaign() {
    gpt_cleanup_status=$?
    trap - EXIT HUP INT TERM
    gpt_cleanup_failed=0
    if [ "$xbootldr_mounted" -ne 0 ]; then
        cleanup_gpt_mount "$xbootldr_mount" "$xbootldr_devnum" \
            && xbootldr_mounted=0 || gpt_cleanup_failed=1
    fi
    if [ "$esp_mounted" -ne 0 ]; then
        cleanup_gpt_mount "$esp_mount" "$esp_devnum" \
            && esp_mounted=0 || gpt_cleanup_failed=1
    fi
    if [ "$campaign_complete" -eq 1 ] && [ "$gpt_cleanup_failed" -eq 0 ]; then
        for gpt_cleanup_directory in "$xbootldr_mount" "$esp_mount" "$mount_root"; do
            if [ -d "$gpt_cleanup_directory" ] && [ ! -L "$gpt_cleanup_directory" ]; then
                rmdir -- "$gpt_cleanup_directory" || gpt_cleanup_failed=1
            else
                gpt_cleanup_failed=1
            fi
        done
    fi
    if [ "$campaign_complete" -eq 1 ] && [ "$gpt_cleanup_failed" -eq 0 ]; then
        if [ -d "$publication_build_root" ] && [ ! -L "$publication_build_root" ] \
            && [ "$(stat -Lc '%u:%g:%a:%F' -- "$publication_build_root")" \
                = '0:0:700:directory' ]; then
            "$rm_command" -rf --one-file-system -- "$publication_build_root" \
                || gpt_cleanup_failed=1
            if [ -e "$publication_build_root" ] || [ -L "$publication_build_root" ]; then
                gpt_cleanup_failed=1
            fi
        else
            gpt_cleanup_failed=1
        fi
    fi
    if [ "$campaign_complete" -eq 1 ] && [ "$gpt_cleanup_failed" -eq 0 ]; then
        rmdir -- "$campaign_lock" || gpt_cleanup_failed=1
    fi
    if [ "$campaign_complete" -eq 1 ] && [ "$gpt_cleanup_failed" -eq 0 ]; then
        rm -f -- "$consumed_marker" || gpt_cleanup_failed=1
    fi
    if [ "$authorization_consumed" -eq 1 ] \
        && { [ "$campaign_complete" -ne 1 ] || [ "$gpt_cleanup_failed" -ne 0 ]; }; then
        printf '%s\n' 'leaving GPT topology recovery sentinels for fail-closed VM recovery' >&2
        [ "$destructive_started" -eq 0 ] \
            || printf '%s\n' 'partition effects started; target completion is unclassified' >&2
        gpt_cleanup_status=1
    fi
    [ "$gpt_cleanup_failed" -eq 0 ] || gpt_cleanup_status=1
    exit "$gpt_cleanup_status"
}

verify_gpt_whole_disk_identity() (
    inspect_block_device "$target_disk" 'GPT topology target disk'
    [ "$BLOCK_DEVNUM" = "$target_devnum" ] \
        && [ "$BLOCK_SYS_PATH" = "$target_sys_path" ] \
        && [ "$BLOCK_DISK_DEVNUM" = "$target_disk_devnum" ] \
        || die 'GPT topology target whole-disk identity changed'
    grep -Fqx 'DEVTYPE=disk' "$target_sys_path/uevent" \
        || die 'GPT topology target is no longer a whole disk'
    [ "$(cat "$target_sys_path/ro")" = 0 ] \
        || die 'GPT topology target became read-only'
    [ "$(cat "$target_sys_path/queue/logical_block_size")" = 512 ] \
        || die 'GPT topology fixed geometry requires 512-byte logical sectors'
    [ "$(readlink -f -- "$target_stable_path")" = "$target_disk" ] \
        || die 'GPT topology target stable path changed'
    [ "$(cat "$target_sys_path/diskseq")" = "$target_diskseq" ] \
        || die 'GPT topology target disk sequence changed'
    whole_sectors=$(cat "$target_sys_path/size") \
        || die 'cannot re-read GPT topology target size'
    [ "$((whole_sectors * 512))" = "$target_bytes" ] \
        || die 'GPT topology target byte size changed'
    [ "$(mount_device_for /)" = "$root_devnum" ] \
        || die 'live root ownership changed during GPT topology campaign'
    [ "$(mount_device_for "$expected_live_esp_mountpoint")" = "$live_esp_devnum" ] \
        || die 'live ESP ownership changed during GPT topology campaign'
    [ "$target_disk_devnum" != "$root_disk_devnum" ] \
        && [ "$target_disk_devnum" != "$live_esp_disk_devnum" ] \
        || die 'GPT topology target overlaps live root or live ESP storage'
    case "$target_disk" in
        "$live_system_disk" | "$live_system_disk"[0-9]*)
            die 'GPT topology target may never be a primary live-system node'
            ;;
    esac
    verify_empty_kernel_directory "$target_sys_path/holders" 'GPT target holder set'
    verify_empty_kernel_directory "$target_sys_path/slaves" 'GPT target slave set'
    [ "$(device_mount_count "$target_devnum")" = 0 ] \
        || die 'GPT topology whole disk is unexpectedly mounted'
    for whole_child_path in "$target_sys_path"/*; do
        [ -f "$whole_child_path/partition" ] || continue
        whole_child_devnum=$(cat "$whole_child_path/dev") \
            || die 'cannot inspect existing GPT target child identity'
        require_pattern 'GPT target child device identity' "$whole_child_devnum" \
            '^[0-9]+:[0-9]+$'
        whole_child_devname=$(sed -n 's/^DEVNAME=//p' "$whole_child_path/uevent")
        require_pattern 'GPT target child kernel name' "$whole_child_devname" \
            '^[A-Za-z0-9._+-]+$'
        inspect_block_device "/dev/$whole_child_devname" 'existing GPT target child'
        [ "$BLOCK_DEVNUM" = "$whole_child_devnum" ] \
            && [ "$BLOCK_DISK_DEVNUM" = "$target_disk_devnum" ] \
            && [ "$BLOCK_DISK_SYS_PATH" = "$target_sys_path" ] \
            || die 'existing GPT target child escaped its admitted parent'
        [ "$(cat "$BLOCK_SYS_PATH/ro")" = 0 ] \
            || die 'existing GPT target child is read-only'
        verify_empty_kernel_directory "$BLOCK_SYS_PATH/holders" \
            'existing GPT target child holder set'
        verify_empty_kernel_directory "$BLOCK_SYS_PATH/slaves" \
            'existing GPT target child slave set'
        if [ -n "$esp_devnum" ] && [ "$whole_child_devnum" = "$esp_devnum" ] \
            && [ "$esp_mounted" -eq 1 ]; then
            gpt_mount_is_exact_target "$esp_mount" "$esp_devnum" \
                || die 'tracked ESP mount identity or policy changed'
        elif [ -n "$xbootldr_devnum" ] && [ "$whole_child_devnum" = "$xbootldr_devnum" ] \
            && [ "$xbootldr_mounted" -eq 1 ]; then
            gpt_mount_is_exact_target "$xbootldr_mount" "$xbootldr_devnum" \
                || die 'tracked XBOOTLDR mount identity or policy changed'
        else
            [ "$(device_mount_count "$whole_child_devnum")" = 0 ] \
                || die 'an untracked GPT target child is mounted'
        fi
    done
    verify_no_target_swap
    inspect_block_device "$target_disk" 'GPT topology target closing disk'
    [ "$BLOCK_DEVNUM" = "$target_devnum" ] \
        && [ "$BLOCK_SYS_PATH" = "$target_sys_path" ] \
        && [ "$BLOCK_DISK_DEVNUM" = "$target_disk_devnum" ] \
        || die 'GPT topology target changed during descendant admission'
    case "$expected_partition_layout" in
        empty)
            verify_partition_inventory 0
            ;;
        alias)
            [ "$($blkid_command -p -s PTTYPE -o value -- "$target_disk")" = gpt ] \
                || die 'admitted target no longer has an exact GPT table'
            verify_partition_inventory 1
            verify_partition_geometry 1 2048 524288
            ;;
        distinct)
            [ "$($blkid_command -p -s PTTYPE -o value -- "$target_disk")" = gpt ] \
                || die 'admitted target no longer has an exact GPT table'
            verify_partition_inventory 2
            verify_partition_geometry 1 2048 524288
            verify_partition_geometry 2 526336 1048576
            ;;
        *) die 'invalid retained GPT layout expectation' ;;
    esac
    if [ "$esp_formatted" -eq 1 ]; then
        authenticate_partition_filesystem "$esp_device" CASTESP
    fi
    if [ "$xbootldr_formatted" -eq 1 ]; then
        authenticate_partition_filesystem "$xbootldr_device" CASTXBOOT
    fi
    inspect_block_device "$target_disk" 'GPT topology final closing disk'
    [ "$BLOCK_DEVNUM" = "$target_devnum" ] \
        && [ "$BLOCK_SYS_PATH" = "$target_sys_path" ] \
        && [ "$BLOCK_DISK_DEVNUM" = "$target_disk_devnum" ] \
        || die 'GPT topology target changed during geometry authentication'
)

require_gpt_repartition_safe() (
    [ "$esp_mounted" -eq 0 ] && [ "$xbootldr_mounted" -eq 0 ] \
        || die 'tracked GPT topology mounts remain before repartitioning'
    verify_gpt_whole_disk_identity
    for safe_child_path in "$target_sys_path"/*; do
        [ -f "$safe_child_path/partition" ] || continue
        safe_child_devnum=$(cat "$safe_child_path/dev") \
            || die 'cannot inspect pre-repartition child identity'
        safe_child_devname=$(sed -n 's/^DEVNAME=//p' "$safe_child_path/uevent")
        require_pattern 'pre-repartition child kernel name' "$safe_child_devname" \
            '^[A-Za-z0-9._+-]+$'
        inspect_block_device "/dev/$safe_child_devname" 'pre-repartition target child'
        [ "$BLOCK_DEVNUM" = "$safe_child_devnum" ] \
            && [ "$BLOCK_DISK_DEVNUM" = "$target_disk_devnum" ] \
            && [ "$BLOCK_DISK_SYS_PATH" = "$target_sys_path" ] \
            || die 'pre-repartition child escaped the admitted disk'
        [ "$(device_mount_count "$safe_child_devnum")" = 0 ] \
            || die 'target descendant remains mounted before repartitioning'
        verify_empty_kernel_directory "$BLOCK_SYS_PATH/holders" \
            'pre-repartition child holder set'
        verify_empty_kernel_directory "$BLOCK_SYS_PATH/slaves" \
            'pre-repartition child slave set'
    done
    verify_no_target_swap
    inspect_block_device "$target_disk" 'pre-repartition closing target disk'
    [ "$BLOCK_DEVNUM" = "$target_devnum" ] \
        && [ "$BLOCK_SYS_PATH" = "$target_sys_path" ] \
        && [ "$BLOCK_DISK_DEVNUM" = "$target_disk_devnum" ] \
        || die 'target changed at pre-repartition boundary'
)

write_partition_layout() (
    write_layout_kind=$1
    [ ! -e "$partition_layout" ] && [ ! -L "$partition_layout" ] \
        || die 'fresh GPT layout input already exists'
    umask 077
    case "$write_layout_kind" in
        alias)
            cat >"$partition_layout" <<EOF
label: gpt
unit: sectors
first-lba: 2048

start=2048, size=524288, type=$esp_type, name="CAST ESP"
EOF
            ;;
        distinct)
            cat >"$partition_layout" <<EOF
label: gpt
unit: sectors
first-lba: 2048

start=2048, size=524288, type=$esp_type, name="CAST ESP"
start=526336, size=1048576, type=$xbootldr_type, name="CAST XBOOTLDR"
EOF
            ;;
        *) die 'unknown GPT topology layout' ;;
    esac
    [ "$(stat -Lc '%u:%g:%a:%F:%h' -- "$partition_layout")" \
        = '0:0:600:regular file:1' ] \
        || die 'GPT layout input metadata is unsafe'
)

settle_partition_table() (
    run_bounded 30s "$partprobe_command" -- "$target_disk" \
        || die 'kernel refused the new exact GPT partition table'
    run_bounded 30s "$udevadm_command" settle \
        || die 'udev did not settle the new exact GPT partition table'
)

create_partition_layout() {
    create_layout_kind=$1
    verify_gpt_whole_disk_identity
    write_partition_layout "$create_layout_kind"
    require_gpt_repartition_safe
    run_bounded 120s "$sfdisk_command" --wipe always --wipe-partitions always \
        -- "$target_disk" <"$partition_layout" \
        || die 'bounded GPT partition creation failed on the admitted disk'
    rm -f -- "$partition_layout" \
        || die 'cannot remove consumed GPT layout input'
    settle_partition_table
    esp_formatted=0
    xbootldr_formatted=0
    expected_partition_layout=$create_layout_kind
    verify_gpt_whole_disk_identity
}

partition_device_for_number() (
    lookup_partition_number=$1
    lookup_partition_found=
    for lookup_child_path in "$target_sys_path"/*; do
        [ -f "$lookup_child_path/partition" ] || continue
        [ "$(cat "$lookup_child_path/partition")" = "$lookup_partition_number" ] \
            || continue
        [ -z "$lookup_partition_found" ] \
            || die 'partition number is ambiguous below admitted disk'
        lookup_partition_devname=$(sed -n 's/^DEVNAME=//p' "$lookup_child_path/uevent")
        require_pattern 'partition kernel name' "$lookup_partition_devname" \
            '^[A-Za-z0-9._+-]+$'
        lookup_partition_found=/dev/$lookup_partition_devname
    done
    [ -n "$lookup_partition_found" ] \
        || die 'expected partition is absent below admitted disk'
    printf '%s\n' "$lookup_partition_found"
)

verify_partition_inventory() (
    inventory_expected_count=$1
    inventory_observed_count=0
    for inventory_child_path in "$target_sys_path"/*; do
        [ -f "$inventory_child_path/partition" ] || continue
        inventory_observed_count=$((inventory_observed_count + 1))
    done
    [ "$inventory_observed_count" = "$inventory_expected_count" ] \
        || die 'admitted disk has an unexpected partition inventory'
)

verify_partition_geometry() (
    geometry_number=$1
    geometry_start=$2
    geometry_size=$3
    geometry_device=$(partition_device_for_number "$geometry_number")
    inspect_block_device "$geometry_device" "partition $geometry_number geometry"
    [ "$BLOCK_DISK_DEVNUM" = "$target_disk_devnum" ] \
        && [ "$BLOCK_DISK_SYS_PATH" = "$target_sys_path" ] \
        || die 'partition geometry escaped the admitted disk'
    [ "$(cat "$BLOCK_SYS_PATH/start")" = "$geometry_start" ] \
        && [ "$(cat "$BLOCK_SYS_PATH/size")" = "$geometry_size" ] \
        || die 'generated partition geometry changed'
)

authenticate_partition_filesystem() (
    filesystem_auth_device=$1
    filesystem_expected_label=$2
    filesystem_observed_type=$($blkid_command -p -s TYPE -o value \
        -- "$filesystem_auth_device") \
        || die 'cannot authenticate generated partition filesystem type'
    filesystem_observed_label=$($blkid_command -p -s LABEL -o value \
        -- "$filesystem_auth_device") \
        || die 'cannot authenticate generated partition filesystem label'
    [ "$filesystem_observed_type" = vfat ] \
        && [ "$filesystem_observed_label" = "$filesystem_expected_label" ] \
        || die 'generated partition VFAT type or label changed'
)

authenticate_generated_partition() (
    partition_auth_device=$1
    partition_auth_number=$2
    partition_auth_type=$3
    partition_auth_start=$4
    partition_auth_size=$5
    partition_auth_mount_count=$6
    inspect_block_device "$partition_auth_device" \
        "generated partition $partition_auth_number"
    [ "$BLOCK_DISK_DEVNUM" = "$target_disk_devnum" ] \
        && [ "$BLOCK_DISK_SYS_PATH" = "$target_sys_path" ] \
        || die 'generated partition escaped the admitted whole disk'
    [ -f "$BLOCK_SYS_PATH/partition" ] \
        && [ "$(cat "$BLOCK_SYS_PATH/partition")" = "$partition_auth_number" ] \
        || die 'generated partition number is not exact'
    [ "$(cat "$BLOCK_SYS_PATH/ro")" = 0 ] \
        || die 'generated partition is read-only'
    [ "$(cat "$BLOCK_SYS_PATH/start")" = "$partition_auth_start" ] \
        && [ "$(cat "$BLOCK_SYS_PATH/size")" = "$partition_auth_size" ] \
        || die 'generated partition geometry is not exact'
    verify_empty_kernel_directory "$BLOCK_SYS_PATH/holders" \
        'generated partition holder set'
    verify_empty_kernel_directory "$BLOCK_SYS_PATH/slaves" \
        'generated partition slave set'
    partition_auth_devnum=$BLOCK_DEVNUM
    partition_auth_sysfs_partuuid=$(sed -n 's/^PARTUUID=//p' "$BLOCK_SYS_PATH/uevent")
    require_pattern 'generated partition PARTUUID' "$partition_auth_sysfs_partuuid" \
        '^[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}$'
    partition_auth_partuuid=$(printf '%s\n' "$partition_auth_sysfs_partuuid" \
        | tr 'A-F' 'a-f')
    partition_auth_stable=/dev/disk/by-partuuid/$partition_auth_partuuid
    [ -L "$partition_auth_stable" ] \
        && [ "$(readlink -f -- "$partition_auth_stable")" = "$partition_auth_device" ] \
        || die 'generated partition PARTUUID alias is not exact'
    partition_auth_blkid_partuuid=$($blkid_command -p -s PART_ENTRY_UUID -o value \
        -- "$partition_auth_device") \
        || die 'cannot authenticate generated partition UUID from GPT'
    partition_auth_blkid_type=$($blkid_command -p -s PART_ENTRY_TYPE -o value \
        -- "$partition_auth_device") \
        || die 'cannot authenticate generated partition type from GPT'
    [ "$(printf '%s\n' "$partition_auth_blkid_partuuid" | tr 'A-F' 'a-f')" \
        = "$partition_auth_partuuid" ] \
        && [ "$(printf '%s\n' "$partition_auth_blkid_type" | tr 'A-F' 'a-f')" \
            = "$partition_auth_type" ] \
        || die 'generated partition GPT identity or role changed'
    [ "$(device_mount_count "$partition_auth_devnum")" \
        = "$partition_auth_mount_count" ] \
        || die 'generated partition mount count is not exact'
    printf '%s|%s\n' "$partition_auth_devnum" "$partition_auth_partuuid"
)

reauthenticate_generated_partition() (
    partition_reauth_device=$1
    partition_reauth_number=$2
    partition_reauth_type=$3
    partition_reauth_start=$4
    partition_reauth_size=$5
    partition_reauth_devnum=$6
    partition_reauth_partuuid=$7
    partition_reauth_mount_count=$8
    partition_reauth_evidence=$(authenticate_generated_partition \
        "$partition_reauth_device" "$partition_reauth_number" \
        "$partition_reauth_type" "$partition_reauth_start" \
        "$partition_reauth_size" "$partition_reauth_mount_count")
    [ "${partition_reauth_evidence%%|*}" = "$partition_reauth_devnum" ] \
        && [ "${partition_reauth_evidence#*|}" = "$partition_reauth_partuuid" ] \
        || die 'generated partition identity changed after authentication'
)

prepare_alias_partitions() {
    create_partition_layout alias
    verify_partition_inventory 1
    esp_device=$(partition_device_for_number 1)
    alias_partition_evidence=$(authenticate_generated_partition \
        "$esp_device" 1 "$esp_type" 2048 524288 0)
    esp_devnum=${alias_partition_evidence%%|*}
    esp_partuuid=${alias_partition_evidence#*|}
}

prepare_distinct_partitions() {
    create_partition_layout distinct
    verify_partition_inventory 2
    esp_device=$(partition_device_for_number 1)
    distinct_esp_evidence=$(authenticate_generated_partition \
        "$esp_device" 1 "$esp_type" 2048 524288 0)
    esp_devnum=${distinct_esp_evidence%%|*}
    esp_partuuid=${distinct_esp_evidence#*|}
    xbootldr_device=$(partition_device_for_number 2)
    distinct_xbootldr_evidence=$(authenticate_generated_partition \
        "$xbootldr_device" 2 "$xbootldr_type" 526336 1048576 0)
    xbootldr_devnum=${distinct_xbootldr_evidence%%|*}
    xbootldr_partuuid=${distinct_xbootldr_evidence#*|}
    [ "$esp_devnum" != "$xbootldr_devnum" ] \
        && [ "$esp_partuuid" != "$xbootldr_partuuid" ] \
        || die 'distinct ESP and XBOOTLDR identities alias'
}

format_partition() {
    format_device=$1
    format_label=$2
    format_number=$3
    format_type=$4
    format_start=$5
    format_size=$6
    format_devnum=$7
    format_partuuid=$8
    verify_gpt_whole_disk_identity
    reauthenticate_generated_partition \
        "$format_device" "$format_number" "$format_type" \
        "$format_start" "$format_size" "$format_devnum" "$format_partuuid" 0
    run_bounded 120s "$mkfs_command" --mbr=n -F 32 -n "$format_label" \
        -- "$format_device" \
        || die 'bounded VFAT creation failed on authenticated generated partition'
    authenticate_partition_filesystem "$format_device" "$format_label"
    case "$format_number" in
        1) esp_formatted=1 ;;
        2) xbootldr_formatted=1 ;;
        *) die 'formatted GPT topology partition number is invalid' ;;
    esac
    verify_gpt_whole_disk_identity
    reauthenticate_generated_partition \
        "$format_device" "$format_number" "$format_type" \
        "$format_start" "$format_size" "$format_devnum" "$format_partuuid" 0
    authenticate_partition_filesystem "$format_device" "$format_label"
}

mount_gpt_partition() (
    mount_partition_device=$1
    mount_partition_devnum=$2
    mount_partition_path=$3
    mount_partition_number=$4
    mount_partition_type=$5
    mount_partition_start=$6
    mount_partition_size=$7
    mount_partition_partuuid=$8
    mount_partition_label=$9
    verify_init_mount_namespace
    authenticate_partition_filesystem \
        "$mount_partition_device" "$mount_partition_label"
    reauthenticate_generated_partition \
        "$mount_partition_device" "$mount_partition_number" "$mount_partition_type" \
        "$mount_partition_start" "$mount_partition_size" \
        "$mount_partition_devnum" "$mount_partition_partuuid" 0
    mount_partition_status=0
    run_bounded 30s "$mount_command" -i -t vfat \
        -o rw,nosuid,nodev,noexec,nosymfollow,uid=0,gid=0,fmask=0133,dmask=0022 \
        -- "$mount_partition_device" "$mount_partition_path" \
        || mount_partition_status=$?
    if gpt_mount_is_exact_target "$mount_partition_path" "$mount_partition_devnum"; then
        reauthenticate_generated_partition \
            "$mount_partition_device" "$mount_partition_number" \
            "$mount_partition_type" "$mount_partition_start" "$mount_partition_size" \
            "$mount_partition_devnum" "$mount_partition_partuuid" 1
        authenticate_partition_filesystem \
            "$mount_partition_device" "$mount_partition_label"
        [ "$mount_partition_status" -eq 0 ] \
            || die 'mount reported failure after exact GPT attachment appeared'
        return 0
    fi
    die 'GPT partition mount result is absent, inexact, or ambiguous'
)

unmount_gpt_partition() (
    unmount_partition_path=$1
    unmount_partition_devnum=$2
    verify_init_mount_namespace
    gpt_mount_is_exact_target "$unmount_partition_path" "$unmount_partition_devnum" \
        || die 'refusing to unmount an inexact GPT topology attachment'
    run_bounded 30s "$umount_command" -i -- "$unmount_partition_path" \
        || die 'bounded GPT topology unmount failed'
    [ "$(mountpoint_count "$unmount_partition_path")" = 0 ] \
        && [ "$(device_mount_count "$unmount_partition_devnum")" = 0 ] \
        || die 'GPT topology attachment remained after unmount'
)

write_topology_source() (
    source_topology=$1
    if [ ! -d "$topology_installation" ]; then
        "$mkdir_command" -m 700 -- "$topology_installation" \
            || die 'cannot create topology installation root'
        "$mkdir_command" -p -- "$topology_installation/etc/cast" \
            || die 'cannot create topology source parent'
        chmod 0755 "$topology_installation" "$topology_installation/etc" \
            "$topology_installation/etc/cast" \
            || die 'cannot establish topology source directory policy'
    fi
    source_temporary=$topology_installation/etc/cast/.boot-topology.glu.stage
    [ ! -e "$source_temporary" ] && [ ! -L "$source_temporary" ] \
        || die 'topology source staging name already exists'
    case "$source_topology" in
        alias)
            printf '%s\n%s\n' \
                'let cast = import! cast.boot_topology.v2' \
                "cast.boot_topology.aliases_esp { partuuid = \"$esp_partuuid\", mount_point = \"$esp_mount\" }" \
                >"$source_temporary"
            ;;
        distinct)
            printf '%s\n%s\n' \
                'let cast = import! cast.boot_topology.v2' \
                "cast.boot_topology.distinct { partuuid = \"$esp_partuuid\", mount_point = \"$esp_mount\" } { partuuid = \"$xbootldr_partuuid\", mount_point = \"$xbootldr_mount\" }" \
                >"$source_temporary"
            ;;
        *) die 'unknown topology source shape' ;;
    esac
    chmod 0644 -- "$source_temporary" \
        || die 'cannot establish topology source file policy'
    mv -T -- "$source_temporary" "$topology_source" \
        || die 'cannot publish topology source outside the target disk'
    [ "$(stat -Lc '%u:%g:%a:%F:%h' -- "$topology_source")" \
        = '0:0:644:regular file:1' ] \
        || die 'topology source metadata is unsafe'
)

run_gpt_topology_test() (
    topology_test_kind=$1
    topology_test_phase=$2
    [ "$publication_runner_prepared" -eq 1 ] \
        || die 'production publisher runner is not prepared'
    [ -n "$topology_test_confirmation" ] && [ -n "$topology_test_make_target" ] \
        || die 'production GPT publication profile is not configured'
    verify_immutable_publication_source
    verify_publication_develop_profile
    verify_publication_binary_manifest
    verify_gpt_whole_disk_identity
    reauthenticate_generated_partition \
        "$esp_device" 1 "$esp_type" 2048 524288 "$esp_devnum" "$esp_partuuid" 1
    authenticate_partition_filesystem "$esp_device" CASTESP
    gpt_mount_is_exact_target "$esp_mount" "$esp_devnum" \
        || die 'ESP attachment policy changed before production topology test'
    if [ "$topology_test_kind" = distinct ]; then
        reauthenticate_generated_partition \
            "$xbootldr_device" 2 "$xbootldr_type" 526336 1048576 \
            "$xbootldr_devnum" "$xbootldr_partuuid" 1
        authenticate_partition_filesystem "$xbootldr_device" CASTXBOOT
        gpt_mount_is_exact_target "$xbootldr_mount" "$xbootldr_devnum" \
            || die 'XBOOTLDR attachment policy changed before production topology test'
    fi
    topology_test_status=0
    "$env_command" -i \
        PATH=/usr/sbin:/usr/bin:/sbin:/bin \
        HOME=/root USER=root LOGNAME=root LC_ALL=C LANG=C TMPDIR=/tmp \
        "SSH_CONNECTION=$SSH_CONNECTION" \
        "CAST_VM_GPT_TOPOLOGY_CONFIRMATION=$topology_test_confirmation" \
        "CAST_VM_GPT_TOPOLOGY_KIND=$topology_test_kind" \
        "CAST_VM_GPT_TOPOLOGY_PHASE=$topology_test_phase" \
        "CAST_VM_GPT_TOPOLOGY_INSTALLATION=$topology_installation" \
        "CAST_VM_GPT_AGGREGATE_FIXTURE_PARENT=$aggregate_fixture_root/$topology_test_kind" \
        "CAST_VM_GPT_TOPOLOGY_ESP_MOUNT=$esp_mount" \
        "CAST_VM_GPT_TOPOLOGY_ESP_DEVNUM=$esp_devnum" \
        "CAST_VM_GPT_TOPOLOGY_ESP_PARTUUID=$esp_partuuid" \
        "CAST_VM_GPT_TOPOLOGY_XBOOTLDR_MOUNT=$xbootldr_mount" \
        "CAST_VM_GPT_TOPOLOGY_XBOOTLDR_DEVNUM=$xbootldr_devnum" \
        "CAST_VM_GPT_TOPOLOGY_XBOOTLDR_PARTUUID=$xbootldr_partuuid" \
        "CAST_VM_BOOT_PUBLICATION_EXPECTED_HOSTNAME=$expected_hostname" \
        "CAST_VM_BOOT_PUBLICATION_EXPECTED_MACHINE_ID=$expected_machine_id" \
        "CAST_VM_BOOT_PUBLICATION_EXPECTED_BOOT_ID=$expected_boot_id" \
        "CAST_VM_BOOT_PUBLICATION_EXPECTED_VIRTUALIZATION=$expected_virtualization" \
        "CAST_VM_BOOT_PUBLICATION_EXPECTED_SSH_SHA256=$ssh_connection_hash" \
        "CAST_VM_BOOT_PUBLICATION_EXPECTED_TARGET_DISK=$target_disk" \
        "CAST_VM_BOOT_PUBLICATION_EXPECTED_TARGET_STABLE_PATH=$target_stable_path" \
        "CAST_VM_BOOT_PUBLICATION_EXPECTED_TARGET_DISKSEQ=$target_diskseq" \
        "CAST_VM_BOOT_PUBLICATION_EXPECTED_TARGET_BYTES=$target_bytes" \
        "CAST_VM_BOOT_PUBLICATION_EXPECTED_COMMIT=$expected_commit" \
        "CAST_VM_BOOT_PUBLICATION_CONSUMED_MARKER=$consumed_marker" \
        "CAST_VM_BOOT_PUBLICATION_BUILD_ROOT=$publication_build_root" \
        "CAST_VM_BOOT_PUBLICATION_SOURCE_ROOT=$publication_source_root" \
        "CAST_VM_BOOT_PUBLICATION_DEVELOP_PROFILE=$publication_develop_profile" \
        "CAST_VM_BOOT_PUBLICATION_BINARY_MANIFEST=$publication_binary_manifest" \
        "CARGO_TARGET_DIR=$publication_test_target" \
        "CARGO_HOME=$publication_cargo_home" \
        "$make_command" -C "$publication_source_root" \
        "$topology_test_make_target" || topology_test_status=$?
    verify_gpt_whole_disk_identity
    reauthenticate_generated_partition \
        "$esp_device" 1 "$esp_type" 2048 524288 "$esp_devnum" "$esp_partuuid" 1
    authenticate_partition_filesystem "$esp_device" CASTESP
    gpt_mount_is_exact_target "$esp_mount" "$esp_devnum" \
        || die 'ESP attachment policy changed after production topology test'
    if [ "$topology_test_kind" = distinct ]; then
        reauthenticate_generated_partition \
            "$xbootldr_device" 2 "$xbootldr_type" 526336 1048576 \
            "$xbootldr_devnum" "$xbootldr_partuuid" 1
        authenticate_partition_filesystem "$xbootldr_device" CASTXBOOT
        gpt_mount_is_exact_target "$xbootldr_mount" "$xbootldr_devnum" \
            || die 'XBOOTLDR attachment policy changed after production topology test'
    fi
    [ "$topology_test_status" -eq 0 ] \
        || die "production GPT topology test failed during $topology_test_kind/$topology_test_phase"
)

check_partition_filesystem() (
    fsck_partition_device=$1
    fsck_partition_number=$2
    fsck_partition_type=$3
    fsck_partition_start=$4
    fsck_partition_size=$5
    fsck_partition_devnum=$6
    fsck_partition_partuuid=$7
    fsck_partition_label=$8
    reauthenticate_generated_partition \
        "$fsck_partition_device" "$fsck_partition_number" "$fsck_partition_type" \
        "$fsck_partition_start" "$fsck_partition_size" \
        "$fsck_partition_devnum" "$fsck_partition_partuuid" 0
    authenticate_partition_filesystem "$fsck_partition_device" "$fsck_partition_label"
    run_bounded 120s "$fsck_fat_command" -n -- "$fsck_partition_device" \
        || die 'read-only VFAT consistency check failed'
    reauthenticate_generated_partition \
        "$fsck_partition_device" "$fsck_partition_number" "$fsck_partition_type" \
        "$fsck_partition_start" "$fsck_partition_size" \
        "$fsck_partition_devnum" "$fsck_partition_partuuid" 0
    authenticate_partition_filesystem "$fsck_partition_device" "$fsck_partition_label"
)

run_alias_topology() {
    prepare_alias_partitions
    format_partition "$esp_device" CASTESP 1 "$esp_type" 2048 524288 \
        "$esp_devnum" "$esp_partuuid"
    esp_mounted=2
    mount_gpt_partition "$esp_device" "$esp_devnum" "$esp_mount" \
        1 "$esp_type" 2048 524288 "$esp_partuuid" CASTESP
    esp_mounted=1
    write_topology_source alias
    run_gpt_topology_test alias publish
    run_bounded 30s "$sync_command" -f "$esp_mount" \
        || die 'ESP alias durability barrier failed'
    unmount_gpt_partition "$esp_mount" "$esp_devnum"
    esp_mounted=0
    check_partition_filesystem "$esp_device" 1 "$esp_type" 2048 524288 \
        "$esp_devnum" "$esp_partuuid" CASTESP
    esp_mounted=2
    mount_gpt_partition "$esp_device" "$esp_devnum" "$esp_mount" \
        1 "$esp_type" 2048 524288 "$esp_partuuid" CASTESP
    esp_mounted=1
    run_gpt_topology_test alias revalidate
    run_bounded 30s "$sync_command" -f "$esp_mount" \
        || die 'ESP alias remount durability barrier failed'
    unmount_gpt_partition "$esp_mount" "$esp_devnum"
    esp_mounted=0
    check_partition_filesystem "$esp_device" 1 "$esp_type" 2048 524288 \
        "$esp_devnum" "$esp_partuuid" CASTESP
}

run_distinct_topology() {
    prepare_distinct_partitions
    format_partition "$esp_device" CASTESP 1 "$esp_type" 2048 524288 \
        "$esp_devnum" "$esp_partuuid"
    format_partition "$xbootldr_device" CASTXBOOT 2 "$xbootldr_type" \
        526336 1048576 "$xbootldr_devnum" "$xbootldr_partuuid"
    esp_mounted=2
    mount_gpt_partition "$esp_device" "$esp_devnum" "$esp_mount" \
        1 "$esp_type" 2048 524288 "$esp_partuuid" CASTESP
    esp_mounted=1
    xbootldr_mounted=2
    mount_gpt_partition "$xbootldr_device" "$xbootldr_devnum" "$xbootldr_mount" \
        2 "$xbootldr_type" 526336 1048576 "$xbootldr_partuuid" CASTXBOOT
    xbootldr_mounted=1
    write_topology_source distinct
    run_gpt_topology_test distinct publish
    run_bounded 30s "$sync_command" -f "$esp_mount" \
        || die 'distinct ESP durability barrier failed'
    run_bounded 30s "$sync_command" -f "$xbootldr_mount" \
        || die 'distinct XBOOTLDR durability barrier failed'
    unmount_gpt_partition "$xbootldr_mount" "$xbootldr_devnum"
    xbootldr_mounted=0
    unmount_gpt_partition "$esp_mount" "$esp_devnum"
    esp_mounted=0
    check_partition_filesystem "$esp_device" 1 "$esp_type" 2048 524288 \
        "$esp_devnum" "$esp_partuuid" CASTESP
    check_partition_filesystem "$xbootldr_device" 2 "$xbootldr_type" \
        526336 1048576 "$xbootldr_devnum" "$xbootldr_partuuid" CASTXBOOT
    esp_mounted=2
    mount_gpt_partition "$esp_device" "$esp_devnum" "$esp_mount" \
        1 "$esp_type" 2048 524288 "$esp_partuuid" CASTESP
    esp_mounted=1
    xbootldr_mounted=2
    mount_gpt_partition "$xbootldr_device" "$xbootldr_devnum" "$xbootldr_mount" \
        2 "$xbootldr_type" 526336 1048576 "$xbootldr_partuuid" CASTXBOOT
    xbootldr_mounted=1
    run_gpt_topology_test distinct revalidate
    run_bounded 30s "$sync_command" -f "$esp_mount" \
        || die 'distinct ESP remount durability barrier failed'
    run_bounded 30s "$sync_command" -f "$xbootldr_mount" \
        || die 'distinct XBOOTLDR remount durability barrier failed'
    unmount_gpt_partition "$xbootldr_mount" "$xbootldr_devnum"
    xbootldr_mounted=0
    unmount_gpt_partition "$esp_mount" "$esp_devnum"
    esp_mounted=0
    check_partition_filesystem "$esp_device" 1 "$esp_type" 2048 524288 \
        "$esp_devnum" "$esp_partuuid" CASTESP
    check_partition_filesystem "$xbootldr_device" 2 "$xbootldr_type" \
        526336 1048576 "$xbootldr_devnum" "$xbootldr_partuuid" CASTXBOOT
}

run_campaign() {
    configure_gpt_publication_profile
    [ "$target_bytes" -ge 2147483648 ] \
        || die 'GPT topology target is too small for the fixed bounded layouts'
    ensure_runtime_root
    verify_runtime_inventory marker
    verify_marker "$authorization_marker"
    verify_init_mount_namespace
    load_gpt_effect_commands
    mkdir -m 700 -- "$campaign_lock" \
        || die 'another campaign or failed campaign already owns the lock'
    trap cleanup_gpt_campaign EXIT
    trap 'exit 129' HUP
    trap 'exit 130' INT
    trap 'exit 143' TERM
    [ ! -e "$consumed_marker" ] && [ ! -L "$consumed_marker" ] \
        || die 'a consumed authorization marker already exists'
    mv -- "$authorization_marker" "$consumed_marker" \
        || die 'cannot consume the GPT authorization marker'
    authorization_consumed=1
    verify_marker "$consumed_marker"
    verify_runtime_inventory campaign
    mkdir -m 700 -- "$mount_root" \
        || die 'cannot create the private GPT topology mount root'
    mkdir -m 700 -- "$esp_mount" "$xbootldr_mount" \
        || die 'cannot create private GPT topology mountpoints'

    verify_guest_identity
    verify_init_mount_namespace
    verify_target_disk
    prepare_boot_file_publication_runner
    prepare_aggregate_fixture_parents
    verify_guest_identity
    verify_init_mount_namespace
    verify_target_disk
    printf '%s\n' \
        "AUTHORIZED_GPT_TARGET=$target_stable_path" \
        "AUTHORIZED_TARGET_DISKSEQ=$target_diskseq" \
        "AUTHORIZED_TARGET_NODE=$target_disk" \
        "AUTHORIZED_TARGET_BYTES=$target_bytes" \
        "AUTHORIZED_TARGET_DEVICE_NUMBER=$target_devnum" \
        "AUTHORIZED_GUEST_BOOT_ID=$expected_boot_id" \
        "AUTHORIZED_REPOSITORY_COMMIT=$expected_commit" >&2
    verify_marker "$consumed_marker"
    destructive_started=1
    run_alias_topology
    run_distinct_topology
    verify_guest_identity
    verify_init_mount_namespace
    verify_marker "$consumed_marker"
    verify_gpt_whole_disk_identity
    campaign_complete=1
    if [ "$campaign_profile" = gpt-receipt-bound-aggregate-v1 ]; then
        printf '%s\n' \
            'Disposable VM receipt-bound aggregate GPT campaign passed.' \
            'Immutable aggregate publication was revalidated after unmount/remount.' \
            'No promotion or reboot was requested or performed.'
    else
        printf '%s\n' \
            'Disposable VM GPT ESP/XBOOTLDR topology campaign passed.' \
            'ESP-as-BOOT and distinct ESP+XBOOTLDR survived unmount/remount.' \
            'No reboot was requested or performed.'
    fi
}
