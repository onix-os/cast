#!/bin/sh

set -eu

if [ "$#" -ne 1 ]; then
    printf 'usage: %s <all|fixture-name>\n' "$0" >&2
    exit 2
fi

fixture=$1
case "$fixture" in
    all|autotools|autotools-options|cargo|cargo-features|cargo-vendored|cmake|custom|daemon-generated|factory-override|generated-config|hooks-patch|meson|split) ;;
    *)
        printf '%s\n' \
            'fixture must be exactly `all` or one of: autotools autotools-options cargo cargo-features cargo-vendored cmake custom daemon-generated factory-override generated-config hooks-patch meson split' >&2
        exit 2
        ;;
esac

root=$(CDPATH= cd -- "$(dirname -- "$0")/../.." && pwd -P)
tmpdir=${TMPDIR-}
package_store=${CAST_BOOTSTRAP_PACKAGE_STORE:-$root/target/bootstrap-fixtures/packages}
require_execution=${CAST_REQUIRE_EXECUTION-}
cargo_command=${CARGO:-cargo}

case "$require_execution" in
    0|1) ;;
    *)
        printf 'CAST_REQUIRE_EXECUTION must be set to exactly 0 or 1; got %s\n' \
            "${require_execution:-<unset>}" >&2
        exit 2
        ;;
esac

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

case "$package_store" in
    /*) ;;
    *) printf 'CAST_BOOTSTRAP_PACKAGE_STORE must be absolute: %s\n' "$package_store" >&2; exit 2 ;;
esac
if [ ! -d "$package_store" ] || [ -L "$package_store" ]; then
    printf 'verified bootstrap package store is unavailable at %s; run `make bootstrap-fixtures-prepare` first\n' \
        "$package_store" >&2
    exit 1
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
if ! systemctl --user show-environment >/dev/null 2>&1; then
    if [ "$require_execution" = 0 ]; then
        printf '%s\n' \
            'SKIP delegated execution fixture: no reachable systemd user manager; this is not execution success' >&2
        exit 0
    fi
    printf '%s\n' \
        'required delegated execution fixture has no reachable systemd user manager' >&2
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

stop_owned_unit() {
    environment=$(systemctl --user show "$unit" --property=Environment --value 2>/dev/null) || return 0
    test -n "$environment" || return 0
    case " $environment " in
        *" $unit_marker "*) ;;
        *)
            printf 'refusing to stop delegated unit without this invocation marker: %s\n' "$unit" >&2
            return 0
            ;;
    esac

    if ! systemctl --user stop "$unit" >/dev/null 2>&1; then
        printf 'normal stop failed for owned delegated unit %s; forcing its control group down\n' "$unit" >&2
        systemctl --user kill --kill-whom=all --signal=SIGKILL "$unit" >/dev/null 2>&1 || :
        systemctl --user stop "$unit" >/dev/null 2>&1 || :
    fi
}

cleanup() {
    status=$?
    trap - EXIT
    # Cleanup owns the first interruption. A second terminal signal must not
    # tear it down between authenticating and stopping the transient unit.
    trap '' HUP INT TERM
    stop_owned_unit
    if [ -n "$systemd_run_pid" ]; then
        kill -TERM "$systemd_run_pid" >/dev/null 2>&1 || :
        wait "$systemd_run_pid" 2>/dev/null || :
        # Catch the narrow race in which systemd accepted the transient unit
        # immediately after the first ownership query.
        stop_owned_unit
    fi
    rm -f "$cargo_messages"
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

printf 'Running fixture selection %s as a single-task delegated systemd service...\n' "$fixture"
if load_state=$(systemctl --user show "$unit" --property=LoadState --value 2>/dev/null); then
    if [ "$load_state" != not-found ]; then
        printf 'refusing pre-existing delegated unit name %s with load state %s\n' "$unit" "$load_state" >&2
        exit 1
    fi
else
    printf 'could not authenticate delegated unit-name availability: %s\n' "$unit" >&2
    exit 1
fi
# Do not enter cleanup in the unavoidably narrow interval between starting the
# background client and assigning `$!`: defer a first signal until the child
# PID has been captured. The ordinary exit traps are restored immediately
# afterwards, before any wait can block.
launch_signal_status=
trap 'launch_signal_status=129' HUP
trap 'launch_signal_status=130' INT
trap 'launch_signal_status=143' TERM
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
    --setenv="CAST_BOOTSTRAP_PACKAGE_STORE=$package_store" \
    --setenv="CAST_EXECUTION_FIXTURE=$fixture" \
    --setenv="CAST_REQUIRE_EXECUTION=$require_execution" \
    --setenv=CAST_DELEGATED_FIXTURE_RUNNER=1 \
    --setenv="$unit_marker" \
    --setenv=RUST_BACKTRACE=1 \
    --property='Delegate=cpu memory pids' \
    --property=DelegateSubgroup=cast-supervisor \
    --property=ExitType=cgroup \
    --property=KillMode=control-group \
    --property=RuntimeMaxSec=2h \
    --property=TimeoutStartSec=30s \
    --property=TimeoutStopSec=30s \
    --property=SendSIGKILL=yes \
    "$executable" &
systemd_run_pid=$!
trap 'exit 129' HUP
trap 'exit 130' INT
trap 'exit 143' TERM
if [ -n "$launch_signal_status" ]; then
    exit "$launch_signal_status"
fi
if wait "$systemd_run_pid"; then
    status=0
else
    status=$?
fi
systemd_run_pid=
exit "$status"
