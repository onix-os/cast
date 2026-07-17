#!/bin/sh

set -eu

root=$(CDPATH= cd -- "$(dirname -- "$0")/../.." && pwd -P)
runner="$root/misc/scripts/run-delegated-execution-fixture.sh"
proof_generator="$root/misc/scripts/test-support/write-fixtures-ci-proof-v2.sh"
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
evidence="$work/evidence"
fake_commit=0123456789abcdef0123456789abcdef01234567
mkdir -p "$fakebin" "$state" "$private_tmp" "$package_store" "$evidence"
chmod 700 "$private_tmp" "$evidence"
if [ -L "$proof_generator" ] || [ ! -f "$proof_generator" ] \
    || [ ! -x "$proof_generator" ]; then
    printf 'fixture proof test generator is unavailable or unsafe: %s\n' \
        "$proof_generator" >&2
    exit 1
fi

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
if test "${FAKE_CARGO_MODE:-artifact}" = missing-artifact; then
    printf '%s\n' '{"reason":"build-finished","success":true}'
    exit 0
fi
printf '%s\n' \
    "{\"reason\":\"compiler-artifact\",\"target\":{\"name\":\"delegated_execution_fixture\",\"kind\":[\"test\"],\"crate_types\":[\"bin\"],\"src_path\":\"$FAKE_SOURCE\"},\"profile\":{\"test\":true},\"executable\":\"$FAKE_ARTIFACT\"}"
EOF
chmod 755 "$fakebin/cargo"

cat >"$fakebin/git" <<'EOF'
#!/bin/sh
set -eu
: "${FAKE_GIT_COMMIT:?}"
: "${FAKE_STATE:?}"
test "$1" = -C
shift 2
case "$1" in
    rev-parse)
        test "$2" = --verify
        test "$3" = HEAD
        printf '%s\n' "$FAKE_GIT_COMMIT"
        ;;
    status)
        test "$2" = --porcelain
        test "$3" = --untracked-files=normal
        printf '%s\n' status >>"$FAKE_STATE/git-status-calls"
        status_call=$(wc -l <"$FAKE_STATE/git-status-calls")
        case "${FAKE_GIT_STATUS_MODE:-clean}" in
            clean) ;;
            dirty) printf '%s\n' ' M fixture-input' ;;
            fail-before) exit 71 ;;
            fail-after)
                test "$status_call" -lt 2 || exit 72
                ;;
            *) exit 2 ;;
        esac
        ;;
    *) exit 2 ;;
