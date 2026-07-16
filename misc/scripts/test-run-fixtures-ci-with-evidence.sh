#!/bin/sh

set -eu

root=$(CDPATH= cd -- "$(dirname -- "$0")/../.." && pwd -P)
runner="$root/misc/scripts/run-fixtures-ci-with-evidence.sh"
work=$(mktemp -d "${TMPDIR:-/tmp}/cast-fixtures-ci-evidence-test.XXXXXXXXXXXX")
cleanup() {
    rm -rf "$work"
}
trap cleanup EXIT HUP INT TERM

fakebin="$work/bin"
evidence="$work/evidence"
outer_state="$work/outer-state"
real_tee=$(command -v tee)
real_jq=$(command -v jq)
fake_commit=$(git -C "$root" rev-parse --verify HEAD)
mkdir -p "$fakebin" "$evidence" "$outer_state"
chmod 700 "$evidence"

grep -Fq 'CAST_FIXTURE_EVIDENCE_DIR="$${CAST_FIXTURE_EVIDENCE_DIR:-$(TOP_DIR)/target/fixture-evidence}"' \
    "$root/Makefile"
if grep -Fq 'FIXTURE_EVIDENCE_DIR ?=' "$root/Makefile"; then
    printf '%s\n' 'fixture evidence must not cross a Make-expanded path variable' >&2
    exit 1
fi

cat >"$fakebin/make" <<'EOF'
#!/bin/sh
set -eu
: "${FAKE_MAKE_MODE:?}"
: "${CAST_FIXTURE_EVIDENCE_DIR:?}"
test "${1-}" = --no-print-directory
test "${2-}" = -C
test "${4-}" = fixtures-ci
test "$#" -eq 4
repository=$3

