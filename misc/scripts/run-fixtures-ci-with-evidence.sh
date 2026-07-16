#!/bin/bash

set -euo pipefail

if [[ ${1-} == --capture-log ]]; then
    if [[ $# -ne 4 ]]; then
        printf 'internal log capture requires FIFO, byte bound, and destination\n' >&2
        exit 2
    fi
    set -o pipefail
    tee >(cat >&2) <"$2" | tail -c "$3" >"$4"
    exit $?
fi

proof=
bounded_temporary=
run_directory=
launch_signal_status=
setup_cleanup() {
    status=$?
    trap - EXIT
    trap '' HUP INT TERM
    set +e
    [[ -z "$run_directory" ]] || rm -rf -- "$run_directory"
    [[ -z "$bounded_temporary" ]] || rm -f -- "$bounded_temporary"
    if [[ $status -ne 0 && -n "$proof" ]]; then
        rm -f -- "$proof"
    fi
    exit "$status"
}
trap setup_cleanup EXIT
trap '[[ -n "$launch_signal_status" ]] || launch_signal_status=129' HUP
trap '[[ -n "$launch_signal_status" ]] || launch_signal_status=130' INT
trap '[[ -n "$launch_signal_status" ]] || launch_signal_status=143' TERM

root=$(CDPATH= cd -- "$(dirname -- "$0")/../.." && pwd -P)
latched_runner="$root/misc/scripts/run-latched-command.sh"
owned_unit_stopper="$root/misc/scripts/stop-owned-fixture-unit.sh"
evidence_dir=${FIXTURE_EVIDENCE_DIR:-$root/target/fixture-evidence}
maximum_log_bytes=${CAST_FIXTURE_LOG_MAX_BYTES:-1048576}
timeout_seconds=${CAST_FIXTURE_CI_TIMEOUT_SECONDS:-10800}
kill_after_seconds=${CAST_FIXTURE_CI_KILL_AFTER_SECONDS:-30}
make_command=${MAKE:-make}
test_signal_after_reap=${CAST_FIXTURE_TEST_SIGNAL_AFTER_LATCHED_REAP-}
test_release_gate=${CAST_FIXTURE_TEST_LATCHED_RELEASE_GATE-}

if [[ -L $latched_runner || ! -f $latched_runner || ! -x $latched_runner ]]; then
    printf 'latched command runner is unavailable or unsafe: %s\n' "$latched_runner" >&2
    exit 1
fi
if [[ -L $owned_unit_stopper || ! -f $owned_unit_stopper \
    || ! -x $owned_unit_stopper ]]; then
    printf 'owned fixture unit stopper is unavailable or unsafe: %s\n' \
        "$owned_unit_stopper" >&2
    exit 1
fi
case "$test_signal_after_reap" in
    ''|1) ;;
    *) printf 'CAST_FIXTURE_TEST_SIGNAL_AFTER_LATCHED_REAP must be empty or 1\n' >&2; exit 2 ;;
