#!/bin/sh

set -eu

if [ "$#" -ne 1 ]; then
    printf 'usage: %s <--preflight-only|all|fixture-name>\n' "$0" >&2
    exit 2
fi

mode=fixture
fixture=
case "$1" in
    --preflight-only)
        mode=preflight
        ;;
    all|autotools|autotools-options|cargo|cargo-features|cargo-vendored|cmake|custom|daemon-generated|factory-override|generated-config|generated-shell|hooks-patch|meson|plugin-output|split|userspace-profile)
        fixture=$1
        ;;
    *)
        printf '%s\n' \
            'argument must be exactly `--preflight-only`, `all`, or one of: autotools autotools-options cargo cargo-features cargo-vendored cmake custom daemon-generated factory-override generated-config generated-shell hooks-patch meson plugin-output split userspace-profile' >&2
        exit 2
        ;;
esac

root=$(CDPATH= cd -- "$(dirname -- "$0")/../.." && pwd -P)
latched_runner="$root/misc/scripts/run-latched-command.sh"
owned_unit_stopper="$root/misc/scripts/stop-owned-fixture-unit.sh"
tmpdir=${TMPDIR-}
package_store=${CAST_BOOTSTRAP_PACKAGE_STORE:-$root/target/bootstrap-fixtures/packages}
require_execution=${CAST_REQUIRE_EXECUTION-}
cargo_command=${CARGO:-cargo}
evidence_dir=${CAST_FIXTURE_EVIDENCE_DIR:-$root/target/fixture-evidence}
test_signal_after_reap=${CAST_FIXTURE_TEST_SIGNAL_AFTER_LATCHED_REAP-}
if [ "$mode" = preflight ]; then
    default_client_kill_after_seconds=5
    service_runtime_max=30s
    systemd_client_timeout_seconds=40
else
    default_client_kill_after_seconds=30
    service_runtime_max=2h
    systemd_client_timeout_seconds=7290
fi
client_kill_after_seconds=${CAST_DELEGATED_KILL_AFTER_SECONDS:-$default_client_kill_after_seconds}
client_status_timeout_seconds=${CAST_DELEGATED_STATUS_TIMEOUT_SECONDS:-}
proof_path=
proof_temporary=
git_commit=

if [ -L "$latched_runner" ] || [ ! -f "$latched_runner" ] || [ ! -x "$latched_runner" ]; then
    printf 'latched command runner is unavailable or unsafe: %s\n' "$latched_runner" >&2
    exit 1
fi
if [ -L "$owned_unit_stopper" ] || [ ! -f "$owned_unit_stopper" ] \
    || [ ! -x "$owned_unit_stopper" ]; then
    printf 'owned fixture unit stopper is unavailable or unsafe: %s\n' \
        "$owned_unit_stopper" >&2
    exit 1
fi
case "$test_signal_after_reap" in
    ''|1) ;;
    *) printf 'CAST_FIXTURE_TEST_SIGNAL_AFTER_LATCHED_REAP must be empty or 1\n' >&2; exit 2 ;;
esac
case "$client_kill_after_seconds" in
    ''|*[!0-9]*) printf 'CAST_DELEGATED_KILL_AFTER_SECONDS must be decimal\n' >&2; exit 2 ;;
esac
case "$client_kill_after_seconds" in
    0|0*|????*)
        printf 'CAST_DELEGATED_KILL_AFTER_SECONDS must be between 1 and 300\n' >&2
        exit 2
        ;;
esac
if [ "$client_kill_after_seconds" -lt 1 ] || [ "$client_kill_after_seconds" -gt 300 ]; then
    printf 'CAST_DELEGATED_KILL_AFTER_SECONDS must be between 1 and 300\n' >&2
    exit 2
fi
if [ -z "$client_status_timeout_seconds" ]; then
    client_status_timeout_seconds=$((systemd_client_timeout_seconds + client_kill_after_seconds + 5))
fi
case "$client_status_timeout_seconds" in
    ''|*[!0-9]*) printf 'CAST_DELEGATED_STATUS_TIMEOUT_SECONDS must be decimal\n' >&2; exit 2 ;;
    0|0*|?????*)
        printf 'CAST_DELEGATED_STATUS_TIMEOUT_SECONDS must be between 1 and 8000\n' >&2
        exit 2
        ;;