esac
EOF
chmod 755 "$fakebin/git"

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
            *--property=ActiveState*)
                if test -f "$FAKE_STATE/environment"; then
                    printf '%s\n' active
                else
                    printf '%s\n' inactive
                fi
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
proof=
commit=
: >"$FAKE_STATE/systemd-run-args"
for argument in "$@"; do
    printf '%s\n' "$argument" >>"$FAKE_STATE/systemd-run-args"
    case "$argument" in
        --unit=*) unit=${argument#--unit=} ;;
        --setenv=CAST_DELEGATED_FIXTURE_TOKEN=*) marker=${argument#--setenv=} ;;
        --setenv=CAST_FIXTURE_PROOF_PATH=*) proof=${argument#--setenv=CAST_FIXTURE_PROOF_PATH=} ;;
        --setenv=CAST_FIXTURE_GIT_COMMIT=*) commit=${argument#--setenv=CAST_FIXTURE_GIT_COMMIT=} ;;
    esac
done
test -n "$unit"
test -n "$marker"
printf '%s\n' "$unit" >"$FAKE_STATE/unit"

case "${FAKE_SYSTEMD_RUN_MODE:-success}" in
    success)
        printf '%s\n' "$marker" >"$FAKE_STATE/environment"
        if test -n "$proof"; then
            case "${FAKE_PROOF_MODE:-valid}" in
                valid)
                    test -n "$commit"
                    : "${FAKE_PROOF_GENERATOR:?}"
                    "$FAKE_PROOF_GENERATOR" "$proof" "$commit"
                    ;;
                multi-document)
                    test -n "$commit"
                    : "${FAKE_PROOF_GENERATOR:?}"
                    "$FAKE_PROOF_GENERATOR" "$proof" "$commit"
                    printf '%s\n' '{"ignored":"document"}' >>"$proof"
                    ;;
                missing) ;;
                malformed) printf '%s\n' '{"result":"passed"}' >"$proof"; chmod 644 "$proof" ;;
                *) exit 2 ;;
            esac
        fi
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
    direct-supervisor-signal)
        printf '%s\n' "$marker" >"$FAKE_STATE/environment"
        kill -TERM "${CAST_LATCHED_SUPERVISOR_PID:?}"
        sleep 5
        exit 143
        ;;
    freeze-supervisor)
        printf '%s\n' "$marker" >"$FAKE_STATE/environment"
        printf '%s\n' "${CAST_LATCHED_SUPERVISOR_PID:?}" \
            >"$FAKE_STATE/supervisor-pid"
        kill -STOP "${CAST_LATCHED_SUPERVISOR_PID:?}"
        : >"$FAKE_STATE/frozen-supervisor-ready"
        while :; do
            sleep 1
        done
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
    selector=$1
    env \
        PATH="$fakebin:$PATH" \
        TMPDIR="$private_tmp" \
        CAST_BOOTSTRAP_PACKAGE_STORE="$package_store" \
        CAST_FIXTURE_EVIDENCE_DIR="$evidence" \
        CAST_REQUIRE_EXECUTION="$2" \
        CAST_DELEGATED_PREFLIGHT_ONLY=1 \
        CARGO="$fakebin/cargo" \
        FAKE_ARTIFACT="$artifact" \
        FAKE_PROOF_GENERATOR="$proof_generator" \
        FAKE_SOURCE="$root/crates/mason/tests/delegated_execution_fixture.rs" \
        FAKE_STATE="$state" \
        FAKE_MANAGER="$3" \
        FAKE_SYSTEMD_RUN_MODE="$4" \
        FAKE_STOP_MODE="${5:-success}" \
        FAKE_PROOF_MODE="${6:-valid}" \
        FAKE_CARGO_MODE="${7:-artifact}" \
        FAKE_GIT_COMMIT="$fake_commit" \
        FAKE_GIT_STATUS_MODE="${8:-clean}" \
        CAST_DELEGATED_KILL_AFTER_SECONDS="${CAST_DELEGATED_KILL_AFTER_SECONDS-30}" \
        CAST_DELEGATED_STATUS_TIMEOUT_SECONDS="${CAST_DELEGATED_STATUS_TIMEOUT_SECONDS-}" \
        CAST_FIXTURE_TEST_SIGNAL_AFTER_LATCHED_REAP="${CAST_FIXTURE_TEST_SIGNAL_AFTER_LATCHED_REAP-}" \
        "$runner" "$selector"
}

run_preflight() {
    env \
        PATH="$fakebin:$PATH" \
        TMPDIR="$private_tmp" \
        CAST_BOOTSTRAP_PACKAGE_STORE="$work/definitely-missing-packages" \
        CAST_FIXTURE_EVIDENCE_DIR="$work/definitely-missing-evidence" \
        CAST_REQUIRE_EXECUTION="$1" \
        CAST_DELEGATED_PREFLIGHT_ONLY=0 \
        CARGO="$fakebin/cargo" \
        FAKE_ARTIFACT="$artifact" \
        FAKE_PROOF_GENERATOR="$proof_generator" \
        FAKE_SOURCE="$root/crates/mason/tests/delegated_execution_fixture.rs" \
        FAKE_STATE="$state" \
        FAKE_MANAGER="$2" \
        FAKE_SYSTEMD_RUN_MODE="$3" \
        FAKE_STOP_MODE="${4:-success}" \
        CAST_DELEGATED_KILL_AFTER_SECONDS="${CAST_DELEGATED_KILL_AFTER_SECONDS-}" \
        CAST_DELEGATED_STATUS_TIMEOUT_SECONDS="${CAST_DELEGATED_STATUS_TIMEOUT_SECONDS-}" \
        "$runner" --preflight-only
}