esac
case "$test_release_gate" in
    ''|/*) ;;
    *) printf 'CAST_FIXTURE_TEST_LATCHED_RELEASE_GATE must be empty or absolute\n' >&2; exit 2 ;;
esac

case "$evidence_dir" in
    /*) ;;
    *) printf 'FIXTURE_EVIDENCE_DIR must be absolute: %s\n' "$evidence_dir" >&2; exit 2 ;;
esac
case "$maximum_log_bytes" in
    ''|*[!0-9]*) printf 'CAST_FIXTURE_LOG_MAX_BYTES must be decimal\n' >&2; exit 2 ;;
esac
case "$maximum_log_bytes" in
    0|0*|????????*)
        printf 'CAST_FIXTURE_LOG_MAX_BYTES must be between 1 and 1048576\n' >&2
        exit 2
        ;;
esac
if (( maximum_log_bytes < 1 || maximum_log_bytes > 1048576 )); then
    printf 'CAST_FIXTURE_LOG_MAX_BYTES must be between 1 and 1048576\n' >&2
    exit 2
fi
case "$timeout_seconds" in
    ''|*[!0-9]*) printf 'CAST_FIXTURE_CI_TIMEOUT_SECONDS must be decimal\n' >&2; exit 2 ;;
esac
case "$timeout_seconds" in
    0|0*|??????*)
        printf 'CAST_FIXTURE_CI_TIMEOUT_SECONDS must be between 1 and 21600\n' >&2
        exit 2
        ;;
esac
if (( timeout_seconds < 1 || timeout_seconds > 21600 )); then
    printf 'CAST_FIXTURE_CI_TIMEOUT_SECONDS must be between 1 and 21600\n' >&2
    exit 2
fi
case "$kill_after_seconds" in
    ''|*[!0-9]*) printf 'CAST_FIXTURE_CI_KILL_AFTER_SECONDS must be decimal\n' >&2; exit 2 ;;
esac
case "$kill_after_seconds" in
    0|0*|????*)
        printf 'CAST_FIXTURE_CI_KILL_AFTER_SECONDS must be between 1 and 300\n' >&2
        exit 2
        ;;
esac
if (( kill_after_seconds < 1 || kill_after_seconds > 300 )); then
    printf 'CAST_FIXTURE_CI_KILL_AFTER_SECONDS must be between 1 and 300\n' >&2
    exit 2
fi
case "$make_command" in
    *[!A-Za-z0-9_./+-]*) printf 'MAKE must name exactly one executable: %s\n' "$make_command" >&2; exit 2 ;;
esac
command -v "$make_command" >/dev/null 2>&1 || {
    printf 'Make executable is unavailable: %s\n' "$make_command" >&2
    exit 1
}
command -v setsid >/dev/null 2>&1 || {
    printf 'setsid is required to own fixture CI process groups\n' >&2
    exit 1
}
command -v systemd-run >/dev/null 2>&1 || {
    printf 'systemd-run is required to contain the fixture CI process tree\n' >&2
    exit 1
}
command -v systemctl >/dev/null 2>&1 || {
    printf 'systemctl is required to own the fixture CI service\n' >&2
    exit 1
}
if ! timeout --kill-after=1s 10s systemctl --user show-environment >/dev/null 2>&1; then
    printf 'fixture CI requires a reachable systemd user manager\n' >&2
    exit 1
fi

if [[ -L "$evidence_dir" || -e "$evidence_dir" && ! -d "$evidence_dir" ]]; then
    printf 'fixture evidence path must be a non-symlink directory: %s\n' "$evidence_dir" >&2
    exit 1
fi
mkdir -p "$evidence_dir"
chmod 700 "$evidence_dir"
if [[ $(stat -c '%u' "$evidence_dir") -ne $(id -u) || $(stat -c '%a' "$evidence_dir") != 700 ]]; then
    printf 'fixture evidence directory must be caller-owned with mode 700: %s\n' "$evidence_dir" >&2
    exit 1
fi

proof="$evidence_dir/fixtures-ci-proof.json"
bounded_log="$evidence_dir/fixtures-ci.log"
bounded_temporary="$evidence_dir/.fixtures-ci.log.tmp"
rm -f "$proof" "$bounded_log" "$bounded_temporary"

umask 077
: >"$bounded_temporary"
chmod 600 "$bounded_temporary"
run_directory=$(mktemp -d "$evidence_dir/.fixtures-ci-run.XXXXXXXXXXXX")
chmod 700 "$run_directory"
if [[ -L "$run_directory" || ! -d "$run_directory" \
    || $(stat -c '%u' "$run_directory") -ne $(id -u) \
    || $(stat -c '%a' "$run_directory") != 700 ]]; then
    printf 'fixture CI run staging must be a caller-owned mode-700 directory: %s\n' \
        "$run_directory" >&2
    exit 1
fi
staged_proof="$run_directory/fixtures-ci-proof.json"
output_fifo="$run_directory/fixture-output.fifo"
execution_ready="$run_directory/execution-ready"
execution_acknowledgement="$run_directory/execution-acknowledged"
execution_channels_ready="$run_directory/execution-channels-ready"
execution_status_fifo="$run_directory/execution-status.fifo"
execution_release_fifo="$run_directory/execution-release.fifo"
invocation_token=${run_directory##*.}
case "$invocation_token" in
    ''|*[!A-Za-z0-9]*)
        printf 'mktemp returned an unsafe fixture CI invocation token: %s\n' \
            "$invocation_token" >&2
        exit 1
        ;;
esac
unit="cast-fixtures-ci-$(id -u)-$$-$invocation_token.service"
unit_marker="CAST_FIXTURE_CI_UNIT_TOKEN=$invocation_token"
mkfifo -m 600 "$output_fifo"
mkfifo -m 600 "$execution_status_fifo"
mkfifo -m 600 "$execution_release_fifo"
# Linux permits an owner to hold a FIFO read/write. These temporary anchors
# make channel establishment nonblocking; they are closed as soon as the
# acknowledged supervisor proves that its one-way endpoints are open.
exec 5<>"$execution_status_fifo"
exec 6<>"$execution_release_fifo"
execution_pid=
execution_launch_pid=
execution_release_open=0
log_pipeline_pid=
unit_cleanup_failed=0

if load_state=$(timeout --kill-after=1s 10s systemctl --user show "$unit" \
    --property=LoadState --value 2>/dev/null); then
    if [[ $load_state != not-found ]]; then
        printf 'refusing pre-existing fixture CI unit %s with load state %s\n' \
            "$unit" "$load_state" >&2
        exit 1
    fi
else
    printf 'could not authenticate fixture CI unit-name availability: %s\n' "$unit" >&2
    exit 1
fi

validate_proof() {
    local candidate_proof=$1
    local proof_owner proof_mode proof_links proof_size proof_commit git_status
    if [[ ! -f "$candidate_proof" || -L "$candidate_proof" ]]; then
        printf 'successful fixture CI did not publish one regular proof: %s\n' "$candidate_proof" >&2
        return 1
    fi
    proof_owner=$(stat -c '%u' "$candidate_proof") || return 1
    proof_mode=$(stat -c '%a' "$candidate_proof") || return 1
    proof_links=$(stat -c '%h' "$candidate_proof") || return 1
    proof_size=$(stat -c '%s' "$candidate_proof") || return 1
    if [[ $proof_owner -ne $(id -u) || $proof_mode != 644 || $proof_links -ne 1 ]]; then
        printf 'fixture CI proof must be caller-owned, mode 644, and singly linked: %s\n' \
            "$candidate_proof" >&2
        return 1
    fi
    if [[ $proof_size -le 0 || $proof_size -gt 4096 ]]; then
        printf 'fixture CI proof must contain between 1 and 4096 bytes: %s\n' "$candidate_proof" >&2
        return 1
    fi
    command -v git >/dev/null 2>&1 || {
        printf 'git is required to validate fixture CI proof provenance\n' >&2
        return 1
    }
    command -v jq >/dev/null 2>&1 || {
        printf 'jq is required to validate fixture CI proof content\n' >&2
        return 1
    }
    proof_commit=$(git -C "$root" rev-parse --verify HEAD) || {
        printf 'cannot resolve the fixture CI proof commit\n' >&2
        return 1
    }
    case "$proof_commit" in
        ''|*[!0-9a-f]*)
            printf 'Git returned a noncanonical fixture CI proof commit: %s\n' "$proof_commit" >&2
            return 1
            ;;
    esac
    if [[ ${#proof_commit} -ne 40 && ${#proof_commit} -ne 64 ]]; then
        printf 'Git returned an unexpected fixture CI proof commit length: %s\n' \
            "${#proof_commit}" >&2
        return 1
    fi
    if ! git_status=$(git -C "$root" status --porcelain --untracked-files=normal); then
        printf 'cannot inspect checkout cleanliness while validating fixture CI proof\n' >&2
        return 1
    fi
    if [[ -n $git_status ]]; then
        printf 'fixture CI proof refuses a checkout which differs from commit %s\n' \
            "$proof_commit" >&2
        return 1
    fi
    if ! jq -s -e --arg commit "$proof_commit" '
        length == 1 and .[0] == {
          schema: "cast.fixtures-ci-proof.v1",
          git_commit: $commit,
          git_tree: "clean",
          selection: "all",
          required_execution: true,
          fixture_count: 13,
          fixtures: [
            "autotools",
            "autotools-options",
            "cargo",
            "cargo-features",
            "cargo-vendored",
            "cmake",
            "custom",
            "daemon-generated",
            "factory-override",
            "generated-config",
            "hooks-patch",
            "meson",
            "split"
          ],
          assertions: [
            "contentful-build-and-publish",
            "decoded-bundle-contract",
            "locked-plan-and-derivation-reuse",
            "second-contentful-build-reused",
            "stone-and-manifest-bytes-identical"
          ],
          result: "passed"
        }
    ' "$candidate_proof" >/dev/null; then
        printf 'fixture CI proof does not exactly match the required commit and fixture matrix\n' >&2
        return 1
    fi
}

wait_group_exit() {
    local pid=$1
    timeout --kill-after=1s -- "${kill_after_seconds}s" \
        bash -c '
            target_group=$1
            while :; do
                live_member=0
                for stat_file in /proc/[0-9]*/stat; do
                    IFS= read -r process_stat <"$stat_file" 2>/dev/null || continue
                    process_tail=${process_stat##*) }
                    set -- $process_tail
                    process_state=${1-}
                    process_group=${3-}
                    case "$process_state" in
                        Z|X|"") ;;
                        *)
                            if [[ $process_group == "$target_group" ]]; then
                                live_member=1
                                break
                            fi
                            ;;
                    esac
                done
                [[ $live_member -eq 1 ]] || exit 0
                sleep 0.05
            done
        ' fixture-ci-group-wait "$pid"
}

