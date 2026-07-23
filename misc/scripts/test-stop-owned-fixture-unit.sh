#!/bin/sh

set -eu

root=$(CDPATH= cd -- "$(dirname -- "$0")/../.." && pwd -P)
stopper="$root/misc/scripts/stop-owned-fixture-unit.sh"
work=$(mktemp -d "${TMPDIR:-/tmp}/cast-owned-unit-stopper-test.XXXXXXXXXXXX")
cleanup() {
    cleanup_status=$?
    trap - EXIT HUP INT TERM
    rm -rf "$work"
    exit "$cleanup_status"
}
trap cleanup EXIT
trap 'exit 129' HUP
trap 'exit 130' INT
trap 'exit 143' TERM

fail() {
    printf 'owned fixture unit stopper test failed: %s\n' "$1" >&2
    exit 1
}

fakebin="$work/bin"
state="$work/state"
mkdir -m 700 "$fakebin" "$state"

cat >"$fakebin/systemctl" <<'EOF'
#!/bin/sh
set -eu

: "${FAKE_STOPPER_MODE:?}"
: "${FAKE_STOPPER_STATE:?}"
: "${FAKE_EXPECTED_UNIT:?}"
: "${FAKE_EXPECTED_MARKER:?}"

printf '%s\n' "$*" >>"$FAKE_STOPPER_STATE/invocations"
test "${1-}" = --user
shift

increment_count() {
    count_file=$1
    count=0
    if test -f "$count_file"; then
        IFS= read -r count <"$count_file"
    fi
    count=$((count + 1))
    printf '%s\n' "$count" >"$count_file"
}

case "${1-}" in
    show)
        test "${2-}" = "$FAKE_EXPECTED_UNIT"
        property=${3-}
        test "${4-}" = --value
        case "$property" in
            --property=LoadState)
                increment_count "$FAKE_STOPPER_STATE/load-calls"
                load_call=$count
                if test "$FAKE_STOPPER_MODE" = initial-load-fail \
                    && test "$load_call" -eq 1; then
                    exit 61
                fi
                if test "$FAKE_STOPPER_MODE" = final-load-fail \
                    && test "$load_call" -eq 2; then
                    exit 62
                fi
                case "$FAKE_STOPPER_MODE:$load_call" in
                    not-found:*) printf '%s\n' not-found ;;
                    normal-gone:2) printf '%s\n' not-found ;;
                    *) printf '%s\n' loaded ;;
                esac
                ;;
            --property=Environment)
                increment_count "$FAKE_STOPPER_STATE/environment-calls"
                case "$FAKE_STOPPER_MODE" in
                    environment-fail) exit 63 ;;
                    foreign-environment)
                        printf '%s\n' 'CAST_FIXTURE_CI_UNIT_TOKEN=foreign'
                        ;;
                    *)
                        printf 'NOISE=1 %s OTHER=2\n' "$FAKE_EXPECTED_MARKER"
                        ;;
                esac
                ;;
            --property=ActiveState)
                increment_count "$FAKE_STOPPER_STATE/active-calls"
                case "$FAKE_STOPPER_MODE" in
                    active-fail) exit 64 ;;
                    normal-inactive|forced-inactive) printf '%s\n' inactive ;;
                    kill-fail-active|second-stop-fail-active) printf '%s\n' active ;;
                    *) printf '%s\n' active ;;
                esac
                ;;
            *) exit 2 ;;
        esac
        ;;
    stop)
        test "${2-}" = "$FAKE_EXPECTED_UNIT"
        test "$#" -eq 2
        increment_count "$FAKE_STOPPER_STATE/stop-calls"
        stop_call=$count
        case "$FAKE_STOPPER_MODE:$stop_call" in
            forced-inactive:1|kill-fail-active:1|second-stop-fail-active:1)
                exit 70
                ;;
            second-stop-fail-active:2) exit 72 ;;
            *) exit 0 ;;
        esac
        ;;
    kill)
        test "${2-}" = --kill-whom=all
        test "${3-}" = --signal=SIGKILL
        test "${4-}" = "$FAKE_EXPECTED_UNIT"
        test "$#" -eq 4
        increment_count "$FAKE_STOPPER_STATE/kill-calls"
        test "$FAKE_STOPPER_MODE" != kill-fail-active || exit 71
        ;;
    *) exit 2 ;;
