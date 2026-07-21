timeout_command=
mkfs_command=
mount_command=
umount_command=
mkdir_command=
sync_command=

load_effect_commands() {
    timeout_command=$(command_path timeout)
    mkfs_command=$(command_path mkfs.fat)
    mount_command=$(command_path mount)
    umount_command=$(command_path umount)
    mkdir_command=$(command_path mkdir)
    sync_command=$(command_path sync)
}

run_bounded() {
    bound=$1
    shift
    # GNU timeout's default non-foreground mode owns the child process group.
    "$timeout_command" --signal=TERM --kill-after=5s "$bound" "$@"
}

mount_is_exact_identity() {
    identity=$(awk -v mountpoint="$mount_root" '
        $5 == mountpoint {
            for (i = 1; i <= NF; i += 1) {
                if ($i == "-") print $3 "|" $(i + 1)
            }
        }
    ' /proc/self/mountinfo)
    [ -n "$identity" ] || return 1
    case "$identity" in *'
'*) return 1 ;; esac
    [ "$identity" = "$target_devnum|vfat" ] || return 1
    [ "$(target_mount_count)" = 1 ] || return 1
}

mount_is_exact_target() {
    evidence=$(awk -v mountpoint="$mount_root" '
        $5 == mountpoint {
            for (i = 1; i <= NF; i += 1) {
                if ($i == "-") {
                    print $3 "|" $6 "|" $(i + 1) "|" $(i + 3)
                }
            }
        }
    ' /proc/self/mountinfo)
    [ -n "$evidence" ] || return 1
    case "$evidence" in *'
'*) return 1 ;; esac
    old_ifs=$IFS
    IFS='|'
    set -- $evidence
    IFS=$old_ifs
    [ "$#" -eq 4 ] || return 1
    [ "$1" = "$target_devnum" ] || return 1
    case ",$2," in *,rw,*) ;; *) return 1 ;; esac
    for required in nosuid nodev noexec nosymfollow; do
        case ",$2," in *,$required,*) ;; *) return 1 ;; esac
    done
    [ "$3" = vfat ] || return 1
    for required in rw fmask=0133 dmask=0022; do
        case ",$4," in *,$required,*) ;; *) return 1 ;; esac
    done
    super_options=$4
    old_ifs=$IFS
    IFS=,
    set -- $super_options
    IFS=$old_ifs
    for option do
        case "$option" in
            uid=0 | gid=0) ;;
            uid=* | gid=*) return 1 ;;
        esac
    done
    mount_root_metadata=$(stat -Lc '%u:%g:%F' -- "$mount_root") || return 1
    [ "$mount_root_metadata" = '0:0:directory' ] || return 1
    [ "$(target_mount_count)" = 1 ] || return 1
}

mounted_by_campaign=0
authorization_consumed=0
destructive_started=0
campaign_complete=0
cleanup_campaign() {
    status=$?
    trap - EXIT HUP INT TERM
    cleanup_failed=0
    if [ "$mounted_by_campaign" -ne 0 ]; then
        if mount_is_exact_identity; then
            run_bounded 30s "$umount_command" -i -- "$mount_root" \
                || cleanup_failed=1
            if [ "$(campaign_mountpoint_count)" != 0 ] \
                || [ "$(target_mount_count)" != 0 ]; then
                cleanup_failed=1
            else
                mounted_by_campaign=0
            fi
        elif [ "$(campaign_mountpoint_count)" = 0 ] \
            && [ "$(target_mount_count)" = 0 ]; then
            mounted_by_campaign=0
        else
            printf '%s\n' \
                'refusing cleanup: campaign mount no longer has the admitted identity' >&2
            cleanup_failed=1
        fi
    fi
    if [ "$campaign_complete" -eq 1 ]; then
        [ "$(campaign_mountpoint_count)" = 0 ] \
            && [ "$(target_mount_count)" = 0 ] || cleanup_failed=1
    fi
    if [ "$campaign_complete" -eq 1 ] && [ "$cleanup_failed" -eq 0 ]; then
        rmdir -- "$mount_root" || cleanup_failed=1
    fi
    if [ "$campaign_complete" -eq 1 ] && [ "$cleanup_failed" -eq 0 ]; then
        rmdir -- "$campaign_lock" || cleanup_failed=1
    fi
    if [ "$campaign_complete" -eq 1 ] && [ "$cleanup_failed" -eq 0 ]; then
        rm -f -- "$consumed_marker" || cleanup_failed=1
    fi
    if [ "$authorization_consumed" -eq 1 ] \
        && { [ "$campaign_complete" -ne 1 ] || [ "$cleanup_failed" -ne 0 ]; }; then
        printf '%s\n' \
            'leaving recovery sentinel state for fail-closed VM recovery' >&2
        if [ "$destructive_started" -eq 1 ]; then
            printf '%s\n' \
                'a destructive child started; the target state is not classified as complete' >&2
        fi
        status=1
    fi
    [ "$cleanup_failed" -eq 0 ] || status=1
    exit "$status"
}