stop_group() {
    local pid=$1 group_status
    if kill -0 "$pid" >/dev/null 2>&1 || kill -0 -- "-$pid" >/dev/null 2>&1; then
        # Give the inner delegated runner time to authenticate and stop its
        # separate systemd service before forcing the enclosing Make group down.
        kill -TERM -- "-$pid" >/dev/null 2>&1 \
            || kill -TERM "$pid" >/dev/null 2>&1 \
            || :
        if ! wait_group_exit "$pid"; then
            kill -KILL -- "-$pid" >/dev/null 2>&1 \
                || kill -KILL "$pid" >/dev/null 2>&1 \
                || :
        fi
    fi
    if wait "$pid" 2>/dev/null; then
        return 0
    else
        group_status=$?
        return "$group_status"
    fi
}

stop_owned_unit() {
    "$owned_unit_stopper" "$unit" "$unit_marker" "$kill_after_seconds"
}

finalize() {
    status=$?
    # Finalization owns the first interruption. Later terminal signals must not
    # interrupt proof invalidation or bounded-log publication.
    trap '' HUP INT TERM
    trap - EXIT
    set +e
    stop_owned_unit || unit_cleanup_failed=1
    if [[ -n "$execution_pid" ]]; then
        stop_group "$execution_pid"
        execution_pid=
        if [[ $execution_release_open -eq 1 ]]; then
            exec 8>&-
            execution_release_open=0
        fi
        # Close the narrow race where systemd accepted the service after the
        # first ownership query but before the client group was stopped.
        stop_owned_unit || unit_cleanup_failed=1
    elif [[ $execution_release_open -eq 1 ]]; then
        exec 8>&-
        execution_release_open=0
    fi
    if [[ -n "$log_pipeline_pid" ]]; then
        # Once execution is gone the FIFO has no writer. Let the bounded logger
        # drain those final bytes before forcing its process group down.
        if wait_group_exit "$log_pipeline_pid"; then
            wait "$log_pipeline_pid" 2>/dev/null || :
        else
            stop_group "$log_pipeline_pid"
        fi
        log_pipeline_pid=
    fi
    rm -rf -- "$run_directory"
    if [[ -f "$bounded_temporary" && ! -L "$bounded_temporary" ]] \
        && [[ $(stat -c '%s' "$bounded_temporary") -le $maximum_log_bytes ]]; then
        chmod 600 "$bounded_temporary"
        mv -f "$bounded_temporary" "$bounded_log"
        finalize_status=$?
    else
        finalize_status=1
    fi
    rm -f "$bounded_temporary"
    if [[ $unit_cleanup_failed -eq 1 && $status -eq 0 ]]; then
        status=1
    fi
    if [[ $status -ne 0 || $finalize_status -ne 0 ]]; then
        rm -f "$proof"
    fi
    if [[ $status -eq 0 && $finalize_status -ne 0 ]]; then
        status=$finalize_status
    fi
    exit "$status"
}
trap finalize EXIT
terminate() {
    signal_status=$1
    trap '' HUP INT TERM
    exit "$signal_status"
}

