#!/bin/sh

set -eu

root=$(CDPATH= cd -- "$(dirname -- "$0")/../.." && pwd -P)
runner="$root/misc/scripts/run-delegated-execution-fixture.sh"
work=$(mktemp -d "${TMPDIR:-/tmp}/cast-delegated-runner-test.XXXXXXXXXXXX")
cleanup() {
    rm -rf "$work"
}
trap cleanup EXIT HUP INT TERM

fakebin="$work/bin"
state="$work/state"
private_tmp="$work/tmp"
package_store="$work/packages"
artifact="$work/delegated_execution_fixture"
mkdir -p "$fakebin" "$state" "$private_tmp" "$package_store"
chmod 700 "$private_tmp"

cat >"$artifact" <<'EOF'
#!/bin/sh
exit 0
EOF
chmod 755 "$artifact"

cat >"$fakebin/cargo" <<'EOF'
#!/bin/sh
set -eu
: "${FAKE_ARTIFACT:?}"
: "${FAKE_SOURCE:?}"
printf '%s\n' cargo-called >>"$FAKE_STATE/cargo-calls"
printf '%s\n' \
    "{\"reason\":\"compiler-artifact\",\"target\":{\"name\":\"delegated_execution_fixture\",\"kind\":[\"test\"],\"crate_types\":[\"bin\"],\"src_path\":\"$FAKE_SOURCE\"},\"profile\":{\"test\":true},\"executable\":\"$FAKE_ARTIFACT\"}"
EOF
chmod 755 "$fakebin/cargo"

cat >"$fakebin/systemctl" <<'EOF'
#!/bin/sh
set -eu
: "${FAKE_STATE:?}"
test "$1" = --user
shift
case "$1" in
    show-environment)
        test "${FAKE_MANAGER:-ready}" = ready
        ;;
    show)
        case "$*" in
            *--property=LoadState*)
                if test -f "$FAKE_STATE/environment"; then
                    printf '%s\n' loaded
                else
                    printf '%s\n' not-found
                fi
                ;;
            *--property=Environment*)
                test ! -f "$FAKE_STATE/environment" || cat "$FAKE_STATE/environment"
                ;;
            *) exit 2 ;;
        esac
        ;;
    stop)
        shift
        printf '%s\n' "$1" >>"$FAKE_STATE/stops"
        case "${FAKE_STOP_MODE:-success}" in
            success) ;;
            fail) exit 1 ;;
            signal) kill -TERM "$PPID" ;;
            *) exit 2 ;;
        esac
        : >"$FAKE_STATE/stopped"
        rm -f "$FAKE_STATE/environment"
        ;;
    kill)
        printf '%s\n' "$*" >>"$FAKE_STATE/kills"
        ;;
    *)
        printf 'unexpected fake systemctl invocation: %s\n' "$*" >&2
        exit 2
        ;;
esac
EOF
chmod 755 "$fakebin/systemctl"

cat >"$fakebin/systemd-run" <<'EOF'
#!/bin/sh
set -eu
: "${FAKE_STATE:?}"
unit=
marker=
: >"$FAKE_STATE/systemd-run-args"
for argument in "$@"; do
    printf '%s\n' "$argument" >>"$FAKE_STATE/systemd-run-args"
    case "$argument" in
        --unit=*) unit=${argument#--unit=} ;;
        --setenv=CAST_DELEGATED_FIXTURE_TOKEN=*) marker=${argument#--setenv=} ;;
    esac
done
test -n "$unit"
test -n "$marker"
printf '%s\n' "$unit" >"$FAKE_STATE/unit"

case "${FAKE_SYSTEMD_RUN_MODE:-success}" in
    success)
        printf '%s\n' "$marker" >"$FAKE_STATE/environment"
        exit 0
        ;;
    failure)
        printf '%s\n' "$marker" >"$FAKE_STATE/environment"
        exit 42
        ;;
    signal)
        printf '%s\n' "$marker" >"$FAKE_STATE/environment"
        kill -TERM "$PPID"
        attempts=0
        while test ! -f "$FAKE_STATE/stopped" && test "$attempts" -lt 100; do
            attempts=$((attempts + 1))
            sleep 0.05
        done
        exit 143
        ;;
    signal-before-accept)
        # Model systemd accepting the request independently after the CLI has
        # already signalled its caller and ignored cleanup's TERM.
        trap '' TERM
        kill -TERM "$PPID"
        sleep 0.1
        printf '%s\n' "$marker" >"$FAKE_STATE/environment"
        exit 143
        ;;
    foreign)
        printf '%s\n' 'CAST_DELEGATED_FIXTURE_TOKEN=foreign' >"$FAKE_STATE/environment"
        exit 42
        ;;
    *) exit 2 ;;
