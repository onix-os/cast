#!/bin/sh

set -eu

parent_loss_cleanup=
parent_loss_unit=
parent_loss_marker=
parent_loss_bound=
post_kill_setsid=
post_kill_shell=
if [ "$#" -lt 7 ]; then
    printf '%s\n' \
        "usage: $0 <ready-file> <ack-file> <channels-ready-file> <status-fifo> <release-fifo> [--parent-loss-cleanup <executable> <unit> <marker> <bound>] -- <command> [argument ...]" >&2
    exit 2
fi
ready_file=$1
ack_file=$2
channels_ready_file=$3
status_fifo=$4
release_fifo=$5
if [ "$#" -ge 7 ] && [ "$6" = -- ]; then
    shift 6
elif [ "$#" -ge 12 ] && [ "$6" = --parent-loss-cleanup ] \
    && [ "${11}" = -- ]; then
    parent_loss_cleanup=$7
    parent_loss_unit=$8
    parent_loss_marker=$9
    parent_loss_bound=${10}
    shift 11
else
    printf '%s\n' \
        "usage: $0 <ready-file> <ack-file> <channels-ready-file> <status-fifo> <release-fifo> [--parent-loss-cleanup <executable> <unit> <marker> <bound>] -- <command> [argument ...]" >&2
    exit 2
fi
kill_after_seconds=${CAST_LATCHED_KILL_AFTER_SECONDS:-30}