create_publication_parent() {
    current=$mount_root
    old_ifs=$IFS
    IFS=/
    set -- $publication_parent
    IFS=$old_ifs
    for component do
        current=$current/$component
        if [ ! -e "$current" ] && [ ! -L "$current" ]; then
            run_bounded 10s "$mkdir_command" -- "$current" \
                || die 'cannot create the declared publication parent'
        fi
        [ -d "$current" ] && [ ! -L "$current" ] \
            || die 'declared publication parent is not a directory'
    done
}

verify_publication_parent() {
    current=$mount_root
    old_ifs=$IFS
    IFS=/
    set -- $publication_parent
    IFS=$old_ifs
    for component do
        current=$current/$component
        [ -d "$current" ] && [ ! -L "$current" ] \
            || die 'publication parent did not survive remount'
    done
}

mount_target() {
    verify_init_mount_namespace
    mount_status=0
    run_bounded 30s "$mount_command" -i -t vfat \
        -o rw,nosuid,nodev,noexec,nosymfollow,uid=0,gid=0,fmask=0133,dmask=0022 \
        -- "$target_disk" "$mount_root" || mount_status=$?
    if mount_is_exact_target; then
        mounted_by_campaign=1
        [ "$mount_status" -eq 0 ] \
            || die 'mount reported failure after publishing the exact admitted attachment'
        return 0
    fi
    if mount_is_exact_identity; then
        mounted_by_campaign=1
        die 'mounted target identity is exact but its policy is not admitted'
    fi
    if [ "$(campaign_mountpoint_count)" = 0 ] \
        && [ "$(target_mount_count)" = 0 ]; then
        mounted_by_campaign=0
        die 'cannot mount the admitted VFAT target'
    fi
    mounted_by_campaign=2
    die 'mount result is ambiguous and will not be treated as authority'
}

unmount_target() {
    verify_init_mount_namespace
    mount_is_exact_target \
        || die 'refusing to unmount a target whose identity or policy changed'
    unmount_status=0
    run_bounded 30s "$umount_command" -i -- "$mount_root" \
        || unmount_status=$?
    if [ "$(campaign_mountpoint_count)" = 0 ] \
        && [ "$(target_mount_count)" = 0 ]; then
        mounted_by_campaign=0
        [ "$unmount_status" -eq 0 ] \
            || die 'unmount reported failure after removing the exact attachment'
        return 0
    fi
    if mount_is_exact_target; then
        mounted_by_campaign=1
        die 'target remains mounted after the bounded unmount'
    fi
    mounted_by_campaign=2
    die 'unmount result is ambiguous and will not be retried'
}

run_campaign() {
    ensure_runtime_root
    verify_runtime_inventory marker
    verify_marker "$authorization_marker"
    verify_init_mount_namespace
    load_effect_commands
    mkdir -m 700 -- "$campaign_lock" \
        || die 'another campaign or failed campaign already owns the lock'
    trap cleanup_campaign EXIT
    trap 'exit 129' HUP
    trap 'exit 130' INT
    trap 'exit 143' TERM
    [ ! -e "$consumed_marker" ] && [ ! -L "$consumed_marker" ] \
        || die 'a consumed authorization marker already exists'
    mv -- "$authorization_marker" "$consumed_marker" \
        || die 'cannot consume the authorization marker'
    authorization_consumed=1
    verify_marker "$consumed_marker"
    verify_runtime_inventory campaign
    mkdir -m 700 -- "$mount_root" \
        || die 'cannot create the private campaign mountpoint'

    verify_guest_identity
    verify_init_mount_namespace
    verify_target_disk
    printf '%s\n' \
        "AUTHORIZED_DESTRUCTIVE_TARGET=$target_stable_path" \
        "AUTHORIZED_TARGET_DISKSEQ=$target_diskseq" \
        "AUTHORIZED_TARGET_NODE=$target_disk" \
        "AUTHORIZED_TARGET_BYTES=$target_bytes" \
        "AUTHORIZED_TARGET_DEVICE_NUMBER=$target_devnum" \
        "AUTHORIZED_GUEST_BOOT_ID=$expected_boot_id" \
        "AUTHORIZED_REPOSITORY_COMMIT=$expected_commit" >&2
    # Literal last pre-effect authority check: it also rechecks freshness.
    verify_marker "$consumed_marker"
    destructive_started=1
    run_bounded 120s "$mkfs_command" -I --mbr=n -F 32 -n "$filesystem_label" \
        -- "$target_disk" \
        || die 'bounded VFAT creation failed on the exact admitted target'
    verify_target_disk

    mount_target
    create_publication_parent
    run_bounded 30s "$sync_command" -f "$mount_root" \
        || die 'bounded first VFAT durability barrier failed'
    unmount_target
    mount_target
    verify_publication_parent
    run_bounded 30s "$sync_command" -f "$mount_root" \
        || die 'bounded remount durability barrier failed'
    unmount_target
    campaign_complete=1
    printf '%s\n' \
        'Disposable VM VFAT foundation campaign passed.' \
        "Persistent parent: $publication_parent" \
        'No reboot was requested or performed.'
}