emit_proof() {
    commit=$(git -C "$repository" rev-parse --verify HEAD)
    cat >"$CAST_FIXTURE_EVIDENCE_DIR/fixtures-ci-proof.json" <<EOF_PROOF
{
  "schema": "cast.fixtures-ci-proof.v1",
  "git_commit": "$commit",
  "git_tree": "clean",
  "selection": "all",
  "required_execution": true,
  "fixture_count": 13,
  "fixtures": [
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
  "assertions": [
    "contentful-build-and-publish",
    "decoded-bundle-contract",
    "locked-plan-and-derivation-reuse",
    "second-contentful-build-reused",
    "stone-and-manifest-bytes-identical"
  ],
  "result": "passed"
}
EOF_PROOF
    printf 'temporary-proof-from-%s\n' "$FAKE_MAKE_MODE" \
        >"$CAST_FIXTURE_EVIDENCE_DIR/.fixtures-ci-proof.json.tmp"
    chmod 644 "$CAST_FIXTURE_EVIDENCE_DIR/fixtures-ci-proof.json"
}

case "$FAKE_MAKE_MODE" in
    success)
        printf 'BEGIN-SUCCESS\n'
        index=0
        while [ "$index" -lt 80 ]; do
            printf 'bounded-success-line-%03d-abcdefghijklmnopqrstuvwxyz\n' "$index"
            index=$((index + 1))
        done
        printf 'END-SUCCESS\n'
        emit_proof
        ;;
    success-public-late-proof)
        : "${FAKE_PUBLIC_EVIDENCE_DIR:?}"
        : "${FAKE_OUTER_STATE:?}"
        emit_proof
        setsid sh -c '
            trap "" HUP INT TERM
            sleep 2
            printf "%s\n" forged-after-success >"$1"
        ' public-proof-writer \
            "$FAKE_PUBLIC_EVIDENCE_DIR/fixtures-ci-proof.json" \
            </dev/null >/dev/null 2>&1 &
        printf '%s\n' "$!" >"$FAKE_OUTER_STATE/descendant-pid"
        printf 'SUCCESS-WITH-CGROUP-DESCENDANT\n'
        ;;
    success-no-proof)
        printf 'SUCCESS-WITHOUT-PROOF\n'
        ;;
    malformed-proof)
        printf '%s\n' '{"result":"passed"}' >"$CAST_FIXTURE_EVIDENCE_DIR/fixtures-ci-proof.json"
        chmod 644 "$CAST_FIXTURE_EVIDENCE_DIR/fixtures-ci-proof.json"
        printf 'MALFORMED-PROOF\n'
        ;;
    failure)
        printf 'BEGIN-FAILURE\n'
        index=0
        while [ "$index" -lt 80 ]; do
            printf 'bounded-failure-line-%03d-abcdefghijklmnopqrstuvwxyz\n' "$index"
            index=$((index + 1))
        done
        printf 'END-FAILURE\n'
        exit 37
        ;;
    failure-fifo-descendant)
        : "${FAKE_OUTER_STATE:?}"
        setsid sh -c '
            trap "" HUP INT TERM
            sleep 5
        ' fifo-holder &
        printf '%s\n' "$!" >"$FAKE_OUTER_STATE/descendant-pid"
        printf 'FAILURE-WITH-FIFO-DESCENDANT\n'
        exit 37
        ;;
    emit-then-fail)
        emit_proof
        printf 'EMIT-THEN-FAIL\n'
        exit 38
        ;;
    timeout)
        emit_proof
        printf 'BEGIN-TIMEOUT\n'
        sleep 5
        ;;
    ignore-term)
        emit_proof
        trap '' TERM
        printf 'BEGIN-IGNORE-TERM\n'
        while :; do
            sleep 1
        done
        ;;
    signal)
        emit_proof
        printf 'BEGIN-SIGNAL\n'
        wrapper_pid=${CAST_FIXTURE_WRAPPER_PID:?}
        kill -TERM "$wrapper_pid"
        exit 143
        ;;
    double-signal)
        emit_proof
        printf 'BEGIN-DOUBLE-SIGNAL\n'
        wrapper_pid=${CAST_FIXTURE_WRAPPER_PID:?}
        (
            trap '' TERM
            while kill -0 "$wrapper_pid" 2>/dev/null; do
                kill -TERM "$wrapper_pid" 2>/dev/null || exit 0
            done
        ) </dev/null >/dev/null 2>&1 &
        exit 143
        ;;
    signal-late-proof)
        trap '' HUP INT TERM
        : "${FAKE_LATE_PID_FILE:?}"
        setsid sh -c '
            trap "" HUP INT TERM
            sleep 2
            printf "%s\n" late-proof >"$1" 2>/dev/null || :
        ' late-proof-writer \
            "$CAST_FIXTURE_EVIDENCE_DIR/fixtures-ci-proof.json" \
            </dev/null >/dev/null 2>&1 &
        printf '%s\n' "$!" >"$FAKE_LATE_PID_FILE"
        printf '%s\n' "$!" >"$FAKE_OUTER_STATE/descendant-pid"
        printf 'BEGIN-SIGNAL-LATE-PROOF\n'
        wrapper_pid=${CAST_FIXTURE_WRAPPER_PID:?}
        kill -TERM "$wrapper_pid"
        while :; do
            sleep 1
        done
        ;;
    finalize-failure)
        emit_proof
        evidence_parent=$(dirname "$CAST_FIXTURE_EVIDENCE_DIR")
        rm -f "$evidence_parent/.fixtures-ci.log.tmp"
        mkdir "$evidence_parent/.fixtures-ci.log.tmp"
        printf 'FINALIZE-FAILURE\n'
        ;;
    *) exit 2 ;;
esac
EOF
chmod 755 "$fakebin/make"

cat >"$fakebin/git" <<'EOF'
#!/bin/sh
set -eu
: "${FAKE_GIT_COMMIT:?}"
: "${FAKE_OUTER_STATE:?}"
test "${1-}" = -C
shift 2
case "${1-}" in
    rev-parse)
        test "${2-}" = --verify
        test "${3-}" = HEAD
        printf '%s\n' "$FAKE_GIT_COMMIT"
        ;;
    status)
        test "${2-}" = --porcelain
        test "${3-}" = --untracked-files=normal
        case "${FAKE_GIT_STATUS_MODE-clean}" in
            clean) ;;
            dirty) printf '%s\n' ' M fixture-input' ;;
            fail) exit 71 ;;
            *) exit 2 ;;
        esac
        ;;
    *) exit 2 ;;
esac
EOF
chmod 755 "$fakebin/git"