esac
EOF
chmod 755 "$fakebin/systemd-run"

run_fixture() {
    env \
        PATH="$fakebin:$PATH" \
        TMPDIR="$private_tmp" \
        CAST_BOOTSTRAP_PACKAGE_STORE="$package_store" \
        CAST_REQUIRE_EXECUTION="$1" \
        CARGO="$fakebin/cargo" \
        FAKE_ARTIFACT="$artifact" \
        FAKE_SOURCE="$root/crates/mason/tests/delegated_execution_fixture.rs" \
        FAKE_STATE="$state" \
        FAKE_MANAGER="$2" \
        FAKE_SYSTEMD_RUN_MODE="$3" \
        FAKE_STOP_MODE="${4:-success}" \
        "$runner" custom
}

reset_state() {
    rm -f "$state"/*
}

reset_state
run_fixture 1 ready success
args="$state/systemd-run-args"
unit=$(cat "$state/unit")
case "$unit" in
    cast-delegated-fixture-*-*-*.service) ;;
    *) printf 'unsafe delegated unit name: %s\n' "$unit" >&2; exit 1 ;;
esac
for expected in \
    --wait \
    --pipe \
    --collect \
    --property=ExitType=cgroup \
    --property=KillMode=control-group \
    --property=RuntimeMaxSec=2h \
    --property=TimeoutStartSec=30s \
    --property=TimeoutStopSec=30s \
    --property=SendSIGKILL=yes \
    --setenv=CAST_DELEGATED_FIXTURE_RUNNER=1
do
    grep -Fqx -- "$expected" "$args"
done
grep -Fqx -- "--unit=$unit" "$args"
grep -Fqx -- "$unit" "$state/stops"

reset_state
set +e
run_fixture 1 ready signal >"$work/signal.out" 2>"$work/signal.err"
status=$?
set -e
test "$status" -eq 143
unit=$(cat "$state/unit")
grep -Fqx -- "$unit" "$state/stops"

reset_state
set +e
run_fixture 1 ready signal-before-accept >"$work/delayed.out" 2>"$work/delayed.err"
status=$?
set -e
test "$status" -eq 143
unit=$(cat "$state/unit")
grep -Fqx -- "$unit" "$state/stops"

reset_state
set +e
run_fixture 1 ready failure >"$work/failure.out" 2>"$work/failure.err"
status=$?
set -e
test "$status" -eq 42
unit=$(cat "$state/unit")
grep -Fqx -- "$unit" "$state/stops"

reset_state
set +e
run_fixture 1 ready foreign >"$work/foreign.out" 2>"$work/foreign.err"
status=$?
set -e
test "$status" -eq 42
grep -Fq 'refusing to stop delegated unit without this invocation marker' "$work/foreign.err"
test ! -e "$state/stops"
test ! -e "$state/kills"

reset_state
set +e
run_fixture 1 ready failure fail >"$work/stop-failure.out" 2>"$work/stop-failure.err"
status=$?
set -e
test "$status" -eq 42
unit=$(cat "$state/unit")
grep -Fqx -- "kill --kill-whom=all --signal=SIGKILL $unit" "$state/kills"
grep -Fq 'forcing its control group down' "$work/stop-failure.err"

reset_state
set +e
run_fixture 1 ready failure signal >"$work/second-signal.out" 2>"$work/second-signal.err"
status=$?
set -e
test "$status" -eq 42
unit=$(cat "$state/unit")
grep -Fqx -- "$unit" "$state/stops"

reset_state
run_fixture 0 missing success >"$work/optional.out" 2>"$work/optional.err"
grep -Fq 'SKIP delegated execution fixture' "$work/optional.err"
test ! -e "$state/cargo-calls"

reset_state
set +e
run_fixture 1 missing success >"$work/required.out" 2>"$work/required.err"
status=$?
set -e
test "$status" -eq 1
grep -Fq 'no reachable systemd user manager' "$work/required.err"
test ! -e "$state/cargo-calls"

printf '%s\n' 'delegated fixture runner lifecycle tests passed'