if [ -n "$parent_loss_cleanup" ]; then
    case "$parent_loss_cleanup" in
        /*) ;;
        *) printf 'parent-loss cleanup executable must be absolute: %s\n' \
            "$parent_loss_cleanup" >&2; exit 2 ;;
    esac
    if [ -L "$parent_loss_cleanup" ] || [ ! -f "$parent_loss_cleanup" ] \
        || [ ! -x "$parent_loss_cleanup" ]; then
        printf 'parent-loss cleanup executable is unavailable or unsafe: %s\n' \
            "$parent_loss_cleanup" >&2
        exit 1
    fi
    post_kill_setsid=$(command -v setsid) || {
        printf 'setsid is required for post-KILL fixture cleanup\n' >&2
        exit 1
    }
    post_kill_shell=$(command -v sh) || {
        printf 'sh is required for post-KILL fixture cleanup\n' >&2
        exit 1
    }
    case "$post_kill_setsid:$post_kill_shell" in
        /*:/*) ;;
        *) printf 'post-KILL cleanup commands must resolve absolutely\n' >&2; exit 1 ;;
    esac
fi

for private_path in \
    "$ready_file" "$ack_file" "$channels_ready_file" \
    "$status_fifo" "$release_fifo"
do
    case "$private_path" in
        /*) ;;
        *) printf 'latched command path must be absolute: %s\n' "$private_path" >&2; exit 2 ;;
    esac
done
case "$kill_after_seconds" in
    ''|*[!0-9]*) printf 'CAST_LATCHED_KILL_AFTER_SECONDS must be decimal\n' >&2; exit 2 ;;
esac
case "$kill_after_seconds" in
    0|0*|????*)
        printf 'CAST_LATCHED_KILL_AFTER_SECONDS must be between 1 and 300\n' >&2
        exit 2
        ;;
esac
if [ "$kill_after_seconds" -lt 1 ] || [ "$kill_after_seconds" -gt 300 ]; then
    printf 'CAST_LATCHED_KILL_AFTER_SECONDS must be between 1 and 300\n' >&2
    exit 2
fi
if [ -n "$parent_loss_cleanup" ]; then
    case "$parent_loss_bound" in
        ''|*[!0-9]*)
            printf 'parent-loss cleanup bound must be decimal\n' >&2
            exit 2
            ;;
        0|0*|????*)
            printf 'parent-loss cleanup bound must be between 1 and 300\n' >&2
            exit 2
            ;;
    esac
    if [ "$parent_loss_bound" -lt 1 ] || [ "$parent_loss_bound" -gt 300 ]; then
        printf 'parent-loss cleanup bound must be between 1 and 300\n' >&2
        exit 2
    fi
    if [ "$parent_loss_bound" != "$kill_after_seconds" ]; then
        printf 'parent-loss cleanup bound must equal the latched KILL bound\n' >&2
        exit 2
    fi
fi
if [ -e "$ready_file" ] || [ -L "$ready_file" ]; then
    printf 'latched command ready path already exists: %s\n' "$ready_file" >&2
    exit 1
fi
for absent_path in "$ack_file" "$channels_ready_file"; do
    if [ -e "$absent_path" ] || [ -L "$absent_path" ]; then
        printf 'latched command private path already exists: %s\n' "$absent_path" >&2
        exit 1
    fi
done
for fifo in "$status_fifo" "$release_fifo"; do
    if [ -L "$fifo" ] || [ ! -p "$fifo" ]; then
        printf 'latched command channel must be a non-symlink FIFO: %s\n' "$fifo" >&2
        exit 1
    fi
    if [ "$(stat -c '%u:%a:%h' "$fifo")" != "$(id -u):600:1" ]; then
        printf 'latched command FIFO must be caller-owned, mode 600, and singly linked: %s\n' \
            "$fifo" >&2
        exit 1
    fi
done

require_group_leader() {
    IFS= read -r process_stat <"/proc/$$/stat" || return 1
    process_tail=${process_stat##*) }
    set -- $process_tail
    [ "$3" = "$$" ] && [ "$4" = "$$" ]
}
if ! require_group_leader; then
    printf 'latched command must be its own Linux session and process-group leader\n' >&2
    exit 1
fi

termination_status=
command_pid=
watchdog_pid=
release_monitor_pid=
release_result_open=0
release_monitor_result=
release_monitor_status=1
parent_loss=0
termination_complete="${ack_file}.termination-complete.$$"
command_complete="${channels_ready_file}.command-complete.$$"
run_parent_loss_cleanup() {
    [ -n "$parent_loss_cleanup" ] || return 0
    "$parent_loss_cleanup" \
        "$parent_loss_unit" "$parent_loss_marker" "$parent_loss_bound"
}
stop_release_monitor() {
    [ -n "$release_monitor_pid" ] || return 0
    kill -TERM "$release_monitor_pid" >/dev/null 2>&1 || :
}
wait_release_monitor() {
    release_monitor_result=monitor-failed
    if [ "$release_result_open" -eq 1 ]; then
        IFS= read -r release_monitor_result <&6 || release_monitor_result=monitor-failed
        exec 6<&-
        release_result_open=0
    fi
    if [ -n "$release_monitor_pid" ]; then
        if wait "$release_monitor_pid" 2>/dev/null; then
            release_monitor_status=0
        else
            release_monitor_status=$?
        fi
        release_monitor_pid=
    fi
}
cleanup_private_monitor() {
    cleanup_status=$?
    trap - EXIT
    trap '' HUP INT TERM USR1
    set +e
    stop_release_monitor
    if [ -n "$release_monitor_pid" ]; then
        wait "$release_monitor_pid" 2>/dev/null || :
        release_monitor_pid=
    fi
    if [ "$release_result_open" -eq 1 ]; then
        exec 6<&-
        release_result_open=0
    fi
    rm -f "$command_complete"
    exit "$cleanup_status"
}
forward_termination() {
    [ -n "$termination_status" ] || return 0
    [ -n "$command_pid" ] || return 0
    [ -z "$watchdog_pid" ] || return 0

    # Ignore the forwarded group TERM in this leader, but deliver it to the
    # already-captured payload. A private watchdog bounds an ignoring payload.
    trap '' HUP INT TERM
    kill -TERM "-$$" >/dev/null 2>&1 || :
    (
        exec 3>&-
        exec 4<&-
        exec 5>&-
        exec 6<&-
        exec 7>&-
        watchdog_attempt=0
        watchdog_bound=$((kill_after_seconds * 20))
        while [ ! -e "$termination_complete" ]; do
            watchdog_attempt=$((watchdog_attempt + 1))
            if [ "$watchdog_attempt" -gt "$watchdog_bound" ]; then
                kill -KILL "-$$" >/dev/null 2>&1 || :
                exit 0
            fi
            sleep 0.05
        done
    ) &
    watchdog_pid=$!
}
remember_signal() {
    [ -n "$termination_status" ] || termination_status=$1
    stop_release_monitor
    forward_termination
}
remember_parent_loss() {
    parent_loss=1
    [ -n "$termination_status" ] || termination_status=1
}
exit_if_termination_pending() {
    [ -n "$termination_status" ] || return 0
    if [ "$parent_loss" -eq 0 ]; then
        stop_release_monitor
    fi
    wait_release_monitor
    if [ "$parent_loss" -eq 1 ]; then
        run_parent_loss_cleanup || :
    fi
    exit "$termination_status"
}
trap 'remember_signal 129' HUP
trap 'remember_signal 130' INT
trap 'remember_signal 143' TERM
trap remember_parent_loss USR1

# Establish both channels before publishing readiness. A dedicated monitor is
# the only release-channel reader, so parent EOF is observed even while the
# supervisor is awaiting acknowledgement or a long-running command.
exec 3>"$status_fifo"
exec 4<"$release_fifo"
# The parent opened temporary read/write anchors only to make this handshake
# nonblocking. Drop inherited copies so parent death produces real FIFO EOF.
exec 5>&-
exec 6>&-
release_result_fifo="${ack_file}.release-result.$$"
if [ -e "$release_result_fifo" ] || [ -L "$release_result_fifo" ] \
    || [ -e "$command_complete" ] || [ -L "$command_complete" ]; then
    printf 'latched command private monitor path already exists\n' >&2
    exit 1
fi
mkfifo -m 600 "$release_result_fifo"
if [ -L "$release_result_fifo" ] || [ ! -p "$release_result_fifo" ] \
    || [ "$(stat -c '%u:%a:%h' "$release_result_fifo")" != "$(id -u):600:1" ]; then
    printf 'latched command monitor result channel is unsafe\n' >&2
    exit 1
fi
# A temporary Linux read/write anchor makes both one-way result endpoints
# nonblocking. Unlinking the FIFO before launch prevents payload forgery.
exec 5<>"$release_result_fifo"
exec 6<"$release_result_fifo"
release_result_open=1
exec 7>"$release_result_fifo"
rm -f "$release_result_fifo"
supervisor_pid=$$
(
    trap - HUP INT TERM
    trap '' USR1
    exec 3>&-
    exec 5>&-
    exec 6<&-
    release_record=
    extra_record=
    if IFS= read -r release_record <&4; then
        if IFS= read -r extra_record <&4; then
            monitor_result=invalid-release
        elif [ "$release_record" = release ] \
            && [ -f "$command_complete" ] && [ ! -L "$command_complete" ]; then
            monitor_result=release
        else
            monitor_result=invalid-release
        fi
    else
        monitor_result=parent-eof
    fi
    exec 4<&-
    case "$monitor_result" in
        release)
            printf '%s\n' "$monitor_result" >&7 || exit 1
            monitor_status=0
            ;;
        *)
            # The leader may already be gone, which also closes the only result
            # reader. Cleanup must not depend on this diagnostic write.
            trap '' PIPE
            printf '%s\n' "$monitor_result" >&7 || :
            # Notify first so the supervisor cannot launch new work while this
            # child performs the first authenticated unit cleanup pass.
            # The monitor itself pins this process-group identity and ignores
            # USR1, so group delivery cannot target a reused numeric PID if the
            # supervisor leader has already died unexpectedly.
            kill -USR1 "-$supervisor_pid" >/dev/null 2>&1 || :
            cleanup_failed=0
            run_parent_loss_cleanup || cleanup_failed=1
            # Parent loss must not depend on the supervisor trap running: the
            # leader may be frozen. This monitor pins the PGID, ignores its own
            # TERM, closes the registration window, repeats authenticated unit
            # cleanup, and finally KILLs any non-zombie group members left
            # beyond the configured bound.
            trap '' HUP INT TERM USR1
            kill -TERM "-$supervisor_pid" >/dev/null 2>&1 || :
            run_parent_loss_cleanup || cleanup_failed=1
            IFS= read -r monitor_stat < /proc/self/stat
            monitor_pid=${monitor_stat%% *}
            monitor_attempt=0
            monitor_bound=$((kill_after_seconds * 20))
            while :; do
                live_member=0
                for stat_file in /proc/[0-9]*/stat; do
                    IFS= read -r process_stat <"$stat_file" 2>/dev/null || continue
                    process_pid=${process_stat%% *}
                    process_tail=${process_stat##*) }
                    set -- $process_tail
                    process_state=${1-}
                    process_group=${3-}
                    case "$process_state" in
                        Z|X|"") ;;
                        *)
                            if [ "$process_group" = "$supervisor_pid" ] \
                                && [ "$process_pid" != "$monitor_pid" ]; then
                                live_member=1
                                break
                            fi
                            ;;
                    esac
                done
                [ "$live_member" -eq 1 ] || break
                monitor_attempt=$((monitor_attempt + 1))
                if [ "$monitor_attempt" -gt "$monitor_bound" ]; then
                    if [ -n "$parent_loss_cleanup" ]; then
                        "$post_kill_setsid" "$post_kill_shell" -c '
                            trap "" HUP INT TERM USR1
                            target_group=$1
                            stop_bound=$2
                            cleanup_executable=$3
                            cleanup_unit=$4
                            cleanup_marker=$5
                            exec 3>&-
                            exec 4<&-
                            exec 5>&-
                            exec 6<&-
                            exec 7>&-
                            drain_attempt=0
                            drain_bound=$((stop_bound * 20))
                            while :; do
                                live_member=0
                                for stat_file in /proc/[0-9]*/stat; do
                                    IFS= read -r process_stat <"$stat_file" 2>/dev/null \
                                        || continue
                                    process_tail=${process_stat##*) }
                                    set -- $process_tail
                                    process_state=${1-}
                                    process_group=${3-}
                                    case "$process_state" in
                                        Z|X|"") ;;
                                        *)
                                            if [ "$process_group" = "$target_group" ]; then
                                                live_member=1
                                                break
                                            fi
                                            ;;
                                    esac
                                done
                                [ "$live_member" -eq 1 ] || break
                                drain_attempt=$((drain_attempt + 1))
                                [ "$drain_attempt" -le "$drain_bound" ] || break
                                sleep 0.05
                            done
                            "$cleanup_executable" \
                                "$cleanup_unit" "$cleanup_marker" "$stop_bound"
                        ' post-kill-fixture-cleanup \
                            "$supervisor_pid" "$parent_loss_bound" \
                            "$parent_loss_cleanup" "$parent_loss_unit" \
                            "$parent_loss_marker" \
                            </dev/null >/dev/null 2>&1 &
                        post_kill_pid=$!
                        post_kill_attempt=0
                        post_kill_detached=0
                        while [ "$post_kill_detached" -eq 0 ]; do
                            if IFS= read -r post_kill_stat \
                                <"/proc/$post_kill_pid/stat" 2>/dev/null; then
                                post_kill_tail=${post_kill_stat##*) }
                                set -- $post_kill_tail
                                post_kill_state=${1-}
                                post_kill_group=${3-}
                                post_kill_session=${4-}
                                if [ "$post_kill_state" != Z ] \
                                    && [ "$post_kill_state" != X ] \
                                    && [ "$post_kill_group" = "$post_kill_pid" ] \
                                    && [ "$post_kill_session" = "$post_kill_pid" ]; then
                                    post_kill_detached=1
                                    break
                                fi
                            fi
                            post_kill_attempt=$((post_kill_attempt + 1))
                            if [ "$post_kill_attempt" -gt 100 ]; then
                                kill -KILL "$post_kill_pid" >/dev/null 2>&1 || :
                                wait "$post_kill_pid" 2>/dev/null || :
                                run_parent_loss_cleanup || cleanup_failed=1
                                break
                            fi
                            sleep 0.01
                        done
                    fi
                    kill -KILL "-$supervisor_pid" >/dev/null 2>&1 || :
                    exit 1
                fi
                sleep 0.05
            done
            # A request can be accepted after both pre-drain ownership probes
            # but before the client finally leaves this process group. Close
            # that registration window with one last authenticated stop after
            # every other live group member is gone.
            run_parent_loss_cleanup || cleanup_failed=1
            monitor_status=1
            ;;
    esac
    exec 7>&-
    exit "$monitor_status"
) </dev/null >/dev/null 2>&1 &
release_monitor_pid=$!
exec 4<&-
exec 5>&-
exec 7>&-
trap cleanup_private_monitor EXIT