cat >"$fakebin/jq" <<'EOF'
#!/bin/sh
set -eu
: "${FAKE_OUTER_STATE:?}"
: "${REAL_JQ:?}"
calls=0
if [ -f "$FAKE_OUTER_STATE/jq-calls" ]; then
    calls=$(cat "$FAKE_OUTER_STATE/jq-calls")
fi
calls=$((calls + 1))
printf '%s\n' "$calls" >"$FAKE_OUTER_STATE/jq-calls"
if [ -n "${FAKE_JQ_SIGNAL_CALL-}" ] \
    && [ "$calls" -eq "$FAKE_JQ_SIGNAL_CALL" ]; then
    kill -TERM "$PPID"
fi
exec "$REAL_JQ" "$@"
EOF
chmod 755 "$fakebin/jq"

cat >"$fakebin/systemd-run" <<'EOF'
#!/bin/sh
set -eu
: "${FAKE_OUTER_STATE:?}"

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

unit=
working_directory=
exit_type=
kill_mode=
runtime_max=
stop_timeout=
: >"$FAKE_OUTER_STATE/environment"
while [ "$#" -gt 0 ]; do
    case "$1" in
        --user|--wait|--pipe|--collect|--no-ask-password|--expand-environment=no|--service-type=exec)
            shift
            ;;
        --unit=*) unit=${1#--unit=}; shift ;;
        --working-directory=*) working_directory=${1#--working-directory=}; shift ;;
        --setenv=*)
            assignment=${1#--setenv=}
            export "$assignment"
            printf '%s\n' "$assignment" >>"$FAKE_OUTER_STATE/environment"
            shift
            ;;
        --property=ExitType=*) exit_type=${1#--property=ExitType=}; shift ;;
        --property=KillMode=*) kill_mode=${1#--property=KillMode=}; shift ;;
        --property=RuntimeMaxSec=*) runtime_max=${1#--property=RuntimeMaxSec=}; shift ;;
        --property=TimeoutStopSec=*) stop_timeout=${1#--property=TimeoutStopSec=}; shift ;;
        --property=SendSIGKILL=yes) shift ;;
        --) shift; break ;;
        *) printf 'unexpected fake systemd-run argument: %s\n' "$1" >&2; exit 2 ;;
    esac
done
test -n "$unit"
test -n "$working_directory"
test "$exit_type" = main
test "$kill_mode" = control-group
case "$runtime_max" in *s) runtime_seconds=${runtime_max%s} ;; *) exit 2 ;; esac
case "$runtime_seconds" in ''|0|*[!0-9]*) exit 2 ;; esac
case "$stop_timeout" in *s) stop_seconds=${stop_timeout%s} ;; *) exit 2 ;; esac
case "$stop_seconds" in ''|0|*[!0-9]*) exit 2 ;; esac
test "$#" -gt 0
printf '%s\n' "$unit" >"$FAKE_OUTER_STATE/unit"
: >"$FAKE_OUTER_STATE/active"
(
    cd "$working_directory"
    exec timeout --kill-after="$stop_timeout" "$runtime_max" "$@"
) &
command_pid=$!
printf '%s\n' "$command_pid" >"$FAKE_OUTER_STATE/command-pid"
set +e
wait "$command_pid"
status=$?
set -e

# Simulate systemd's main-process exit plus KillMode=control-group: a
# session-changing descendant remains in the service cgroup and must be gone
# before the client can report completion.
if [ -f "$FAKE_OUTER_STATE/descendant-pid" ]; then
    descendant=$(cat "$FAKE_OUTER_STATE/descendant-pid")
    kill -TERM "$descendant" 2>/dev/null || :
    attempts=0
    while process_is_live "$descendant"; do
        attempts=$((attempts + 1))
        if [ "$attempts" -gt 20 ]; then
            kill -KILL "$descendant" 2>/dev/null || :
        fi
        test "$attempts" -le 40 || exit 70
        sleep 0.05
    done
fi
rm -f "$FAKE_OUTER_STATE/active" "$FAKE_OUTER_STATE/command-pid"
exit "$status"
EOF
chmod 755 "$fakebin/systemd-run"