set +e
if [[ -n "$launch_signal_status" ]]; then
    exit "$launch_signal_status"
fi
make_executable=$(command -v "$make_command")
service_environment=(
    "--setenv=PATH=$PATH"
    "--setenv=CAST_FIXTURE_EVIDENCE_DIR=$run_directory"
    "--setenv=CAST_FIXTURE_WRAPPER_PID=$$"
    "--setenv=FIXTURE_EVIDENCE_DIR="
    "--setenv=$unit_marker"
)
for environment_name in HOME CARGO_HOME RUSTUP_HOME XDG_RUNTIME_DIR DBUS_SESSION_BUS_ADDRESS; do
    if [[ -v $environment_name ]]; then
        service_environment+=("--setenv=$environment_name=${!environment_name}")
    fi
done
client_timeout_seconds=$((timeout_seconds + kill_after_seconds + 10))
status_timeout_seconds=${CAST_FIXTURE_CI_STATUS_TIMEOUT_SECONDS:-$((
    client_timeout_seconds + kill_after_seconds + 5
))}
case "$status_timeout_seconds" in
    ''|*[!0-9]*)
        printf 'CAST_FIXTURE_CI_STATUS_TIMEOUT_SECONDS must be decimal\n' >&2
        exit 2
        ;;
    0|0*|??????*)
        printf 'CAST_FIXTURE_CI_STATUS_TIMEOUT_SECONDS must be between 1 and 22215\n' >&2
        exit 2
        ;;