# Readiness now proves both that `$!` names this live group leader and that a
# second live group member pins the PGID until the parent closes release.
ready_temporary="${ready_file}.tmp.$$"
umask 077
( set -C; printf '%s\n' "$$" >"$ready_temporary" ) || exit 1
mv "$ready_temporary" "$ready_file"

acknowledgement=
attempt=0
while [ ! -e "$ack_file" ]; do
    exit_if_termination_pending
    attempt=$((attempt + 1))
    if [ "$attempt" -gt 200 ]; then
        printf 'latched command readiness was not acknowledged within 10 seconds\n' >&2
        exit 1
    fi
    sleep 0.05
done
if [ -L "$ack_file" ] || [ ! -f "$ack_file" ] \
    || [ "$(stat -c '%u:%a:%h' "$ack_file")" != "$(id -u):600:1" ]; then
    printf 'latched command acknowledgement is not a safe caller-owned file\n' >&2
    exit 1
fi
IFS= read -r acknowledgement <"$ack_file" || :
if [ "$acknowledgement" != ready ]; then
    printf 'latched command acknowledgement is invalid\n' >&2
    exit 1
fi
exit_if_termination_pending

channels_temporary="${channels_ready_file}.tmp.$$"
( set -C; printf '%s\n' "$$" >"$channels_temporary" ) || exit 1
mv "$channels_temporary" "$channels_ready_file"
exit_if_termination_pending
(
    exec 3>&-
    exec 4<&-
    exec 5>&-
    exec 6<&-
    exec 7>&-
    CAST_LATCHED_SUPERVISOR_PID=$$
    export CAST_LATCHED_SUPERVISOR_PID
    exec "$@"
) &
command_pid=$!
forward_termination