reset_state() {
    rm -f "$state"/*
}

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

reset_state
run_fixture custom 1 ready success
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
    --setenv=CAST_DELEGATED_FIXTURE_RUNNER=1 \
    --setenv=CAST_DELEGATED_PREFLIGHT_ONLY=0
do
    grep -Fqx -- "$expected" "$args"
done
grep -Fqx -- "--unit=$unit" "$args"
grep -Fqx -- "$unit" "$state/stops"

reset_state
run_preflight 1 ready success
args="$state/systemd-run-args"
unit=$(cat "$state/unit")
test -e "$state/cargo-calls"
test ! -e "$work/definitely-missing-packages"
test ! -e "$work/definitely-missing-evidence"
for expected in \
    --wait \
    --pipe \
    --collect \
    '--property=Delegate=cpu memory pids' \
    --property=DelegateSubgroup=cast-supervisor \
    --property=ExitType=cgroup \
    --property=KillMode=control-group \
    --property=RuntimeMaxSec=30s \
    --property=TimeoutStartSec=30s \
    --property=TimeoutStopSec=5s \
    --property=SendSIGKILL=yes \
    --setenv=CAST_DELEGATED_FIXTURE_RUNNER=1 \
    --setenv=CAST_DELEGATED_PREFLIGHT_ONLY=1 \
    --setenv=CAST_REQUIRE_EXECUTION=1 \
    "$artifact"
do
    grep -Fqx -- "$expected" "$args"
done
if grep -Fq -- '--setenv=CAST_BOOTSTRAP_PACKAGE_STORE=' "$args" \
    || grep -Fq -- '--setenv=CAST_EXECUTION_FIXTURE=' "$args" \
    || grep -Fq -- '--setenv=CAST_FIXTURE_PROOF_PATH=' "$args" \
    || grep -Fq -- '--setenv=CAST_FIXTURE_GIT_COMMIT=' "$args"; then
    printf 'preflight leaked fixture materialization or proof state into its unit\n' >&2
    exit 1
fi
grep -Fqx -- "$unit" "$state/stops"

reset_state
set +e
run_preflight 0 ready success >"$work/preflight-optional.out" 2>"$work/preflight-optional.err"
status=$?
set -e
test "$status" -eq 2
grep -Fq 'delegated execution preflight requires CAST_REQUIRE_EXECUTION=1' \
    "$work/preflight-optional.err"
test ! -e "$state/cargo-calls"
test ! -e "$state/systemd-run-args"

reset_state
set +e
run_preflight 1 missing success >"$work/preflight-manager.out" 2>"$work/preflight-manager.err"
status=$?
set -e
test "$status" -eq 1
grep -Fq 'required delegated execution preflight has no reachable systemd user manager' \
    "$work/preflight-manager.err"
test ! -e "$state/cargo-calls"
test ! -e "$state/systemd-run-args"

reset_state
set +e
CAST_DELEGATED_KILL_AFTER_SECONDS=1 \
    CAST_DELEGATED_STATUS_TIMEOUT_SECONDS=1 \
    run_preflight 1 ready freeze-supervisor \
    >"$work/preflight-timeout.out" 2>"$work/preflight-timeout.err"
preflight_timeout_status=$?
set -e
preflight_supervisor_pid=$(cat "$state/supervisor-pid")
preflight_unit=$(cat "$state/unit")
test "$preflight_timeout_status" -eq 124
if process_is_live "$preflight_supervisor_pid"; then
    printf 'timed-out delegated preflight supervisor remained live: %s\n' \
        "$preflight_supervisor_pid" >&2
    exit 1
fi
grep -Fqx -- "$preflight_unit" "$state/stops"
grep -Fq 'status channel exceeded its 1-second bound' \
    "$work/preflight-timeout.err"

fixture_count=0
for fixture_directory in \
    "$root/tests/fixtures/gluon/execution/packages"/* \
    "$root/tests/fixtures/gluon/userspace-profile"; do
    test -d "$fixture_directory" || continue
    fixture=${fixture_directory##*/}
    fixture_count=$((fixture_count + 1))
    reset_state
    run_fixture "$fixture" 1 ready success
    grep -Fqx -- "--setenv=CAST_EXECUTION_FIXTURE=$fixture" "$state/systemd-run-args"
done
test "$fixture_count" -eq 16
test ! -e "$evidence/fixtures-ci-proof.json"