cat >"$fakebin/systemctl" <<'EOF'
#!/bin/sh
set -eu
: "${FAKE_OUTER_STATE:?}"

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

test "${1-}" = --user
shift
case "${1-}" in
    show-environment)
        exit 0
        ;;
    show)
        unit=${2-}
        property=${3-}
        test "${4-}" = --value
        case "$property" in
            --property=LoadState)
                if [ -n "${FAKE_LOAD_STATE_DELAY_SECONDS-}" ]; then
                    : >"$FAKE_OUTER_STATE/load-state-entered"
                    sleep "$FAKE_LOAD_STATE_DELAY_SECONDS"
                fi
                if [ -f "$FAKE_OUTER_STATE/active" ] \
                    && [ "$(cat "$FAKE_OUTER_STATE/unit")" = "$unit" ]; then
                    printf '%s\n' loaded
                else
                    printf '%s\n' not-found
                fi
                ;;
            --property=Environment)
                test -f "$FAKE_OUTER_STATE/active" || exit 1
                tr '\n' ' ' <"$FAKE_OUTER_STATE/environment"
                printf '\n'
                ;;
            *) exit 2 ;;
        esac
        ;;
    stop|kill)
        command=${1}
        shift
        while [ "$#" -gt 0 ]; do
            case "$1" in
                --kill-whom=*|--signal=*) shift ;;
                *) unit=$1; shift ;;
            esac
        done
        if [ -f "$FAKE_OUTER_STATE/unit" ]; then
            test "$(cat "$FAKE_OUTER_STATE/unit")" = "$unit"
        fi
        signal=TERM
        test "$command" = stop || signal=KILL
        if [ -f "$FAKE_OUTER_STATE/command-pid" ]; then
            kill -"$signal" "$(cat "$FAKE_OUTER_STATE/command-pid")" 2>/dev/null || :
        fi
        if [ -f "$FAKE_OUTER_STATE/descendant-pid" ]; then
            descendant=$(cat "$FAKE_OUTER_STATE/descendant-pid")
            kill -"$signal" "$descendant" 2>/dev/null || :
            if [ "$command" = stop ]; then
                attempts=0
                while process_is_live "$descendant"; do
                    attempts=$((attempts + 1))
                    if [ "$attempts" -gt 20 ]; then
                        kill -KILL "$descendant" 2>/dev/null || :
                    fi
                    test "$attempts" -le 40 || break
                    sleep 0.05
                done
                if process_is_live "$descendant"; then
                    printf 'fake systemctl could not drain live descendant %s\n' \
                        "$descendant" >&2
                    exit 70
                fi
            fi
        fi
        ;;
    *) exit 2 ;;
esac
EOF
chmod 755 "$fakebin/systemctl"

cat >"$fakebin/tee" <<'EOF'
#!/bin/sh
set -eu
: "${FAKE_TEE_MODE:?}"
: "${REAL_TEE:?}"
case "$FAKE_TEE_MODE" in
    pass) exec "$REAL_TEE" "$@" ;;
    fail)
        cat >/dev/null
        exit 75
        ;;
    *) exit 2 ;;
esac
EOF
chmod 755 "$fakebin/tee"