esac
if (( status_timeout_seconds < 1 || status_timeout_seconds > 22215 )); then
    printf 'CAST_FIXTURE_CI_STATUS_TIMEOUT_SECONDS must be between 1 and 22215\n' >&2
    exit 2
fi
# No asynchronous launch child may inherit the destructive EXIT finalizer.
# Terminal signals remain deferred while the parent temporarily clears it.
trap - EXIT
CAST_LATCHED_KILL_AFTER_SECONDS="$kill_after_seconds" setsid "$latched_runner" \
    "$execution_ready" "$execution_acknowledgement" \
    "$execution_channels_ready" "$execution_status_fifo" \
    "$execution_release_fifo" \
    --parent-loss-cleanup "$owned_unit_stopper" \
    "$unit" "$unit_marker" "$kill_after_seconds" -- \
    timeout --foreground --kill-after="${kill_after_seconds}s" -- "${client_timeout_seconds}s" \
    systemd-run \
    --user \
    --unit="$unit" \
    --wait \
    --pipe \
    --collect \
    --no-ask-password \
    --expand-environment=no \
    --service-type=exec \
    --working-directory="$root" \
    "${service_environment[@]}" \
    --property=ExitType=main \
    --property=KillMode=control-group \
    --property="RuntimeMaxSec=${timeout_seconds}s" \
    --property="TimeoutStopSec=${kill_after_seconds}s" \
    --property=SendSIGKILL=yes \
    -- \
    "$make_executable" --no-print-directory -C "$root" fixtures-ci \
    >"$output_fifo" 2>&1 5>&- 6>&- &
execution_launch_pid=$!
log_timeout_seconds=$((client_timeout_seconds + kill_after_seconds + 5))
setsid timeout --kill-after="${kill_after_seconds}s" -- "${log_timeout_seconds}s" \
    "$root/misc/scripts/run-fixtures-ci-with-evidence.sh" \
    --capture-log "$output_fifo" "$maximum_log_bytes" \
    "$bounded_temporary" 5>&- 6>&- &
log_pipeline_pid=$!
trap finalize EXIT
trap '[[ -n "$launch_signal_status" ]] || launch_signal_status=129' HUP
trap '[[ -n "$launch_signal_status" ]] || launch_signal_status=130' INT
trap '[[ -n "$launch_signal_status" ]] || launch_signal_status=143' TERM

# Do not enable numeric cleanup until the helper has proved that `$!` names
# its still-live session/process-group leader. Before acknowledgement it cannot
# launch the payload and exits by itself after a bounded wait.
ready_pid=
ready_attempt=0
while [[ ! -f $execution_ready ]]; do
    ready_attempt=$((ready_attempt + 1))
    if (( ready_attempt > 220 )); then
        break
    fi
    kill -0 "$execution_launch_pid" >/dev/null 2>&1 || break
    sleep 0.05
done
if [[ -f $execution_ready && ! -L $execution_ready \
    && $(stat -c '%u:%a:%h' "$execution_ready") == "$(id -u):600:1" ]]; then
    IFS= read -r ready_pid <"$execution_ready" || ready_pid=
