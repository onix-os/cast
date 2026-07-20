#!/bin/sh

set -eu

root=$(CDPATH= cd -- "$(dirname -- "$0")/../.." && pwd -P)
runner="$root/misc/scripts/run-latched-command.sh"
work=$(mktemp -d "${TMPDIR:-/tmp}/cast-latched-command-test.XXXXXXXXXXXX")
active_group=
active_child=
sentinel_pid=

process_is_live() {
    process_pid=$1
    IFS= read -r process_stat 2>/dev/null <"/proc/$process_pid/stat" || return 1
    process_tail=${process_stat##*) }
    process_state=${process_tail%% *}
    case "$process_state" in
        Z|X|'') return 1 ;;
        *) return 0 ;;
    esac
}

process_state() {
    process_pid=$1
    IFS= read -r process_stat 2>/dev/null <"/proc/$process_pid/stat" || return 1
    process_tail=${process_stat##*) }
    printf '%s\n' "${process_tail%% *}"
}

group_has_live_member() {
    target_group=$1
    for stat_file in /proc/[0-9]*/stat; do
        IFS= read -r process_stat 2>/dev/null <"$stat_file" || continue
        process_tail=${process_stat##*) }
        set -- $process_tail
        member_state=${1-}
        member_group=${3-}
        case "$member_state" in
            Z|X|'') ;;
            *) [ "$member_group" != "$target_group" ] || return 0 ;;
        esac
    done
    return 1
}

wait_for_file() {
    waited_path=$1
    waited_attempt=0
    while [ ! -f "$waited_path" ]; do
        waited_attempt=$((waited_attempt + 1))
        [ "$waited_attempt" -le 500 ] || return 1
        sleep 0.01
    done
}

wait_for_process_exit() {
    waited_pid=$1
    waited_attempt=0
    while process_is_live "$waited_pid"; do
        waited_attempt=$((waited_attempt + 1))
        [ "$waited_attempt" -le 500 ] || return 1
        sleep 0.01
    done
}

wait_for_group_exit() {
    waited_group=$1
    waited_attempt=0
    while group_has_live_member "$waited_group"; do
        waited_attempt=$((waited_attempt + 1))
        [ "$waited_attempt" -le 300 ] || return 1
        sleep 0.01
    done
}

