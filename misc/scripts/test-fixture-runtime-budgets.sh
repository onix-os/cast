#!/bin/sh

set -eu

root=$(CDPATH= cd -- "$(dirname -- "$0")/../.." && pwd -P)
budgets="$root/misc/scripts/fixture-runtime-budgets.sh"
delegated_runner="$root/misc/scripts/run-delegated-execution-fixture.sh"
outer_runner="$root/misc/scripts/run-fixtures-ci-with-evidence.sh"
work=$(mktemp -d "${TMPDIR:-/tmp}/cast-fixture-runtime-budget-test.XXXXXXXXXXXX")
cleanup() {
    status=$?
    trap - EXIT HUP INT TERM
    rm -rf "$work"
    exit "$status"
}
trap cleanup EXIT
trap 'exit 129' HUP
trap 'exit 130' INT
trap 'exit 143' TERM

for source in "$budgets" "$delegated_runner" "$outer_runner"; do
    if [ -L "$source" ] || [ ! -f "$source" ] || [ ! -r "$source" ]; then
        printf 'fixture runtime source is unavailable or unsafe: %s\n' "$source" >&2
        exit 1
    fi
done
. "$budgets"
command -v timeout >/dev/null 2>&1 || {
    printf 'timeout is required for fixture runtime budget tests\n' >&2
    exit 1
}

test "$fixture_budget_seconds_per_hour" -eq 3600
test "$fixture_budget_preflight_runtime_seconds" -eq 30
test "$fixture_budget_single_runtime_seconds" -eq 7200
test "$fixture_budget_all_default_runtime_seconds" -eq 14400
test "$fixture_budget_all_max_runtime_seconds" -eq 18000
test "$fixture_budget_outer_minimum_headroom_seconds" -eq 3600
test "$fixture_budget_ci_default_runtime_seconds" -eq 21600
test "$fixture_budget_ci_max_runtime_seconds" -eq 21600

outer_headroom=$((
    fixture_budget_ci_default_runtime_seconds - fixture_budget_all_max_runtime_seconds
))
test "$outer_headroom" -ge "$fixture_budget_outer_minimum_headroom_seconds"
test "$fixture_budget_all_default_runtime_seconds" \
    -gt "$fixture_budget_single_runtime_seconds"

default_kill_after_seconds=30
default_client_timeout=$((
    fixture_budget_all_default_runtime_seconds
        + default_kill_after_seconds
        + fixture_budget_delegated_client_completion_margin_seconds
))
default_status_timeout=$((
    default_client_timeout
        + default_kill_after_seconds
        + fixture_budget_status_delivery_margin_seconds
))
test "$default_client_timeout" -eq 14490
test "$default_status_timeout" -eq 14525

maximum_client_timeout=$((
    fixture_budget_all_max_runtime_seconds
        + fixture_budget_max_kill_after_seconds
        + fixture_budget_delegated_client_completion_margin_seconds
))
maximum_status_timeout=$((
    maximum_client_timeout
        + fixture_budget_max_kill_after_seconds
        + fixture_budget_status_delivery_margin_seconds
))
test "$maximum_client_timeout" -eq 18360
test "$maximum_status_timeout" -eq 18665
test "$fixture_budget_delegated_max_status_timeout_seconds" \
    -eq "$maximum_status_timeout"
test "$fixture_budget_ci_max_status_timeout_seconds" -eq 22215

runtime_case=0
for invalid_runtime in 0 invalid 01 18001; do
    runtime_case=$((runtime_case + 1))
    case "$invalid_runtime" in
        invalid) expected_runtime_error='must be decimal' ;;
        *) expected_runtime_error='must be between 1 and 18000' ;;
    esac
    set +e
    timeout --kill-after=1s 10s env \
        CAST_DELEGATED_RUNTIME_MAX_SECONDS="$invalid_runtime" \
        "$delegated_runner" all \
        >"$work/delegated-$runtime_case.out" \
        2>"$work/delegated-$runtime_case.err"
    delegated_status=$?
    set -e
    test "$delegated_status" -eq 2
    grep -Fq "CAST_DELEGATED_RUNTIME_MAX_SECONDS $expected_runtime_error" \
        "$work/delegated-$runtime_case.err"

    set +e
    timeout --kill-after=1s 10s env \
        FIXTURE_EVIDENCE_DIR="$work/evidence" \
        CAST_DELEGATED_RUNTIME_MAX_SECONDS="$invalid_runtime" \
        "$outer_runner" >"$work/outer-$runtime_case.out" \
        2>"$work/outer-$runtime_case.err"
    outer_status=$?
    set -e
    test "$outer_status" -eq 2
    grep -Fq "CAST_DELEGATED_RUNTIME_MAX_SECONDS $expected_runtime_error" \
        "$work/outer-$runtime_case.err"