esac
EOF
chmod 755 "$fakebin/systemctl"

caller_uid=$(id -u)
fixture_unit="cast-fixtures-ci-$caller_uid-123-Token9.service"
fixture_marker=CAST_FIXTURE_CI_UNIT_TOKEN=Token9
delegated_unit="cast-delegated-fixture-$caller_uid-456-AbC123.service"
delegated_marker=CAST_DELEGATED_FIXTURE_TOKEN=AbC123
case_status=
case_output=
case_error=

reset_state() {
    rm -f "$state"/*
}

run_case() {
    case_name=$1
    mode=$2
    unit=$3
    marker=$4
    bound=$5
    case_output="$work/$case_name.out"
    case_error="$work/$case_name.err"
    reset_state
    set +e
    timeout --kill-after=1s 15s env \
        PATH="$fakebin:$PATH" \
        FAKE_STOPPER_MODE="$mode" \
        FAKE_STOPPER_STATE="$state" \
        FAKE_EXPECTED_UNIT="$unit" \
        FAKE_EXPECTED_MARKER="$marker" \
        "$stopper" "$unit" "$marker" "$bound" \
        >"$case_output" 2>"$case_error"
    case_status=$?
    set -e
}

assert_status() {
    expected_status=$1
    [ "$case_status" -eq "$expected_status" ] || {
        cat "$case_error" >&2
        fail "status $case_status, expected $expected_status"
    }
}

assert_error() {
    expected_error=$1
    grep -Fq "$expected_error" "$case_error" || {
        cat "$case_error" >&2
        fail "missing error: $expected_error"
    }
}

assert_count() {
    count_file=$1
    expected_count=$2
    actual_count=0
    if [ -f "$state/$count_file" ]; then
        IFS= read -r actual_count <"$state/$count_file"
    fi
    [ "$actual_count" -eq "$expected_count" ] \
        || fail "$count_file count $actual_count, expected $expected_count"
}

expect_preflight_rejection() {
    rejection_name=$1
    unit=$2
    marker=$3
    bound=$4
    expected_status=$5
    expected_error=$6
    run_case "$rejection_name" not-found "$unit" "$marker" "$bound"
    assert_status "$expected_status"
    assert_error "$expected_error"
    [ ! -e "$state/invocations" ] \
        || fail "$rejection_name reached systemctl before rejection"
}

# Both supported unit families accept only their coupled marker family/token.
run_case valid-fixtures-family not-found "$fixture_unit" "$fixture_marker" 1
assert_status 0
assert_count load-calls 1
assert_count environment-calls 0
assert_count stop-calls 0

run_case valid-delegated-family not-found "$delegated_unit" "$delegated_marker" 300
assert_status 0
assert_count load-calls 1
assert_count environment-calls 0
assert_count stop-calls 0

wrong_uid=$((caller_uid + 1))
expect_preflight_rejection wrong-family \
    "cast-fixture-ci-$caller_uid-123-Token9.service" "$fixture_marker" 1 1 \
    'refusing non-fixture unit'
expect_preflight_rejection wrong-uid \
    "cast-fixtures-ci-$wrong_uid-123-Token9.service" "$fixture_marker" 1 1 \
    'UID does not match the caller'
expect_preflight_rejection missing-pid \
    "cast-fixtures-ci-$caller_uid--Token9.service" "$fixture_marker" 1 1 \
    'PID is invalid'
expect_preflight_rejection invalid-pid \
    "cast-fixtures-ci-$caller_uid-pid-Token9.service" "$fixture_marker" 1 1 \
    'PID is invalid'
expect_preflight_rejection missing-token \
    "cast-fixtures-ci-$caller_uid-123-.service" \
    'CAST_FIXTURE_CI_UNIT_TOKEN=' 1 1 'token is invalid'
expect_preflight_rejection invalid-token \
    "cast-fixtures-ci-$caller_uid-123-bad-token.service" \
    'CAST_FIXTURE_CI_UNIT_TOKEN=bad-token' 1 1 'token is invalid'
expect_preflight_rejection missing-service-suffix \
    "cast-fixtures-ci-$caller_uid-123-Token9" "$fixture_marker" 1 1 \
    'lacks the exact service suffix'
expect_preflight_rejection trailing-service-data \
    "cast-fixtures-ci-$caller_uid-123-Token9.service.extra" "$fixture_marker" 1 1 \
    'lacks the exact service suffix'
expect_preflight_rejection wrong-marker-family \
    "$fixture_unit" 'CAST_DELEGATED_FIXTURE_TOKEN=Token9' 1 1 \
    'marker does not match its family and token'
expect_preflight_rejection wrong-marker-token \
    "$fixture_unit" 'CAST_FIXTURE_CI_UNIT_TOKEN=Other' 1 1 \
    'marker does not match its family and token'

# Bounds are canonical decimal 1..300: leading zeroes, oversized strings, and
# out-of-range values are rejected before any subprocess can be reached.
expect_preflight_rejection nondecimal-bound "$fixture_unit" "$fixture_marker" 1s 2 \
    'stop bound must be decimal'
for invalid_bound in 0 00 01 0300 301 1000 999999999999999999999999999999; do
    expect_preflight_rejection "invalid-bound-$invalid_bound" \
        "$fixture_unit" "$fixture_marker" "$invalid_bound" 2 \
        'stop bound must be between 1 and 300 seconds'
done

# Every ownership query fails closed, including the two post-stop queries.
run_case initial-load-query-failure initial-load-fail \
    "$fixture_unit" "$fixture_marker" 1
assert_status 1
assert_error 'could not verify owned fixture unit load state'
assert_count load-calls 1
assert_count stop-calls 0

run_case environment-query-failure environment-fail \
    "$fixture_unit" "$fixture_marker" 1
assert_status 1
assert_error 'could not verify owned fixture unit environment'
assert_count load-calls 1
assert_count environment-calls 1
assert_count stop-calls 0

run_case foreign-environment foreign-environment \
    "$fixture_unit" "$fixture_marker" 1
assert_status 1
assert_error 'refusing to stop fixture unit without this invocation marker'
assert_count stop-calls 0

run_case final-load-query-failure final-load-fail \
    "$fixture_unit" "$fixture_marker" 1
assert_status 1
assert_error 'could not verify final owned fixture unit load state'
assert_count stop-calls 1
assert_count load-calls 2

run_case active-query-failure active-fail \
    "$fixture_unit" "$fixture_marker" 1
assert_status 1
assert_error 'could not verify final owned fixture unit active state'
assert_count stop-calls 1
assert_count active-calls 1

# Normal stop succeeds when the unit disappears or remains loaded but inactive.
run_case normal-gone normal-gone "$fixture_unit" "$fixture_marker" 1
assert_status 0
assert_count stop-calls 1
assert_count kill-calls 0
assert_count load-calls 2
assert_count active-calls 0

run_case normal-inactive normal-inactive "$fixture_unit" "$fixture_marker" 1
assert_status 0
assert_count stop-calls 1
assert_count kill-calls 0
assert_count active-calls 1

# A failed normal stop escalates once, retries stop, and accepts only a final
# inactive state.
run_case forced-inactive forced-inactive "$fixture_unit" "$fixture_marker" 1
assert_status 0
assert_error 'normal stop failed for owned fixture unit'
assert_count stop-calls 2
assert_count kill-calls 1
assert_count active-calls 1

# Intermediate escalation errors remain visible, and a final active unit makes
# the whole operation fail closed rather than laundering those errors.
run_case force-kill-failure kill-fail-active "$fixture_unit" "$fixture_marker" 1
assert_status 1
assert_error 'forced kill failed for owned fixture unit'
assert_error 'owned fixture unit remained active after bounded cleanup'
assert_count stop-calls 2
assert_count kill-calls 1

run_case second-stop-failure second-stop-fail-active \
    "$fixture_unit" "$fixture_marker" 1
assert_status 1
assert_error 'post-kill stop failed for owned fixture unit'
assert_error 'owned fixture unit remained active after bounded cleanup'
assert_count stop-calls 2
assert_count kill-calls 1

printf '%s\n' 'owned fixture unit stopper tests passed'