set +e
wait "$command_pid"
command_status=$?
set -e
if [ -n "$termination_status" ]; then
    if [ "$parent_loss" -eq 1 ]; then
        wait_release_monitor
    fi
    forward_termination
    wait "$command_pid" 2>/dev/null || :
    if [ -n "$watchdog_pid" ]; then
        : >"$termination_complete"
        wait "$watchdog_pid" 2>/dev/null || :
        rm -f "$termination_complete"
    fi
    if [ "$parent_loss" -eq 0 ]; then
        wait_release_monitor
    else
        # Catch the registration race between the monitor's first ownership
        # query and termination of the systemd-run client.
        run_parent_loss_cleanup || :
    fi
    # This is deliberately the supervisor's final output. It records the exact
    # ownership this helper can prove: its direct command and private watchdog
    # were reaped. Independent service cgroups may still own output writers.
    printf 'latched supervisor reaped command and watchdog (status %s)\n' \
        "$termination_status"
    exit "$termination_status"
fi
command_pid=

case "$command_status" in
    ''|*[!0-9]*) printf 'latched command produced an invalid exit status\n' >&2; exit 1 ;;
esac
if [ "$command_status" -gt 255 ]; then
    printf 'latched command exit status exceeds 255: %s\n' "$command_status" >&2
    exit 1
