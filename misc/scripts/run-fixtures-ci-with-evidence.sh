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

remove_flat_run_directory() {
    local directory=$1 expected_identity=${2-} actual_identity nested
    local mount_status
    if [[ -z $expected_identity ]]; then
        printf 'fixture CI run staging has no authenticated identity: %s\n' \
            "$directory" >&2
        return 1
    fi
    if [[ -L $directory || ! -d $directory ]]; then
        printf 'fixture CI run staging is no longer a non-symlink directory: %s\n' \
            "$directory" >&2
        return 1
    fi
    actual_identity=$(stat -c '%u:%a:%d:%i' -- "$directory") || return 1
    if [[ $actual_identity != "$expected_identity" ]]; then
        printf 'fixture CI run staging identity changed before cleanup: %s\n' \
            "$directory" >&2
        return 1
    fi
    if ! nested=$(find "$directory" -mindepth 1 -maxdepth 1 -type d \
        -print -quit); then
        printf 'fixture CI run staging could not be inspected safely: %s\n' \
            "$directory" >&2
        return 1
    fi
    if [[ -n $nested ]]; then
        printf 'fixture CI run staging contains a nested directory: %s\n' \
            "$directory" >&2
        return 1
    fi
    actual_identity=$(stat -c '%u:%a:%d:%i' -- "$directory") || return 1
    if [[ $actual_identity != "$expected_identity" ]]; then
        printf 'fixture CI run staging identity changed during cleanup: %s\n' \
            "$directory" >&2
        return 1
    fi
    if mountpoint -q -- "$directory"; then
        printf 'fixture CI run staging must not be a mount point: %s\n' \
            "$directory" >&2
        return 1
    else
        mount_status=$?
    fi
    if [[ $mount_status -ne 32 ]]; then
        printf 'fixture CI run staging mount status is indeterminate: %s\n' \
            "$directory" >&2
        return 1
    fi
    if ! find "$directory" -mindepth 1 -maxdepth 1 ! -type d \
        -exec rm -f -- {} +; then
        printf 'fixture CI run staging contents could not be removed safely: %s\n' \
            "$directory" >&2
        return 1
    fi
    rmdir -- "$directory"
}