run_wrapper() {
    rm -f "$outer_state"/*
    env \
        PATH="$fakebin:$PATH" \
        MAKE="$fakebin/make" \
        FIXTURE_EVIDENCE_DIR="$evidence" \
        CAST_FIXTURE_LOG_MAX_BYTES=256 \
        CAST_FIXTURE_CI_TIMEOUT_SECONDS="$2" \
        CAST_FIXTURE_CI_KILL_AFTER_SECONDS=1 \
        FAKE_LATE_PID_FILE="$work/late-child.pid" \
        FAKE_GIT_COMMIT="$fake_commit" \
        FAKE_GIT_STATUS_MODE="${FAKE_GIT_STATUS_MODE-clean}" \
        FAKE_JQ_SIGNAL_CALL="${FAKE_JQ_SIGNAL_CALL-}" \
        FAKE_OUTER_STATE="$outer_state" \
        FAKE_PUBLIC_EVIDENCE_DIR="$evidence" \
        FAKE_TEE_MODE="${FAKE_TEE_MODE-pass}" \
        REAL_JQ="$real_jq" \
        REAL_TEE="$real_tee" \
        FAKE_MAKE_MODE="$1" \
        "$runner"
}

assert_bounded_inventory() {
    log="$evidence/fixtures-ci.log"
    test -f "$log"
    test ! -L "$log"
    test "$(stat -c '%a' "$log")" = 600
    test "$(stat -c '%s' "$log")" -le 256
    test -z "$(find "$evidence" -maxdepth 1 -name '.fixtures-ci.full.*' -print -quit)"
    test ! -e "$evidence/.fixtures-ci.log.tmp"
    test ! -e "$evidence/.fixtures-ci-proof.json.tmp"
    test ! -e "$evidence/.fixtures-ci.output.fifo"
    test -z "$(find "$evidence" -maxdepth 1 -name '.fixtures-ci-run.*' -print -quit)"
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

set +e
run_wrapper success 10 >"$work/success.out" 2>&1
status=$?
set -e
if [ "$status" -ne 0 ]; then
    cat "$work/success.out" >&2
    exit 1
fi
grep -Fq 'BEGIN-SUCCESS' "$work/success.out"
grep -Fq 'END-SUCCESS' "$work/success.out"
test "$(grep -c '^bounded-success-line-' "$work/success.out")" -eq 80
if ! tr -d '\000' <"$work/success.out" | cmp -s - "$work/success.out"; then
    printf '%s\n' 'redirected fixture output contains a NUL hole' >&2
    exit 1
fi
grep -Fq 'END-SUCCESS' "$evidence/fixtures-ci.log"
jq -e '.result == "passed"' "$evidence/fixtures-ci-proof.json" >/dev/null
assert_bounded_inventory

setup_attempt=1
while [ "$setup_attempt" -le 30 ]; do
    rm -f "$outer_state"/*
    env \
        PATH="$fakebin:$PATH" \
        MAKE="$fakebin/make" \
        FIXTURE_EVIDENCE_DIR="$evidence" \
        CAST_FIXTURE_LOG_MAX_BYTES=256 \
        CAST_FIXTURE_CI_TIMEOUT_SECONDS=10 \
        CAST_FIXTURE_CI_KILL_AFTER_SECONDS=1 \
        FAKE_LATE_PID_FILE="$work/late-child.pid" \
        FAKE_GIT_COMMIT="$fake_commit" \
        FAKE_GIT_STATUS_MODE=clean \
        FAKE_JQ_SIGNAL_CALL= \
        FAKE_LOAD_STATE_DELAY_SECONDS=0.05 \
        FAKE_OUTER_STATE="$outer_state" \
        FAKE_PUBLIC_EVIDENCE_DIR="$evidence" \
        FAKE_TEE_MODE=pass \
        REAL_JQ="$real_jq" \
        REAL_TEE="$real_tee" \
        FAKE_MAKE_MODE=success \
        "$runner" >"$work/setup-signal-$setup_attempt.out" 2>&1 &
    setup_wrapper_pid=$!
    setup_probe=1
    while [ ! -e "$outer_state/load-state-entered" ] \
        && [ "$setup_probe" -le 100 ]; do
        sleep 0.005
        setup_probe=$((setup_probe + 1))
    done
    test -e "$outer_state/load-state-entered"
    kill -TERM "$setup_wrapper_pid"
    set +e
    wait "$setup_wrapper_pid"
    status=$?
    set -e
    test "$status" -eq 143
    test ! -e "$evidence/fixtures-ci-proof.json"
    assert_bounded_inventory
    setup_attempt=$((setup_attempt + 1))
done

set +e
run_wrapper success-public-late-proof 10 >"$work/success-public-late-proof.out" 2>&1
status=$?
set -e
if [ "$status" -ne 0 ]; then
    cat "$work/success-public-late-proof.out" >&2
    exit 1
fi
public_late_pid=$(cat "$outer_state/descendant-pid")
sleep 3
jq -e '.result == "passed"' "$evidence/fixtures-ci-proof.json" >/dev/null
if grep -Fq 'forged-after-success' "$evidence/fixtures-ci-proof.json"; then
    printf '%s\n' 'successful fixture proof was replaced after promotion' >&2
    exit 1
fi
if process_is_live "$public_late_pid"; then
    printf 'successful service descendant escaped its control group: %s\n' \
        "$public_late_pid" >&2
    exit 1
fi
assert_bounded_inventory

for validation_call in 1 2; do
    set +e
    FAKE_JQ_SIGNAL_CALL="$validation_call" run_wrapper success 10 \
        >"$work/validation-signal-$validation_call.out" 2>&1
    status=$?
    set -e
    test "$status" -eq 143
    test ! -e "$evidence/fixtures-ci-proof.json"
    assert_bounded_inventory
done

set +e
FAKE_GIT_STATUS_MODE=dirty run_wrapper success 10 \
    >"$work/dirty-checkout.out" 2>&1
status=$?
set -e
test "$status" -eq 1
grep -Fq 'fixture CI proof refuses a checkout which differs from commit' \
    "$work/dirty-checkout.out"
test ! -e "$evidence/fixtures-ci-proof.json"
assert_bounded_inventory

set +e
run_wrapper success-no-proof 10 >"$work/success-no-proof.out" 2>&1
status=$?
set -e
test "$status" -eq 1
grep -Fq 'successful fixture CI did not publish one regular proof' "$work/success-no-proof.out"
test ! -e "$evidence/fixtures-ci-proof.json"
grep -Fq 'SUCCESS-WITHOUT-PROOF' "$evidence/fixtures-ci.log"
assert_bounded_inventory

failure_started=$(date +%s)
set +e
run_wrapper failure-fifo-descendant 10 >"$work/failure-fifo-descendant.out" 2>&1
status=$?
set -e
failure_elapsed=$(($(date +%s) - failure_started))
test "$status" -eq 37
test "$failure_elapsed" -lt 4
fifo_descendant=$(cat "$outer_state/descendant-pid")
if process_is_live "$fifo_descendant"; then
    printf 'failed service FIFO descendant escaped its control group: %s\n' \
        "$fifo_descendant" >&2
    exit 1
fi
test ! -e "$evidence/fixtures-ci-proof.json"
assert_bounded_inventory

set +e
run_wrapper malformed-proof 10 >"$work/malformed-proof.out" 2>&1
status=$?
set -e
test "$status" -eq 1
grep -Fq 'does not exactly match' "$work/malformed-proof.out"
test ! -e "$evidence/fixtures-ci-proof.json"
grep -Fq 'MALFORMED-PROOF' "$evidence/fixtures-ci.log"
assert_bounded_inventory

printf 'stale-proof\n' >"$evidence/fixtures-ci-proof.json"
set +e
run_wrapper failure 10 >"$work/failure.out" 2>&1
status=$?
set -e
test "$status" -eq 37
test ! -e "$evidence/fixtures-ci-proof.json"
grep -Fq 'END-FAILURE' "$evidence/fixtures-ci.log"
assert_bounded_inventory

set +e
run_wrapper emit-then-fail 10 >"$work/emit-then-fail.out" 2>&1
status=$?
set -e
test "$status" -eq 38
test ! -e "$evidence/fixtures-ci-proof.json"
test ! -e "$evidence/.fixtures-ci-proof.json.tmp"
grep -Fq 'EMIT-THEN-FAIL' "$evidence/fixtures-ci.log"
assert_bounded_inventory

set +e
run_wrapper timeout 1 >"$work/timeout.out" 2>&1
status=$?
set -e
test "$status" -eq 124
test ! -e "$evidence/fixtures-ci-proof.json"
grep -Fq 'BEGIN-TIMEOUT' "$evidence/fixtures-ci.log"
assert_bounded_inventory

set +e
run_wrapper ignore-term 1 >"$work/ignore-term.out" 2>&1
status=$?
set -e
test "$status" -eq 137
test ! -e "$evidence/fixtures-ci-proof.json"
test ! -e "$evidence/.fixtures-ci-proof.json.tmp"
grep -Fq 'BEGIN-IGNORE-TERM' "$evidence/fixtures-ci.log"
assert_bounded_inventory

set +e
run_wrapper signal 10 >"$work/signal.out" 2>&1
status=$?
set -e
test "$status" -eq 143
test ! -e "$evidence/fixtures-ci-proof.json"
test ! -e "$evidence/.fixtures-ci-proof.json.tmp"
grep -Fq 'BEGIN-SIGNAL' "$evidence/fixtures-ci.log"
assert_bounded_inventory

set +e
run_wrapper double-signal 10 >"$work/double-signal.out" 2>&1
status=$?
set -e
test "$status" -eq 143
test ! -e "$evidence/fixtures-ci-proof.json"
test ! -e "$evidence/.fixtures-ci-proof.json.tmp"
grep -Fq 'BEGIN-DOUBLE-SIGNAL' "$evidence/fixtures-ci.log"
assert_bounded_inventory

rm -f "$work/late-child.pid"
set +e
run_wrapper signal-late-proof 10 >"$work/signal-late-proof.out" 2>&1
status=$?
set -e
test "$status" -eq 143
test -f "$work/late-child.pid"
late_child_pid=$(cat "$work/late-child.pid")
sleep 3
test ! -e "$evidence/fixtures-ci-proof.json"
test ! -e "$evidence/.fixtures-ci-proof.json.tmp"
if process_is_live "$late_child_pid"; then
    printf 'detached late-proof writer outlived its bounded test window: %s\n' \
        "$late_child_pid" >&2
    exit 1
fi
grep -Fq 'BEGIN-SIGNAL-LATE-PROOF' "$evidence/fixtures-ci.log"
assert_bounded_inventory

set +e
FAKE_TEE_MODE=fail run_wrapper success 10 >"$work/tee-failure.out" 2>&1
status=$?
set -e
test "$status" -eq 75
test ! -e "$evidence/fixtures-ci-proof.json"
assert_bounded_inventory

set +e
run_wrapper finalize-failure 10 >"$work/finalize-failure.out" 2>&1
status=$?
set -e
test "$status" -eq 1
test ! -e "$evidence/fixtures-ci-proof.json"
test ! -e "$evidence/.fixtures-ci-proof.json.tmp"
test -d "$evidence/.fixtures-ci.log.tmp"
rm -rf "$evidence/.fixtures-ci.log.tmp"
test ! -e "$evidence/fixtures-ci.log"

hostile_marker="$work/make-expansion-ran"
hostile_evidence="$work/\$(shell touch $hostile_marker)"
rm -f "$outer_state"/*
env \
    PATH="$fakebin:$PATH" \
    MAKE="$fakebin/make" \
    FIXTURE_EVIDENCE_DIR="$hostile_evidence" \
    CAST_FIXTURE_LOG_MAX_BYTES=256 \
    CAST_FIXTURE_CI_TIMEOUT_SECONDS=10 \
    CAST_FIXTURE_CI_KILL_AFTER_SECONDS=1 \
    FAKE_LATE_PID_FILE="$work/late-child.pid" \
    FAKE_GIT_COMMIT="$fake_commit" \
    FAKE_GIT_STATUS_MODE=clean \
    FAKE_JQ_SIGNAL_CALL= \
    FAKE_OUTER_STATE="$outer_state" \
    FAKE_TEE_MODE=pass \
    REAL_JQ="$real_jq" \
    REAL_TEE="$real_tee" \
    FAKE_MAKE_MODE=success \
    "$runner" >"$work/hostile-evidence.out" 2>&1
test ! -e "$hostile_marker"
jq -e '.result == "passed"' "$hostile_evidence/fixtures-ci-proof.json" >/dev/null
rm -rf "$hostile_evidence"

set +e
env \
    PATH="$fakebin:$PATH" \
    MAKE="$fakebin/make" \
    FIXTURE_EVIDENCE_DIR="$evidence" \
    CAST_FIXTURE_LOG_MAX_BYTES=1048577 \
    CAST_FIXTURE_CI_TIMEOUT_SECONDS=10 \
    FAKE_MAKE_MODE=success \
    "$runner" >"$work/oversized.out" 2>&1
status=$?
set -e
test "$status" -eq 2
grep -Fq 'must be between 1 and 1048576' "$work/oversized.out"

printf '%s\n' 'bounded fixture CI evidence tests passed'