fi
# The completion record is private evidence for the sole release reader: an
# exact `release` is invalid if it arrives before this point.
command_complete_temporary="${command_complete}.tmp"
( set -C; printf 'complete\n' >"$command_complete_temporary" ) || exit 1
mv "$command_complete_temporary" "$command_complete"
if [ -n "$termination_status" ]; then
    wait_release_monitor
    if [ "$parent_loss" -eq 1 ]; then
        run_parent_loss_cleanup || :
    fi
    exit "$termination_status"
fi
status_published=1
trap '' PIPE
printf '%s\n' "$command_status" >&3 || status_published=0
trap - PIPE
exec 3>&-

# The leader remains alive after reporting completion. Only record-capable
# parent traps are active while release, reap, and numeric ownership clearing
# happen, so no cleanup can target a PID/PGID after it becomes reusable.
wait_release_monitor
rm -f "$command_complete"
if [ -n "$termination_status" ]; then
    if [ "$parent_loss" -eq 1 ]; then
        run_parent_loss_cleanup || :
    fi
    exit "$termination_status"
fi
if [ "$status_published" -ne 1 ] \
    || [ "$release_monitor_result" != release ] \
    || [ "$release_monitor_status" -ne 0 ]; then
    printf 'latched command parent closed without a valid release record\n' >&2
    exit 1
fi
exit "$command_status"