rm -f "$evidence"/*
reset_state
run_fixture all 1 ready success
proof="$evidence/fixtures-ci-proof.json"
test -f "$proof"
test ! -L "$proof"
test "$(stat -c '%a' "$proof")" = 644
test "$(stat -c '%h' "$proof")" -eq 1
test "$(stat -c '%s' "$proof")" -le 131072
jq -e --arg commit "$fake_commit" '
    .schema == "cast.fixtures-ci-proof.v2"
    and .git_commit == $commit
    and .git_tree == "clean"
    and .selection == "all"
    and .required_execution == true
    and .bundle_ledger_schema == "cast.fixtures-ci.bundle.v1"
    and .totals == {
        fixture_count: 16,
        execution_count: 32,
        bundle_validation_count: 48,
        stone_count: 104,
        manifest_count: 32,
        artifact_count: 136,
        artifact_bytes: .totals.artifact_bytes
    }
    and (.fixtures | length) == 16
    and .fixtures[0].name == "autotools"
    and .fixtures[15].name == "userspace-profile"
    and ([.fixtures[].artifacts.stone_count] | add) == 104
    and ([.fixtures[].artifacts.manifest_count] | add) == 32
    and ([.fixtures[].artifacts.artifact_count] | add) == 136
    and .result == "passed"
' "$proof" >/dev/null
grep -Fqx -- "--setenv=CAST_FIXTURE_PROOF_PATH=$proof" "$state/systemd-run-args"
grep -Fqx -- "--setenv=CAST_FIXTURE_GIT_COMMIT=$fake_commit" "$state/systemd-run-args"

rm -f "$evidence"/*
reset_state
set +e
run_fixture all 1 ready success success valid artifact fail-before \
    >"$work/git-status-before.out" 2>"$work/git-status-before.err"
status=$?
set -e
test "$status" -eq 1
grep -Fq 'cannot inspect fixture CI checkout cleanliness before execution' \
    "$work/git-status-before.err"
test ! -e "$proof"
test ! -e "$state/cargo-calls"
test ! -e "$state/systemd-run-args"

rm -f "$evidence"/*
reset_state
set +e
run_fixture all 1 ready success success valid artifact fail-after \
    >"$work/git-status-after.out" 2>"$work/git-status-after.err"
status=$?
set -e
test "$status" -eq 1
grep -Fq 'cannot inspect fixture CI checkout cleanliness after execution' \
    "$work/git-status-after.err"
test ! -e "$proof"
test -e "$state/cargo-calls"
test -e "$state/systemd-run-args"

rm -f "$evidence"/*
reset_state
set +e
run_fixture all 1 ready success success missing >"$work/missing-proof.out" 2>"$work/missing-proof.err"
status=$?
set -e
test "$status" -eq 1
grep -Fq 'did not emit one regular completion proof' "$work/missing-proof.err"
test ! -e "$proof"

rm -f "$evidence"/*
reset_state
set +e
run_fixture all 1 ready success success malformed >"$work/malformed-proof.out" 2>"$work/malformed-proof.err"
status=$?
set -e
test "$status" -eq 1
grep -Fq 'does not exactly match' "$work/malformed-proof.err"
test ! -e "$proof"

rm -f "$evidence"/*
reset_state
set +e
run_fixture all 1 ready success success multi-document \
    >"$work/multi-document-proof.out" 2>"$work/multi-document-proof.err"
status=$?
set -e
test "$status" -eq 1
grep -Fq 'does not exactly match' "$work/multi-document-proof.err"
test ! -e "$proof"

printf '%s\n' stale-proof >"$proof"
reset_state
set +e
run_fixture all 1 ready failure >"$work/failed-matrix.out" 2>"$work/failed-matrix.err"
status=$?
set -e
test "$status" -eq 42
test ! -e "$proof"

reset_state
set +e
run_fixture all 1 ready success success valid missing-artifact >"$work/missing-artifact.out" 2>"$work/missing-artifact.err"
status=$?
set -e
test "$status" -eq 1
grep -Fq 'did not report exactly one harness-free delegated fixture executable' "$work/missing-artifact.err"
test ! -e "$proof"
test ! -e "$state/systemd-run-args"

reset_state
set +e
run_fixture not-an-execution-fixture 1 ready success >"$work/invalid.out" 2>"$work/invalid.err"
status=$?
set -e
test "$status" -eq 2
grep -Fq 'argument must be exactly `--preflight-only`, `all`, or one of:' "$work/invalid.err"
test ! -e "$state/cargo-calls"
test ! -e "$state/systemd-run-args"

reset_state
set +e
run_fixture custom 1 ready signal >"$work/signal.out" 2>"$work/signal.err"
status=$?
set -e
test "$status" -eq 143
unit=$(cat "$state/unit")
grep -Fqx -- "$unit" "$state/stops"

reset_state
set +e
run_fixture custom 1 ready signal-before-accept >"$work/delayed.out" 2>"$work/delayed.err"
status=$?
set -e
test "$status" -eq 143
unit=$(cat "$state/unit")
grep -Fqx -- "$unit" "$state/stops"

reset_state
set +e
run_fixture custom 1 ready direct-supervisor-signal \
    >"$work/direct-supervisor-signal.out" 2>"$work/direct-supervisor-signal.err"
status=$?
set -e
test "$status" -eq 143
unit=$(cat "$state/unit")
grep -Fqx -- "$unit" "$state/stops"

reset_state
set +e
CAST_DELEGATED_KILL_AFTER_SECONDS=1 \
    CAST_DELEGATED_STATUS_TIMEOUT_SECONDS=1 \
    run_fixture custom 1 ready freeze-supervisor \
    >"$work/frozen-supervisor.out" 2>"$work/frozen-supervisor.err"
frozen_status=$?
set -e
frozen_supervisor_pid=$(cat "$state/supervisor-pid")
frozen_unit=$(cat "$state/unit")
test "$frozen_status" -eq 124
if process_is_live "$frozen_supervisor_pid"; then
    printf 'autonomously bounded delegated supervisor remained live: %s\n' \
        "$frozen_supervisor_pid" >&2
    exit 1
fi
grep -Fqx -- "$frozen_unit" "$state/stops"
grep -Fq 'status channel exceeded its 1-second bound' \
    "$work/frozen-supervisor.err"

rm -f "$evidence"/*
reset_state
set +e
CAST_FIXTURE_TEST_SIGNAL_AFTER_LATCHED_REAP=1 \
    run_fixture all 1 ready success >"$work/post-reap-signal.out" 2>"$work/post-reap-signal.err"
status=$?
set -e
test "$status" -eq 143
test ! -e "$evidence/fixtures-ci-proof.json"
unit=$(cat "$state/unit")
grep -Fqx -- "$unit" "$state/stops"

reset_state
set +e
run_fixture custom 1 ready failure >"$work/failure.out" 2>"$work/failure.err"
status=$?
set -e
test "$status" -eq 42
unit=$(cat "$state/unit")
grep -Fqx -- "$unit" "$state/stops"

reset_state
set +e
run_fixture custom 1 ready foreign >"$work/foreign.out" 2>"$work/foreign.err"
status=$?
set -e
test "$status" -eq 42
grep -Fq 'refusing to stop fixture unit without this invocation marker' "$work/foreign.err"
test ! -e "$state/stops"
test ! -e "$state/kills"

reset_state
set +e
run_fixture custom 1 ready failure fail >"$work/stop-failure.out" 2>"$work/stop-failure.err"
status=$?
set -e
test "$status" -eq 42
unit=$(cat "$state/unit")
grep -Fqx -- "kill --kill-whom=all --signal=SIGKILL $unit" "$state/kills"
grep -Fq 'forcing its control group down' "$work/stop-failure.err"

reset_state
set +e
run_fixture custom 1 ready failure signal >"$work/second-signal.out" 2>"$work/second-signal.err"
status=$?
set -e
test "$status" -eq 42
unit=$(cat "$state/unit")
grep -Fqx -- "$unit" "$state/stops"

reset_state
run_fixture custom 0 missing success >"$work/optional.out" 2>"$work/optional.err"
grep -Fq 'SKIP delegated execution fixture' "$work/optional.err"
test ! -e "$state/cargo-calls"

reset_state
set +e
run_fixture custom 1 missing success >"$work/required.out" 2>"$work/required.err"
status=$?
set -e
test "$status" -eq 1
grep -Fq 'no reachable systemd user manager' "$work/required.err"
test ! -e "$state/cargo-calls"

printf '%s\n' 'delegated fixture runner lifecycle tests passed'