esac
if [ "$client_status_timeout_seconds" -lt 1 ] \
    || [ "$client_status_timeout_seconds" -gt 8000 ]; then
    printf 'CAST_DELEGATED_STATUS_TIMEOUT_SECONDS must be between 1 and 8000\n' >&2
    exit 2
fi

case "$require_execution" in
    0|1) ;;
    *)
        printf 'CAST_REQUIRE_EXECUTION must be set to exactly 0 or 1; got %s\n' \
            "${require_execution:-<unset>}" >&2
        exit 2
        ;;
esac
if [ "$mode" = preflight ] && [ "$require_execution" != 1 ]; then
    printf 'delegated execution preflight requires CAST_REQUIRE_EXECUTION=1\n' >&2
    exit 2
fi

if [ "$mode" = fixture ] && [ "$fixture" = all ] && [ "$require_execution" = 1 ]; then
    case "$evidence_dir" in
        /*) ;;
        *) printf 'CAST_FIXTURE_EVIDENCE_DIR must be absolute: %s\n' "$evidence_dir" >&2; exit 2 ;;
    esac
    if [ -L "$evidence_dir" ] || { [ -e "$evidence_dir" ] && [ ! -d "$evidence_dir" ]; }; then
        printf 'fixture evidence path must be a non-symlink directory: %s\n' "$evidence_dir" >&2
        exit 1
    fi
    mkdir -p "$evidence_dir"
    chmod 700 "$evidence_dir"
    if [ "$(stat -c '%u' "$evidence_dir")" -ne "$(id -u)" ] || [ "$(stat -c '%a' "$evidence_dir")" != 700 ]; then
        printf 'fixture evidence directory must be caller-owned with mode 700: %s\n' "$evidence_dir" >&2
        exit 1
    fi
    proof_path="$evidence_dir/fixtures-ci-proof.json"
    proof_temporary="$evidence_dir/.fixtures-ci-proof.json.tmp"
    rm -f "$proof_path" "$proof_temporary"

    command -v git >/dev/null 2>&1 || {
        printf 'git is required to bind fixture CI evidence to the exact checkout\n' >&2
        exit 1
    }
    git_commit=$(git -C "$root" rev-parse --verify HEAD) || {
        printf 'cannot resolve the exact fixture CI Git commit\n' >&2
        exit 1
    }
    case "$git_commit" in
        ''|*[!0-9a-f]*) printf 'Git returned a noncanonical fixture CI commit: %s\n' "$git_commit" >&2; exit 1 ;;
    esac
    if [ "${#git_commit}" -ne 40 ] && [ "${#git_commit}" -ne 64 ]; then
        printf 'Git returned an unexpected fixture CI commit length: %s\n' "${#git_commit}" >&2
        exit 1
    fi
    if ! git_status=$(git -C "$root" status --porcelain --untracked-files=normal); then
        printf 'cannot inspect fixture CI checkout cleanliness before execution\n' >&2
        exit 1
    fi
    if [ -n "$git_status" ]; then
        printf 'required fixture CI proof refuses a checkout which differs from commit %s\n' "$git_commit" >&2
        exit 1
    fi
fi

case "$tmpdir" in
    /*) ;;
    *) printf 'TMPDIR must name an absolute private directory; got %s\n' "${tmpdir:-<unset>}" >&2; exit 2 ;;
esac
if [ ! -d "$tmpdir" ] || [ -L "$tmpdir" ]; then
    printf 'TMPDIR must be an existing non-symlink directory: %s\n' "$tmpdir" >&2
    exit 2
fi
if [ "$(stat -c '%u' "$tmpdir")" -ne "$(id -u)" ] || [ "$(stat -c '%a' "$tmpdir")" != 700 ]; then
    printf 'TMPDIR must be owned by the caller with mode 700: %s\n' "$tmpdir" >&2
    exit 2
fi

if [ "$mode" = fixture ]; then
    case "$package_store" in
        /*) ;;
        *) printf 'CAST_BOOTSTRAP_PACKAGE_STORE must be absolute: %s\n' "$package_store" >&2; exit 2 ;;
    esac
    if [ ! -d "$package_store" ] || [ -L "$package_store" ]; then
        printf 'verified bootstrap package store is unavailable at %s; run `make bootstrap-fixtures-prepare` first\n' \
            "$package_store" >&2
        exit 1
    fi
fi

case "$cargo_command" in
    *[!A-Za-z0-9_./+-]*)
        printf 'CARGO must name exactly one executable, found: %s\n' "$cargo_command" >&2
        exit 2
        ;;
esac
command -v "$cargo_command" >/dev/null 2>&1 || {
    printf 'Cargo executable is unavailable: %s\n' "$cargo_command" >&2
    exit 1
}
command -v jq >/dev/null 2>&1 || {
    printf "jq is required to select Cargo's exact harness-free test executable\n" >&2
    exit 1
}
command -v systemd-run >/dev/null 2>&1 || {
    printf 'systemd-run is required for the delegated execution fixture\n' >&2
    exit 1
}
command -v systemctl >/dev/null 2>&1 || {
    printf 'systemctl is required to own and stop the delegated execution fixture\n' >&2
    exit 1
}
command -v setsid >/dev/null 2>&1 || {
    printf 'setsid is required to pin delegated client process-group ownership\n' >&2
    exit 1
}
command -v timeout >/dev/null 2>&1 || {
    printf 'timeout is required to bound the delegated systemd client\n' >&2
    exit 1
}
if ! timeout --kill-after=1s 10s systemctl --user show-environment >/dev/null 2>&1; then
    if [ "$mode" = fixture ] && [ "$require_execution" = 0 ]; then
        printf '%s\n' \
            'SKIP delegated execution fixture: no reachable systemd user manager; this is not execution success' >&2
        exit 0
    fi
    if [ "$mode" = preflight ]; then
        printf '%s\n' \
            'required delegated execution preflight has no reachable systemd user manager' >&2
    else
        printf '%s\n' \
            'required delegated execution fixture has no reachable systemd user manager' >&2
    fi
    exit 1
fi

cargo_messages=$(mktemp "$tmpdir/cast-delegated-fixture-cargo.XXXXXXXXXXXX")
invocation_token=${cargo_messages##*.}
case "$invocation_token" in
    ''|*[!A-Za-z0-9]*)
        printf 'mktemp returned an unsafe delegated invocation token: %s\n' "$invocation_token" >&2
        exit 1
        ;;
esac
unit="cast-delegated-fixture-$(id -u)-$$-$invocation_token.service"
unit_marker="CAST_DELEGATED_FIXTURE_TOKEN=$invocation_token"
systemd_run_pid=
systemd_launch_pid=
client_release_open=0
client_directory=
client_ready=
client_acknowledgement=
client_channels_ready=
client_status_fifo=
client_release_fifo=
unit_cleanup_failed=0

stop_owned_unit() {
    "$owned_unit_stopper" "$unit" "$unit_marker" "$client_kill_after_seconds"
}

wait_client_group_exit() {
    client_pid=$1
    timeout --kill-after=1s "${client_kill_after_seconds}s" sh -c '
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
                        if [ "$process_group" = "$target_group" ]; then
                            live_member=1
                            break
                        fi
                        ;;
                esac
            done
            [ "$live_member" -eq 1 ] || exit 0
            sleep 0.05
        done
    ' delegated-client-group-wait "$client_pid"
}

stop_client_group() {
    client_pid=$1
    if kill -0 "$client_pid" >/dev/null 2>&1 \
        || kill -0 "-$client_pid" >/dev/null 2>&1; then
        kill -TERM "-$client_pid" >/dev/null 2>&1 \
            || kill -TERM "$client_pid" >/dev/null 2>&1 \
            || :
        if ! wait_client_group_exit "$client_pid"; then
            kill -KILL "-$client_pid" >/dev/null 2>&1 \
                || kill -KILL "$client_pid" >/dev/null 2>&1 \
                || :
        fi
    fi
    if wait "$client_pid" 2>/dev/null; then
        return 0
    else
        client_status=$?
        return "$client_status"
    fi
}

cleanup() {
    status=$?
    # Cleanup owns the first interruption. A second terminal signal must not
    # tear it down between authenticating and stopping the transient unit.
    trap '' HUP INT TERM
    trap - EXIT
    set +e
    stop_owned_unit || unit_cleanup_failed=1
    if [ -n "$systemd_run_pid" ]; then
        stop_client_group "$systemd_run_pid"
        systemd_run_pid=
        if [ "$client_release_open" -eq 1 ]; then
            exec 8>&-
            client_release_open=0
        fi
        # Catch the narrow race in which systemd accepted the transient unit
        # immediately after the first ownership query.
        stop_owned_unit || unit_cleanup_failed=1
    elif [ "$client_release_open" -eq 1 ]; then
        exec 8>&-
        client_release_open=0
    fi
    if [ -n "$client_directory" ]; then
        rm -rf "$client_directory"
    fi
    rm -f "$cargo_messages"
    if [ -n "$proof_temporary" ]; then
        rm -f "$proof_temporary"
    fi
    if [ "$unit_cleanup_failed" -eq 1 ] && [ "$status" -eq 0 ]; then
        status=1
    fi
    if [ "$status" -ne 0 ] && [ -n "$proof_path" ]; then
        rm -f "$proof_path"
    fi
    exit "$status"
}
trap cleanup EXIT
trap 'exit 129' HUP
trap 'exit 130' INT
trap 'exit 143' TERM

printf 'Building the harness-free delegated fixture outside its delegated unit...\n'
if ! "$cargo_command" test --locked -p mason \
    --features delegated-fixture-test-support \
    --test delegated_execution_fixture \
    --no-run \
    --message-format=json-render-diagnostics >"$cargo_messages"
then
    jq -r 'select(.reason == "compiler-message") | .message.rendered // empty' "$cargo_messages" >&2 || true
    printf 'failed to build the harness-free delegated fixture target\n' >&2
    exit 1
fi
jq -r 'select(.reason == "compiler-message") | .message.rendered // empty' "$cargo_messages" >&2

if ! executable=$(jq -er -s --arg expected_source "$root/crates/mason/tests/delegated_execution_fixture.rs" '
    [ .[]
      | select(.reason == "compiler-artifact")
      | select(.target.name == "delegated_execution_fixture")
      | select(.target.kind == ["test"])
      | select(.target.crate_types == ["bin"])
      | select(.target.src_path == $expected_source)
      | select(.profile.test == true)
      | .executable
      | select(type == "string" and length > 0)
    ]
    | if length == 1 then .[0]
      else error("expected exactly one delegated_execution_fixture test executable, found \(length)")
      end
' "$cargo_messages")
then
    printf 'Cargo did not report exactly one harness-free delegated fixture executable\n' >&2
    exit 1
fi

case "$executable" in
    /*) ;;
    *) printf 'Cargo reported a non-absolute delegated fixture executable: %s\n' "$executable" >&2; exit 1 ;;
esac
if [ ! -f "$executable" ] || [ -L "$executable" ] || [ ! -x "$executable" ]; then
    printf 'Cargo reported an unsafe or non-executable delegated fixture artifact: %s\n' "$executable" >&2
    exit 1
fi

if [ "$mode" = preflight ]; then
    printf 'Running production capability preflight as a single-task delegated systemd service...\n'
else
    printf 'Running fixture selection %s as a single-task delegated systemd service...\n' "$fixture"
fi
if load_state=$(timeout --kill-after=1s 10s systemctl --user show \
    "$unit" --property=LoadState --value 2>/dev/null); then
    if [ "$load_state" != not-found ]; then
        printf 'refusing pre-existing delegated unit name %s with load state %s\n' "$unit" "$load_state" >&2
        exit 1
    fi
else
    printf 'could not authenticate delegated unit-name availability: %s\n' "$unit" >&2
    exit 1
fi
client_directory=$(mktemp -d "$tmpdir/cast-delegated-client.XXXXXXXXXXXX")
chmod 700 "$client_directory"
if [ -L "$client_directory" ] || [ ! -d "$client_directory" ] \
    || [ "$(stat -c '%u:%a' "$client_directory")" != "$(id -u):700" ]; then
    printf 'delegated client staging must be a caller-owned mode-700 directory\n' >&2
    exit 1
fi
client_status_fifo="$client_directory/status.fifo"
client_ready="$client_directory/ready"
client_acknowledgement="$client_directory/acknowledged"
client_channels_ready="$client_directory/channels-ready"
client_release_fifo="$client_directory/release.fifo"
mkfifo -m 600 "$client_status_fifo"
mkfifo -m 600 "$client_release_fifo"
# Temporary Linux FIFO anchors make the supervisor's one-way endpoint opens
# nonblocking. They are closed immediately after channel readiness is proven.
exec 5<>"$client_status_fifo"
exec 6<>"$client_release_fifo"
# Do not enter cleanup in the unavoidably narrow interval between starting the
# background client and assigning `$!`: defer a first signal until the child
# PID has been captured. The ordinary exit traps are restored immediately
# afterwards, before any wait can block.
launch_signal_status=
trap 'launch_signal_status=129' HUP
trap 'launch_signal_status=130' INT
trap 'launch_signal_status=143' TERM
set -- "$executable"
if [ "$mode" = preflight ]; then
    set -- \
        "--setenv=CAST_DELEGATED_PREFLIGHT_ONLY=1" \
        "$@"
else
    set -- \
        "--setenv=CAST_DELEGATED_PREFLIGHT_ONLY=0" \
        "--setenv=CAST_BOOTSTRAP_PACKAGE_STORE=$package_store" \
        "--setenv=CAST_EXECUTION_FIXTURE=$fixture" \
        "$@"
fi
if [ -n "$proof_path" ]; then
    set -- \
        "--setenv=CAST_FIXTURE_PROOF_PATH=$proof_path" \
        "--setenv=CAST_FIXTURE_GIT_COMMIT=$git_commit" \
        "$@"
fi
# Clear the parent EXIT trap before forking so the asynchronous Bash child
# cannot run repository cleanup if a signal lands before it execs systemd-run.
# First terminal signals remain deferred until cleanup ownership is restored.
trap - EXIT
CAST_LATCHED_KILL_AFTER_SECONDS="$client_kill_after_seconds" setsid "$latched_runner" \
    "$client_ready" "$client_acknowledgement" \
    "$client_channels_ready" "$client_status_fifo" \
    "$client_release_fifo" \
    --parent-loss-cleanup "$owned_unit_stopper" \
    "$unit" "$unit_marker" "$client_kill_after_seconds" -- \
    timeout --foreground --kill-after="${client_kill_after_seconds}s" \
    "${systemd_client_timeout_seconds}s" \
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
    --setenv="TMPDIR=$tmpdir" \
    --setenv="CAST_REQUIRE_EXECUTION=$require_execution" \
    --setenv=CAST_DELEGATED_FIXTURE_RUNNER=1 \
    --setenv="$unit_marker" \
    --setenv=RUST_BACKTRACE=1 \
    --property='Delegate=cpu memory pids' \
    --property=DelegateSubgroup=cast-supervisor \
    --property=ExitType=cgroup \
    --property=KillMode=control-group \
    --property="RuntimeMaxSec=$service_runtime_max" \
    --property=TimeoutStartSec=30s \
    --property="TimeoutStopSec=${client_kill_after_seconds}s" \
    --property=SendSIGKILL=yes \
    "$@" 5>&- 6>&- &
systemd_launch_pid=$!
trap cleanup EXIT
trap '[ -n "$launch_signal_status" ] || launch_signal_status=129' HUP
trap '[ -n "$launch_signal_status" ] || launch_signal_status=130' INT
trap '[ -n "$launch_signal_status" ] || launch_signal_status=143' TERM

# Numeric cleanup remains disabled until the helper proves that `$!` is its
# live session/process-group leader. It cannot launch systemd-run before the
# acknowledgement and abandons an unacknowledged launch after ten seconds.
ready_pid=
ready_attempt=0
while [ ! -f "$client_ready" ]; do
    ready_attempt=$((ready_attempt + 1))
    if [ "$ready_attempt" -gt 220 ]; then
        break
    fi
    kill -0 "$systemd_launch_pid" >/dev/null 2>&1 || break
    sleep 0.05
done
if [ -f "$client_ready" ] && [ ! -L "$client_ready" ] \
    && [ "$(stat -c '%u:%a:%h' "$client_ready")" = "$(id -u):600:1" ]; then
    IFS= read -r ready_pid <"$client_ready" || ready_pid=
fi
if [ "$ready_pid" != "$systemd_launch_pid" ]; then
    trap '' HUP INT TERM
    wait "$systemd_launch_pid" 2>/dev/null || :
    systemd_launch_pid=
    trap 'exit 129' HUP
    trap 'exit 130' INT
    trap 'exit 143' TERM
    if [ -n "$launch_signal_status" ]; then
        exit "$launch_signal_status"
    fi
    printf 'latched delegated supervisor did not prove launch PID ownership\n' >&2
    exit 1
fi
systemd_run_pid=$systemd_launch_pid
systemd_launch_pid=
trap 'exit 129' HUP
trap 'exit 130' INT
trap 'exit 143' TERM
if [ -n "$launch_signal_status" ]; then
    exit "$launch_signal_status"
fi
acknowledgement_temporary="${client_acknowledgement}.tmp.$$"
( umask 077; set -C; printf 'ready\n' >"$acknowledgement_temporary" ) || exit 1
mv "$acknowledgement_temporary" "$client_acknowledgement" || exit 1
channels_pid=
channels_attempt=0
while [ ! -f "$client_channels_ready" ]; do
    channels_attempt=$((channels_attempt + 1))
    if [ "$channels_attempt" -gt 220 ]; then
        break
    fi
    kill -0 "$systemd_run_pid" >/dev/null 2>&1 || break
    sleep 0.05
done
if [ -f "$client_channels_ready" ] && [ ! -L "$client_channels_ready" ] \
    && [ "$(stat -c '%u:%a:%h' "$client_channels_ready")" = "$(id -u):600:1" ]; then
    IFS= read -r channels_pid <"$client_channels_ready" || channels_pid=
fi
if [ "$channels_pid" != "$systemd_run_pid" ]; then
    trap '' HUP INT TERM
    stop_client_group "$systemd_run_pid"
    systemd_run_pid=
    exec 5>&-
    exec 6>&-
    trap 'exit 129' HUP
    trap 'exit 130' INT
    trap 'exit 143' TERM
    if [ -n "$launch_signal_status" ]; then
        exit "$launch_signal_status"
    fi
    printf 'latched delegated supervisor did not establish private channels\n' >&2
    exit 1
fi
exec 7<"$client_status_fifo"
exec 8>"$client_release_fifo"
client_release_open=1
exec 5>&-
exec 6>&-
trap 'exit 129' HUP
trap 'exit 130' INT
trap 'exit 143' TERM
if [ -n "$launch_signal_status" ]; then
    exit "$launch_signal_status"
fi

# Kill-capable traps remain active while systemd-run is executing; the
# acknowledged supervisor pins the PID/PGID until the explicit release.
status_received=0
status_read_status=0
if status=$(
    # Command substitution itself forks a shell before launching `timeout`.
    # Close release there too: killing the parent must not leave this orphaned
    # substitution holding the monitor's only liveness channel open.
    exec 8>&-
    timeout --foreground \
        --kill-after="${client_kill_after_seconds}s" \
        "${client_status_timeout_seconds}s" sh -c '
            exec 8>&-
            IFS= read -r status <&7 || exit 1
            printf "%s\n" "$status"
        ' delegated-status-reader 8>&-
); then
    status_received=1
    case "$status" in
        ''|*[!0-9]*) status=1 ;;
        *) [ "$status" -le 255 ] || status=1 ;;
    esac
else
    status_read_status=$?
    status=1
    if [ "$status_read_status" -eq 124 ] || [ "$status_read_status" -eq 137 ]; then
        printf 'latched delegated status channel exceeded its %s-second bound\n' \
            "$client_status_timeout_seconds" >&2
    fi
fi
exec 7<&-
launch_signal_status=
trap '[ -n "$launch_signal_status" ] || launch_signal_status=129' HUP
trap '[ -n "$launch_signal_status" ] || launch_signal_status=130' INT
trap '[ -n "$launch_signal_status" ] || launch_signal_status=143' TERM

# The supervisor remains the live group leader until this release. Signals are
# record-only through reap and numeric-ownership clearing, so cleanup can never
# target a PID which the kernel has already made reusable.
if [ "$status_received" -eq 1 ]; then
    trap '' PIPE
    if ! printf 'release\n' >&8; then
        status=1
    fi
    trap - PIPE
    exec 8>&-
    client_release_open=0
    if wait "$systemd_run_pid"; then
        latched_status=0
    else
        latched_status=$?
    fi
else
    # Keep release open so the monitor pins the PGID while status-EOF cleanup
    # stops the authenticated unit and every remaining client-group member.
    stop_owned_unit || unit_cleanup_failed=1
    if stop_client_group "$systemd_run_pid"; then
        latched_status=0
    else
        latched_status=$?
    fi
    systemd_run_pid=
    exec 8>&-
    client_release_open=0
fi
if [ -n "$launch_signal_status" ] && [ -n "$systemd_run_pid" ]; then
    trap '' HUP INT TERM
    if wait "$systemd_run_pid"; then
        latched_status=0
    else
        latched_status=$?
    fi
    trap '[ -n "$launch_signal_status" ] || launch_signal_status=129' HUP
    trap '[ -n "$launch_signal_status" ] || launch_signal_status=130' INT
    trap '[ -n "$launch_signal_status" ] || launch_signal_status=143' TERM
fi
if [ "$test_signal_after_reap" = 1 ]; then
    kill -TERM "$$"
fi
systemd_run_pid=
if [ "$status_received" -eq 0 ]; then
    if [ "$status_read_status" -eq 124 ] || [ "$status_read_status" -eq 137 ]; then
        status=124
    else
        status=$latched_status
        [ "$status" -ne 0 ] || status=1
    fi
elif [ "$latched_status" -ne "$status" ]; then
    printf 'latched delegated client status changed from %s to %s while reaping\n' \
        "$status" "$latched_status" >&2
    status=1
fi
trap 'exit 129' HUP
trap 'exit 130' INT
trap 'exit 143' TERM
if [ -n "$launch_signal_status" ]; then
    exit "$launch_signal_status"
fi
if [ "$status" -ne 0 ]; then
    exit "$status"
fi

if [ -n "$proof_path" ]; then
    git_commit_after=$(git -C "$root" rev-parse --verify HEAD) || {
        printf 'cannot revalidate the fixture CI Git commit after execution\n' >&2
        exit 1
    }
    if ! git_status_after=$(git -C "$root" status --porcelain --untracked-files=normal); then
        printf 'cannot inspect fixture CI checkout cleanliness after execution\n' >&2
        exit 1
    fi
    if [ "$git_commit_after" != "$git_commit" ] || [ -n "$git_status_after" ]; then
        printf 'fixture CI checkout changed while proving commit %s\n' "$git_commit" >&2
        exit 1
    fi
    if [ ! -f "$proof_path" ] || [ -L "$proof_path" ]; then
        printf 'required all-fixture harness did not emit one regular completion proof: %s\n' "$proof_path" >&2
        exit 1
    fi
    if [ "$(stat -c '%u' "$proof_path")" -ne "$(id -u)" ] \
        || [ "$(stat -c '%a' "$proof_path")" != 644 ] \
        || [ "$(stat -c '%h' "$proof_path")" -ne 1 ]; then
        printf 'fixture CI proof must be caller-owned, mode 644, and singly linked: %s\n' "$proof_path" >&2
        exit 1
    fi
    proof_size=$(stat -c '%s' "$proof_path")
    if [ "$proof_size" -le 0 ] || [ "$proof_size" -gt 4096 ]; then
        printf 'fixture CI proof exceeds its 4096-byte bound: %s bytes\n' "$proof_size" >&2
        exit 1
    fi
    if ! jq -s -e --arg commit "$git_commit" '
        length == 1 and .[0] == {
          schema: "cast.fixtures-ci-proof.v1",
          git_commit: $commit,
          git_tree: "clean",
          selection: "all",
          required_execution: true,
          fixture_count: 16,
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
            "generated-shell",
            "hooks-patch",
            "meson",
            "plugin-output",
            "split",
            "userspace-profile"
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
    ' "$proof_path" >/dev/null; then
        printf 'fixture CI proof does not exactly match the required commit and fixture matrix\n' >&2
        exit 1
    fi
    printf 'Published bounded fixture CI proof for commit %s: %s\n' "$git_commit" "$proof_path"
fi

exit 0