cleanup() {
    cleanup_status=$?
    trap - EXIT HUP INT TERM
    set +e
    if [ -n "$active_group" ] && group_has_live_member "$active_group"; then
        kill -CONT "-$active_group" >/dev/null 2>&1 || :
        kill -KILL "-$active_group" >/dev/null 2>&1 || :
    fi
    if [ -n "$active_child" ]; then
        wait "$active_child" 2>/dev/null || :
    fi
    if [ -n "$sentinel_pid" ]; then
        kill -KILL "-$sentinel_pid" >/dev/null 2>&1 || :
        wait "$sentinel_pid" 2>/dev/null || :
    fi
    if [ "$cleanup_status" -ne 0 ]; then
        for helper_log in "$work"/*/helper.log; do
            [ ! -f "$helper_log" ] || cat "$helper_log" >&2
        done
    fi
    rm -rf "$work"
    exit "$cleanup_status"
}
trap cleanup EXIT
trap 'exit 129' HUP
trap 'exit 130' INT
trap 'exit 143' TERM

fail() {
    printf 'latched command fault test failed: %s\n' "$1" >&2
    exit 1
}

cat >"$work/cleanup-callback" <<'EOF'
#!/bin/sh
set -eu
: "${LATCHED_TEST_CLEANUP_LOG:?}"
: "${LATCHED_TEST_CLEANUP_STATUS:?}"
callback_arguments=$*
group_file="${LATCHED_TEST_CLEANUP_LOG}.group"
if [ ! -f "$group_file" ]; then
    IFS= read -r parent_stat <"/proc/$PPID/stat"
    parent_tail=${parent_stat##*) }
    set -- $parent_tail
    ( set -C; printf '%s\n' "${3-}" >"$group_file" ) 2>/dev/null || :
fi
IFS= read -r target_group <"$group_file"
drain_state=drained
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
            if [ "$process_group" = "$target_group" ] \
                && [ "$process_pid" != "$$" ] \
                && [ "$process_pid" != "$PPID" ]; then
                drain_state=live
                break
            fi
            ;;
    esac
done
printf '%s %s\n' "$callback_arguments" "$drain_state" \
    >>"$LATCHED_TEST_CLEANUP_LOG"
exit "$LATCHED_TEST_CLEANUP_STATUS"
EOF
chmod 755 "$work/cleanup-callback"
cleanup_callback="$work/cleanup-callback"

set +e
setsid "$runner" \
    "$work/duplicate-ready" "$work/duplicate-ack" "$work/duplicate-channels" \
    "$work/duplicate-status" "$work/duplicate-release" \
    --close-payload-fd-9 --close-payload-fd-9 -- true \
    >"$work/duplicate-close-option.out" 2>&1
duplicate_close_status=$?
setsid "$runner" \
    "$work/missing-separator-ready" "$work/missing-separator-ack" \
    "$work/missing-separator-channels" "$work/missing-separator-status" \
    "$work/missing-separator-release" --close-payload-fd-9 true \
    >"$work/missing-close-separator.out" 2>&1
missing_close_separator_status=$?
setsid "$runner" \
    "$work/missing-command-ready" "$work/missing-command-ack" \
    "$work/missing-command-channels" "$work/missing-command-status" \
    "$work/missing-command-release" --close-payload-fd-9 -- \
    >"$work/missing-close-command.out" 2>&1
missing_close_command_status=$?
set -e
[ "$duplicate_close_status" -eq 2 ] \
    || fail 'duplicate close-payload-fd-9 option was accepted'
[ "$missing_close_separator_status" -eq 2 ] \
    || fail 'close-payload-fd-9 option without a separator was accepted'
[ "$missing_close_command_status" -eq 2 ] \
    || fail 'close-payload-fd-9 option without a command was accepted'

for invalid_bound in 0 01 301 999999999999999999999999; do
    set +e
    CAST_LATCHED_KILL_AFTER_SECONDS=1 setsid "$runner" \
        "$work/invalid-ready" "$work/invalid-ack" "$work/invalid-channels" \
        "$work/invalid-status" "$work/invalid-release" \
        --parent-loss-cleanup "$cleanup_callback" \
        "cast-fixtures-ci-$(id -u)-$$-Invalid.service" \
        CAST_FIXTURE_CI_UNIT_TOKEN=Invalid "$invalid_bound" -- true \
        >"$work/invalid-bound-$invalid_bound.out" 2>&1
    invalid_status=$?
    set -e
    [ "$invalid_status" -eq 2 ] \
        || fail "parent-loss bound $invalid_bound was accepted"
done

set +e
CAST_LATCHED_KILL_AFTER_SECONDS=2 setsid "$runner" \
    "$work/mismatch-ready" "$work/mismatch-ack" "$work/mismatch-channels" \
    "$work/mismatch-status" "$work/mismatch-release" \
    --parent-loss-cleanup "$cleanup_callback" \
    "cast-fixtures-ci-$(id -u)-$$-Mismatch.service" \
    CAST_FIXTURE_CI_UNIT_TOKEN=Mismatch 1 -- true \
    >"$work/mismatched-bound.out" 2>&1
mismatch_status=$?
set -e
[ "$mismatch_status" -eq 2 ] \
    || fail 'mismatched callback and KILL bounds were accepted'

prepare_case() {
    case_name=$1
    case_directory="$work/$case_name"
    mkdir -m 700 "$case_directory"
    ready_file="$case_directory/ready"
    acknowledgement_file="$case_directory/acknowledged"
    channels_ready_file="$case_directory/channels-ready"
    status_fifo="$case_directory/status.fifo"
    release_fifo="$case_directory/release.fifo"
    cleanup_log="$case_directory/cleanup.log"
    helper_log="$case_directory/helper.log"
    mkfifo -m 600 "$status_fifo" "$release_fifo"
    exec 5<>"$status_fifo"
    exec 6<>"$release_fifo"
}

launch_case() {
    cleanup_status=$1
    shift
    case "${LATCHED_TEST_CLOSE_PAYLOAD_FD_9-0}" in
        0) set -- -- "$@" ;;
        1) set -- --close-payload-fd-9 -- "$@" ;;
        *) fail "$case_name received an invalid close-payload-fd-9 test mode" ;;
    esac
    case_unit="cast-fixtures-ci-$(id -u)-$$-${case_name}.service"
    case_marker="CAST_FIXTURE_CI_UNIT_TOKEN=${case_name}"
    LATCHED_TEST_CLEANUP_LOG="$cleanup_log" \
    LATCHED_TEST_CLEANUP_STATUS="$cleanup_status" \
    CAST_LATCHED_KILL_AFTER_SECONDS=1 \
        setsid "$runner" \
        "$ready_file" "$acknowledgement_file" "$channels_ready_file" \
        "$status_fifo" "$release_fifo" \
        --parent-loss-cleanup "$cleanup_callback" \
        "$case_unit" "$case_marker" 1 "$@" \
        >"$helper_log" 2>&1 5>&- 6>&- &
    leader_pid=$!
    active_group=$leader_pid
    active_child=$leader_pid
}

await_readiness() {
    wait_for_file "$ready_file" || fail "$case_name readiness timed out"
    [ ! -L "$ready_file" ] || fail "$case_name readiness is a symlink"
    IFS= read -r ready_pid <"$ready_file" || fail "$case_name readiness is empty"
    [ "$ready_pid" = "$leader_pid" ] || fail "$case_name readiness PID mismatch"
}

acknowledge_and_await_channels() {
    acknowledgement_temporary="${acknowledgement_file}.tmp.$$"
    ( umask 077; set -C; printf 'ready\n' >"$acknowledgement_temporary" ) \
        || fail "$case_name could not write acknowledgement"
    mv "$acknowledgement_temporary" "$acknowledgement_file" \
        || fail "$case_name could not publish acknowledgement"
    wait_for_file "$channels_ready_file" || fail "$case_name channels timed out"
    [ ! -L "$channels_ready_file" ] || fail "$case_name channels are a symlink"
    IFS= read -r channels_pid <"$channels_ready_file" \
        || fail "$case_name channels receipt is empty"
    [ "$channels_pid" = "$leader_pid" ] || fail "$case_name channels PID mismatch"
}

open_parent_channels() {
    exec 7<"$status_fifo"
    exec 8>"$release_fifo"
    exec 5>&-
    exec 6>&-
}

read_status_with_bound() {
    if ! status_record=$(timeout --kill-after=1s 3s sh -c '
        IFS= read -r value <&7 || exit 1
        printf "%s\n" "$value"
    '); then
        fail "$case_name did not publish status within the bound"
    fi
}

expect_status_eof_with_bound() {
    set +e
    timeout --kill-after=1s 3s sh -c 'IFS= read -r value <&7' \
        >/dev/null 2>&1
    eof_status=$?
    set -e
    [ "$eof_status" -eq 1 ] || fail "$case_name status channel did not reach EOF"
}

reap_expected_status() {
    expected_status=$1
    wait_for_process_exit "$leader_pid" || fail "$case_name leader did not exit"
    set +e
    wait "$leader_pid"
    leader_status=$?
    set -e
    active_child=
    [ "$leader_status" -eq "$expected_status" ] \
        || fail "$case_name leader status $leader_status, expected $expected_status"
}

finish_group() {
    wait_for_group_exit "$leader_pid" || fail "$case_name left live PGID members"
    active_group=
}

assert_cleanup_called() {
    [ -s "$cleanup_log" ] || fail "$case_name skipped its cleanup callback"
}

assert_final_cleanup_called() {
    cleanup_attempt=0
    while [ ! -f "$cleanup_log" ] \
        || [ "$(wc -l <"$cleanup_log")" -lt 3 ]; do
        cleanup_attempt=$((cleanup_attempt + 1))
        [ "$cleanup_attempt" -le 300 ] \
            || fail "$case_name skipped its post-drain cleanup callback"
        sleep 0.01
    done
    tail -n 1 "$cleanup_log" | grep -Fq ' drained' \
        || fail "$case_name final cleanup ran before group drainage"
}

assert_sentinel_live() {
    process_is_live "$sentinel_pid" || fail "$case_name signalled the unrelated sentinel"
}

setsid sh -c '
    trap "" HUP INT TERM USR1
    while :; do sleep 1; done
' </dev/null >/dev/null 2>&1 &
sentinel_pid=$!
process_is_live "$sentinel_pid" || fail 'sentinel did not start'

# A completed command publishes one decimal status and accepts exactly one
# release record. Ordinary release must not invoke parent-loss cleanup.
prepare_case normal
launch_case 0 sh -c 'exit 23'
await_readiness
acknowledge_and_await_channels
open_parent_channels
read_status_with_bound
[ "$status_record" = 23 ] || fail "normal status was $status_record"
printf 'release\n' >&8 || fail 'normal release write failed'
exec 8>&-
exec 7<&-
reap_expected_status 23
finish_group
[ ! -e "$cleanup_log" ] || fail 'normal release invoked parent-loss cleanup'
assert_sentinel_live

exercise_payload_fd9_policy() {
    fd_case=$1
    fd_policy=$2
    prepare_case "$fd_case"
    exec 9>"$case_directory/inherited-fd9"
    if [ "$fd_policy" = closed ]; then
        LATCHED_TEST_CLOSE_PAYLOAD_FD_9=1 launch_case 0 sh -c \
            '[ ! -e "/proc/$$/fd/9" ]'
    else
        launch_case 0 sh -c '[ -e "/proc/$$/fd/9" ]'
    fi
    await_readiness
    acknowledge_and_await_channels
    open_parent_channels
    read_status_with_bound
    [ "$status_record" = 0 ] \
        || fail "$case_name payload FD9 policy returned $status_record"
    printf 'release\n' >&8 || fail "$case_name release write failed"
    exec 8>&-
    exec 7<&-
    reap_expected_status 0
    finish_group
    exec 9>&-
    [ ! -e "$cleanup_log" ] \
        || fail "$case_name release invoked parent-loss cleanup"
    assert_sentinel_live
}

# The option is narrow: legacy callers preserve descriptor inheritance, while
# an explicit close removes FD9 in the trusted pre-exec payload subshell.
exercise_payload_fd9_policy preservefd9 preserved
exercise_payload_fd9_policy closefd9 closed

# Kill the leader after readiness but before acknowledgement. Its private
# monitor must keep the PGID anchored, observe release EOF, and run cleanup
# even though the monitor result reader died with the leader.
prepare_case beforeack
launch_case 0 sh -c 'exec sleep 20'
await_readiness
[ ! -e "$channels_ready_file" ] || fail 'beforeack reached channel readiness'
kill -KILL "$leader_pid" || fail 'beforeack could not kill the leader'
reap_expected_status 137
group_has_live_member "$leader_pid" || fail 'beforeack lost its PGID anchor'
exec 5>&-
exec 6>&-
finish_group
assert_cleanup_called
assert_final_cleanup_called
assert_sentinel_live

exercise_leader_crash() {
    case_label=$1
    callback_status=$2
    ignore_term=$3
    prepare_case "$case_label"
    payload_ready="$case_directory/payload-ready"
    if [ "$ignore_term" -eq 1 ]; then
        launch_case "$callback_status" sh -c '
            trap "" TERM
            printf "ready\n" >"$1"
            kill -KILL "$CAST_LATCHED_SUPERVISOR_PID"
            exec sleep 20
        ' leader-crash "$payload_ready"
    else
        launch_case "$callback_status" sh -c '
            printf "ready\n" >"$1"
            kill -KILL "$CAST_LATCHED_SUPERVISOR_PID"
            exec sleep 20
        ' leader-crash "$payload_ready"
    fi
    await_readiness
    acknowledge_and_await_channels
    open_parent_channels
    wait_for_file "$payload_ready" || fail "$case_name payload did not start"
    expect_status_eof_with_bound
    reap_expected_status 137
    group_has_live_member "$leader_pid" || fail "$case_name lost its PGID anchor"
    exec 8>&-
    exec 7<&-
    finish_group
    assert_cleanup_called
    assert_final_cleanup_called
    assert_sentinel_live
}

# The payload starts only after channel readiness, then SIGKILLs the leader.
# Release EOF must remove the payload and monitor before the PGID is forgotten.
exercise_leader_crash leaderkill 0 0

# A failing authenticated cleanup callback is diagnostic, not permission to
# leak containment. The TERM-ignoring payload forces the monitor's KILL path.
exercise_leader_crash cleanupfail 1 1

# A stopped leader cannot run its signal trap. Parent EOF must therefore make
# the independent monitor TERM the group, wait only its configured bound, and
# KILL the frozen leader and TERM-ignoring payload itself.
prepare_case frozen
frozen_ready="$case_directory/frozen-ready"
launch_case 0 sh -c '
    trap "" TERM
    kill -STOP "$CAST_LATCHED_SUPERVISOR_PID"
    printf "ready\n" >"$1"
    exec sleep 20
' frozen-leader "$frozen_ready"
await_readiness
acknowledge_and_await_channels
open_parent_channels
wait_for_file "$frozen_ready" || fail 'frozen payload did not stop its leader'
frozen_state=$(process_state "$leader_pid") || fail 'frozen leader disappeared early'
case "$frozen_state" in
    T|t) ;;
    *) fail "frozen leader entered state $frozen_state instead of T" ;;
esac
exec 8>&-
exec 7<&-
finish_group
reap_expected_status 137
assert_cleanup_called
assert_final_cleanup_called
assert_sentinel_live

printf '%s\n' 'latched command protocol and fault tests passed'