reclaim_stale_run_directories() {
    local evidence_device=$1 caller_uid=$2 path name token metadata
    local owner mode device inode mount_status nested index
    local -a candidates=() identities=()

    shopt -s nullglob
    candidates=("$evidence_dir"/.fixtures-ci-run.*)
    shopt -u nullglob
    for path in "${candidates[@]}"; do
        name=${path##*/}
        token=${name#.fixtures-ci-run.}
        if [[ ${#token} -ne 12 ]]; then
            printf 'fixture CI found a malformed stale run name: %s\n' "$path" >&2
            return 1
        fi
        case "$token" in
            *[!A-Za-z0-9]*)
                printf 'fixture CI found a malformed stale run name: %s\n' \
                    "$path" >&2
                return 1
                ;;
        esac
        if [[ -L $path || ! -d $path ]]; then
            printf 'stale fixture CI run must be a non-symlink directory: %s\n' \
                "$path" >&2
            return 1
        fi
        metadata=$(stat -c '%u:%a:%d:%i' -- "$path") || {
            printf 'stale fixture CI run metadata is unavailable: %s\n' "$path" >&2
            return 1
        }
        IFS=: read -r owner mode device inode <<<"$metadata"
        if [[ $owner != "$caller_uid" || $mode != 700 ]]; then
            printf 'stale fixture CI run must be caller-owned with mode 700: %s\n' \
                "$path" >&2
            return 1
        fi
        if [[ $device != "$evidence_device" ]]; then
            printf 'stale fixture CI run crosses the evidence filesystem: %s\n' \
                "$path" >&2
            return 1
        fi
        if mountpoint -q -- "$path"; then
            printf 'stale fixture CI run must not be a mount point: %s\n' \
                "$path" >&2
            return 1
        else
            mount_status=$?
        fi
        if [[ $mount_status -ne 32 ]]; then
            printf 'stale fixture CI run mount status is indeterminate: %s\n' \
                "$path" >&2
            return 1
        fi
        if ! nested=$(find "$path" -mindepth 1 -maxdepth 1 -type d \
            -print -quit); then
            printf 'stale fixture CI run could not be inspected safely: %s\n' \
                "$path" >&2
            return 1
        fi
        if [[ -n $nested ]]; then
            printf 'stale fixture CI run contains a nested directory: %s\n' \
                "$path" >&2
            return 1
        fi
        identities+=("$metadata")
    done

    # Revalidate the complete set before removing any entry. This prevents a
    # late identity, mount, or nesting failure in one candidate from partially
    # reclaiming otherwise valid siblings.
    for index in "${!candidates[@]}"; do
        path=${candidates[$index]}
        metadata=$(stat -c '%u:%a:%d:%i' -- "$path") || {
            printf 'stale fixture CI run disappeared before reclamation: %s\n' \
                "$path" >&2
            return 1
        }
        if [[ $metadata != "${identities[$index]}" ]]; then
            printf 'stale fixture CI run identity changed before reclamation: %s\n' \
                "$path" >&2
            return 1
        fi
        if mountpoint -q -- "$path"; then
            printf 'stale fixture CI run became a mount point: %s\n' \
                "$path" >&2
            return 1
        else
            mount_status=$?
        fi
        if [[ $mount_status -ne 32 ]]; then
            printf 'stale fixture CI run mount status became indeterminate: %s\n' \
                "$path" >&2
            return 1
        fi
        if ! nested=$(find "$path" -mindepth 1 -maxdepth 1 -type d \
            -print -quit); then
            printf 'stale fixture CI run could not be re-inspected safely: %s\n' \
                "$path" >&2
            return 1
        fi
        if [[ -n $nested ]]; then
            printf 'stale fixture CI run gained a nested directory: %s\n' \
                "$path" >&2
            return 1
        fi
    done

    for index in "${!candidates[@]}"; do
        path=${candidates[$index]}
        metadata=${identities[$index]}
        remove_flat_run_directory "$path" "$metadata" || return 1
    done
}

proof=
bounded_temporary=
run_directory=
run_directory_identity=
launch_signal_status=
setup_cleanup() {
    status=$?
    trap - EXIT
    trap '' HUP INT TERM
    set +e
    if [[ -n $run_directory ]] \
        && ! remove_flat_run_directory "$run_directory" "$run_directory_identity"; then
        status=1
    fi
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
proof_validator="$root/misc/scripts/validate-fixtures-ci-proof.sh"
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
if [[ -L $proof_validator || ! -f $proof_validator || ! -x $proof_validator ]]; then
    printf 'fixture CI proof validator is unavailable or unsafe: %s\n' \
        "$proof_validator" >&2
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
readonly bash_executable=${BASH-}
case "$bash_executable" in
    /*) ;;
    *) printf 'Running Bash must have an absolute executable: %s\n' \
        "$bash_executable" >&2; exit 1 ;;
esac
if [[ ! -f $bash_executable || ! -x $bash_executable ]]; then
    printf 'Bash executable is unavailable or unsafe: %s\n' \
        "$bash_executable" >&2
    exit 1
fi
locale_archive_is_set=0
locale_archive=
if [[ -v LOCALE_ARCHIVE ]]; then
    locale_archive=$LOCALE_ARCHIVE
    case "$locale_archive" in
        /*) ;;
        *) printf 'LOCALE_ARCHIVE must name an absolute path: %s\n' \
            "$locale_archive" >&2; exit 2 ;;
    esac
    if [[ -L $locale_archive || ! -f $locale_archive \
        || ! -r $locale_archive ]]; then
        printf 'LOCALE_ARCHIVE must name a readable regular non-symlink file: %s\n' \
            "$locale_archive" >&2
        exit 1
    fi
    locale_archive_is_set=1
fi
readonly locale_archive locale_archive_is_set
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
command -v flock >/dev/null 2>&1 || {
    printf 'flock is required to serialize fixture CI evidence publication\n' >&2
    exit 1
}
command -v mountpoint >/dev/null 2>&1 || {
    printf 'mountpoint is required to authenticate stale fixture CI runs\n' >&2
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
caller_uid=$(id -u)
evidence_metadata=$(stat -Lc '%u:%a:%d:%i' -- "$evidence_dir") || {
    printf 'fixture evidence directory metadata is unavailable: %s\n' \
        "$evidence_dir" >&2
    exit 1
}
IFS=: read -r evidence_owner evidence_mode evidence_device evidence_inode \
    <<<"$evidence_metadata"
if [[ $evidence_owner != "$caller_uid" || $evidence_mode != 700 ]]; then
    printf 'fixture evidence directory must be caller-owned with mode 700: %s\n' "$evidence_dir" >&2
    exit 1
fi
exec 9<"$evidence_dir"
locked_evidence_metadata=$(stat -Lc '%u:%a:%d:%i' -- "/proc/$$/fd/9") || {
    printf 'fixture evidence directory descriptor cannot be authenticated: %s\n' \
        "$evidence_dir" >&2
    exit 1
}
if [[ $locked_evidence_metadata != "$evidence_metadata" ]]; then
    printf 'fixture evidence directory changed while it was opened: %s\n' \
        "$evidence_dir" >&2
    exit 1
fi
if ! flock --exclusive --nonblock 9; then
    printf 'fixture evidence directory is already owned by another run: %s\n' \
        "$evidence_dir" >&2
    exit 1
fi
if [[ -L $evidence_dir || ! -d $evidence_dir ]]; then
    printf 'fixture evidence path changed after locking: %s\n' \
        "$evidence_dir" >&2
    exit 1
fi
if [[ $(stat -Lc '%u:%a:%d:%i' -- "$evidence_dir") \
    != "$locked_evidence_metadata" ]]; then
    printf 'fixture evidence directory changed after locking: %s\n' \
        "$evidence_dir" >&2
    exit 1
fi

reclaim_stale_run_directories "$evidence_device" "$caller_uid"

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
run_directory_identity=$(stat -c '%u:%a:%d:%i' -- "$run_directory") || {
    printf 'fixture CI run staging metadata is unavailable: %s\n' \
        "$run_directory" >&2
    exit 1
}
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
if [[ ${#invocation_token} -ne 12 ]]; then
    printf 'mktemp returned an unexpected fixture CI invocation token length: %s\n' \
        "$invocation_token" >&2
    exit 1
fi
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
    local proof_commit git_status validator_status
    if [[ ! -f "$candidate_proof" || -L "$candidate_proof" ]]; then
        printf 'successful fixture CI did not publish one regular proof: %s\n' "$candidate_proof" >&2
        return 1
    fi
    command -v git >/dev/null 2>&1 || {
        printf 'git is required to validate fixture CI proof provenance\n' >&2
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
    if "$proof_validator" "$candidate_proof" "$proof_commit"; then
        return 0
    else
        validator_status=$?
    fi
    # Preserve the outer wrapper's signal/cleanup contract even though the
    # shared validator now owns the jq child which observed the interruption.
    case "$validator_status" in
        129) kill -HUP "$$" ;;
        130) kill -INT "$$" ;;
        143) kill -TERM "$$" ;;
    esac
    return "$validator_status"
}

wait_group_exit() {
    local pid=$1
    timeout --kill-after=1s -- "${kill_after_seconds}s" \
        "$bash_executable" --noprofile --norc -p -c '
            target_group=$1
            while :; do
                live_member=0
                for stat_file in /proc/[0-9]*/stat; do
                    IFS= read -r process_stat 2>/dev/null <"$stat_file" || continue
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
    staging_cleanup_failed=0
    remove_flat_run_directory "$run_directory" "$run_directory_identity" \
        || staging_cleanup_failed=1
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
    if [[ $staging_cleanup_failed -eq 1 && $status -eq 0 ]]; then
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
    "--property=UnsetEnvironment=LOCPATH"
    "--property=UnsetEnvironment=LOCALE_ARCHIVE_2_27"
)
if (( locale_archive_is_set )); then
    service_environment+=("--setenv=LOCALE_ARCHIVE=$locale_archive")
else
    service_environment+=("--property=UnsetEnvironment=LOCALE_ARCHIVE")
fi
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
    "$unit" "$unit_marker" "$kill_after_seconds" \
    --close-payload-fd-9 -- \
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
    "$make_executable" --no-print-directory -C "$root" \
    "SHELL=$bash_executable" fixtures-ci \
    >"$output_fifo" 2>&1 5>&- 6>&- &
execution_launch_pid=$!
log_timeout_seconds=$((client_timeout_seconds + kill_after_seconds + 5))
setsid timeout --kill-after="${kill_after_seconds}s" -- "${log_timeout_seconds}s" \
    "$bash_executable" --noprofile --norc -p \
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
# Keep the bounded read in this shell. Bash defers trapped terminal signals
# while waiting for a foreground command-substitution child, which can delay
# EXIT finalization until systemd has already timed out and collected the unit.
# Its timed `read` builtin remains signal-interruptible, so the first terminal
# signal immediately transfers ownership to `finalize` while the unit is live.
if IFS= read -r -t "$status_timeout_seconds" status <&7; then
    status_received=1
    case "$status" in
        ''|*[!0-9]*) status=1 ;;
        *) (( status <= 255 )) || status=1 ;;
    esac
else
    status_read_status=$?
    status=1
    if (( status_read_status > 128 )); then
        status_read_status=124
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