fi
if [[ $ready_pid != "$execution_launch_pid" ]]; then
    trap '' HUP INT TERM
    wait "$execution_launch_pid" 2>/dev/null || :
    execution_launch_pid=
    trap 'terminate 129' HUP
    trap 'terminate 130' INT
    trap 'terminate 143' TERM
    if [[ -n $launch_signal_status ]]; then
        exit "$launch_signal_status"
    fi
    printf 'latched fixture supervisor did not prove launch PID ownership\n' >&2
    exit 1
fi
execution_pid=$execution_launch_pid
execution_launch_pid=
trap 'terminate 129' HUP
trap 'terminate 130' INT
trap 'terminate 143' TERM
if [[ -n $launch_signal_status ]]; then
    exit "$launch_signal_status"
fi
acknowledgement_temporary="${execution_acknowledgement}.tmp.$$"
( umask 077; set -o noclobber; printf 'ready\n' >"$acknowledgement_temporary" ) || exit 1
mv "$acknowledgement_temporary" "$execution_acknowledgement" || exit 1
channels_pid=
channels_attempt=0
while [[ ! -f $execution_channels_ready ]]; do
    channels_attempt=$((channels_attempt + 1))
    if (( channels_attempt > 220 )); then
        break
    fi
    kill -0 "$execution_pid" >/dev/null 2>&1 || break
    sleep 0.05
done
if [[ -f $execution_channels_ready && ! -L $execution_channels_ready \
    && $(stat -c '%u:%a:%h' "$execution_channels_ready") == "$(id -u):600:1" ]]; then
    IFS= read -r channels_pid <"$execution_channels_ready" || channels_pid=
fi
if [[ $channels_pid != "$execution_pid" ]]; then
    trap '' HUP INT TERM
    stop_group "$execution_pid"
    execution_pid=
    exec 5>&-
    exec 6>&-
    trap 'terminate 129' HUP
    trap 'terminate 130' INT
    trap 'terminate 143' TERM
    if [[ -n $launch_signal_status ]]; then
        exit "$launch_signal_status"
    fi
    printf 'latched fixture supervisor did not establish private channels\n' >&2
    exit 1
fi
exec 7<"$execution_status_fifo"
exec 8>"$execution_release_fifo"
execution_release_open=1
exec 5>&-
exec 6>&-
trap 'terminate 129' HUP
trap 'terminate 130' INT
trap 'terminate 143' TERM
if [[ -n $launch_signal_status ]]; then
    exit "$launch_signal_status"
fi

# Kill-capable traps remain active for the whole payload wait because the
# acknowledged supervisor pins the process-group identity until release.
status_received=0
status_read_status=0
if status=$(
    # Bash forks the command-substitution shell before it applies the command's
    # redirections. Close release in that shell as well, otherwise a SIGKILLed
    # parent can leave an orphan which falsely keeps the monitor alive.
    exec 8>&-
    timeout --foreground \
        --kill-after="${kill_after_seconds}s" -- "${status_timeout_seconds}s" \
        bash -c '
            exec 8>&-
            IFS= read -r status <&7 || exit 1
            printf "%s\n" "$status"
        ' fixture-ci-status-reader 8>&-
); then
    status_received=1
    case "$status" in
        ''|*[!0-9]*) status=1 ;;
        *) (( status <= 255 )) || status=1 ;;
    esac
else
    status_read_status=$?
    status=1
    if [[ $status_read_status -eq 124 || $status_read_status -eq 137 ]]; then
        printf 'latched fixture status channel exceeded its %s-second bound\n' \
            "$status_timeout_seconds" >&2
    fi
fi
exec 7<&-
if [[ -n $test_release_gate ]]; then
    gate_ready="${test_release_gate}.ready"
    gate_continue="${test_release_gate}.continue"
    if [[ -e $gate_ready || -L $gate_ready || -e $gate_continue || -L $gate_continue ]]; then
        printf 'latched release test gate paths must be absent\n' >&2
        status=1
    else
        ( umask 077; set -o noclobber; printf 'ready\n' >"$gate_ready" ) || status=1
        gate_attempt=0
        while [[ ! -e $gate_continue && $gate_attempt -lt 1000 ]]; do
            gate_attempt=$((gate_attempt + 1))
            sleep 0.01
        done
        if [[ ! -e $gate_continue ]]; then
            printf 'latched release test gate timed out\n' >&2
            status=1
        fi
    fi