done

status_case=0
expect_selected_status_bound() {
    selector=$1
    runtime=$2
    kill_after=$3
    expected_bound=$4
    status_case=$((status_case + 1))
    set +e
    timeout --kill-after=1s 10s env \
        CAST_REQUIRE_EXECUTION=1 \
        CAST_DELEGATED_RUNTIME_MAX_SECONDS="$runtime" \
        CAST_DELEGATED_KILL_AFTER_SECONDS="$kill_after" \
        CAST_DELEGATED_STATUS_TIMEOUT_SECONDS="$((expected_bound + 1))" \
        "$delegated_runner" "$selector" \
        >"$work/status-$status_case.out" 2>"$work/status-$status_case.err"
    selected_status=$?
    set -e
    test "$selected_status" -eq 2
    grep -Fq "CAST_DELEGATED_STATUS_TIMEOUT_SECONDS must be between 1 and $expected_bound" \
        "$work/status-$status_case.err"
}
expect_selected_status_bound --preflight-only '' '' 50
expect_selected_status_bound custom 18000 '' 7325
expect_selected_status_bound all '' '' 14525
expect_selected_status_bound all 18000 300 18665

set +e
timeout --kill-after=1s 10s env \
    CAST_REQUIRE_EXECUTION=invalid \
    CAST_DELEGATED_STATUS_TIMEOUT_SECONDS=1 \
    "$delegated_runner" all >"$work/tightened.out" 2>"$work/tightened.err"
tightened_status=$?
set -e
test "$tightened_status" -eq 2
grep -Fq 'CAST_REQUIRE_EXECUTION must be set to exactly 0 or 1' \
    "$work/tightened.err"

set +e
timeout --kill-after=1s 10s env \
    CAST_REQUIRE_EXECUTION=invalid \
    CAST_DELEGATED_RUNTIME_MAX_SECONDS=18000 \
    CAST_DELEGATED_KILL_AFTER_SECONDS=300 \
    CAST_DELEGATED_STATUS_TIMEOUT_SECONDS=18665 \
    "$delegated_runner" all >"$work/maximum-status.out" \
    2>"$work/maximum-status.err"
maximum_status=$?
set -e
test "$maximum_status" -eq 2
grep -Fq 'CAST_REQUIRE_EXECUTION must be set to exactly 0 or 1' \
    "$work/maximum-status.err"

set +e
timeout --kill-after=1s 10s env \
    FIXTURE_EVIDENCE_DIR="$work/evidence" \
    CAST_DELEGATED_RUNTIME_MAX_SECONDS= \
    CAST_FIXTURE_CI_KILL_AFTER_SECONDS=0 \
    "$outer_runner" >"$work/outer-empty.out" 2>"$work/outer-empty.err"
outer_empty_status=$?
set -e
test "$outer_empty_status" -eq 2
grep -Fq 'CAST_FIXTURE_CI_KILL_AFTER_SECONDS must be between 1 and 300' \
    "$work/outer-empty.err"

grep -Fq 'service_runtime_seconds + client_kill_after_seconds + client_completion_margin_seconds' \
    "$delegated_runner"
grep -Fq 'client_timeout_seconds + kill_after_seconds + fixture_budget_status_delivery_margin_seconds' \
    "$outer_runner"
grep -Fq 'CAST_DELEGATED_RUNTIME_MAX_SECONDS=$delegated_runtime_seconds' \
    "$outer_runner"
grep -Fq 'service_runtime_seconds=${CAST_DELEGATED_RUNTIME_MAX_SECONDS:-$fixture_budget_all_default_runtime_seconds}' \
    "$delegated_runner"
grep -Fq 'delegated_runtime_seconds=${CAST_DELEGATED_RUNTIME_MAX_SECONDS:-$fixture_budget_all_default_runtime_seconds}' \
    "$outer_runner"

printf '%s\n' 'fixture runtime budget tests passed'