fi
launch_signal_status=
trap '[[ -n "$launch_signal_status" ]] || launch_signal_status=129' HUP
trap '[[ -n "$launch_signal_status" ]] || launch_signal_status=130' INT
trap '[[ -n "$launch_signal_status" ]] || launch_signal_status=143' TERM

# The supervisor still pins its PID and process group after reporting status.
# From release through reap and ownership clearing, terminal traps only record;
# therefore no cleanup path can signal a numeric identity after it was reaped.
if [[ $status_received -eq 1 ]]; then
    trap '' PIPE
    if ! printf 'release\n' >&8; then
        status=1
    fi
    trap - PIPE
    exec 8>&-
    execution_release_open=0
    if wait "$execution_pid"; then
        latched_status=0
    else
        latched_status=$?
    fi
else
    # Status-channel EOF means the supervisor leader died without completing
    # the protocol. Keep release open so its monitor remains an in-group PGID
    # anchor while the authenticated unit and every remaining group member are
    # stopped, then reap the cached child status and release the monitor.
    stop_owned_unit || unit_cleanup_failed=1
    if stop_group "$execution_pid"; then
        latched_status=0
    else
        latched_status=$?
    fi
    execution_pid=
    exec 8>&-
    execution_release_open=0
fi
if [[ -n "$launch_signal_status" && -n "$execution_pid" ]]; then
    trap '' HUP INT TERM
    if wait "$execution_pid"; then
        latched_status=0
    else
        latched_status=$?
    fi
    trap '[[ -n "$launch_signal_status" ]] || launch_signal_status=129' HUP
    trap '[[ -n "$launch_signal_status" ]] || launch_signal_status=130' INT
    trap '[[ -n "$launch_signal_status" ]] || launch_signal_status=143' TERM
fi
if [[ $test_signal_after_reap == 1 ]]; then
    kill -TERM "$$"
fi
execution_pid=
if [[ $status_received -eq 0 ]]; then
    if [[ $status_read_status -eq 124 || $status_read_status -eq 137 ]]; then
        status=124
    else
        status=$latched_status
        [[ $status -ne 0 ]] || status=1
    fi
elif [[ $latched_status -ne $status ]]; then
    printf 'latched fixture execution status changed from %s to %s while reaping\n' \
        "$status" "$latched_status" >&2
    status=1
fi
trap 'terminate 129' HUP
trap 'terminate 130' INT
trap 'terminate 143' TERM
if [[ -n "$launch_signal_status" ]]; then
    exit "$launch_signal_status"
fi

launch_signal_status=
trap '[[ -n "$launch_signal_status" ]] || launch_signal_status=129' HUP
trap '[[ -n "$launch_signal_status" ]] || launch_signal_status=130' INT
trap '[[ -n "$launch_signal_status" ]] || launch_signal_status=143' TERM
# A terminated client can leave the independently owned service cgroup alive
# with a writer on the output FIFO. Authenticate and stop it before waiting for
# logger EOF. On a collected success this is a bounded no-op.
stop_owned_unit || unit_cleanup_failed=1
if [[ $unit_cleanup_failed -eq 1 && $status -eq 0 ]]; then
    status=1
fi
if wait_group_exit "$log_pipeline_pid"; then
    wait "$log_pipeline_pid"
    log_status=$?
    log_pipeline_pid=
else
    printf 'bounded fixture log did not drain within %s seconds\n' \
        "$kill_after_seconds" >&2
    stop_group "$log_pipeline_pid"
    log_pipeline_pid=
    log_status=1
fi
trap 'terminate 129' HUP
trap 'terminate 130' INT
trap 'terminate 143' TERM
if [[ -n "$launch_signal_status" ]]; then
    exit "$launch_signal_status"
fi
set -e

if [[ $status -eq 0 && $log_status -ne 0 ]]; then
    printf 'bounded fixture log capture failed with status %s\n' "$log_status" >&2
    status=$log_status
fi
if [[ $status -eq 0 ]]; then
    if validate_proof "$staged_proof" \
        && mv -T -- "$staged_proof" "$proof" \
        && validate_proof "$proof"; then
        :
    else
        status=1
    fi
fi
exit "$status"
