#!/bin/sh

set -eu

root=$(CDPATH= cd -- "$(dirname -- "$0")/../.." && pwd -P)
work=$(mktemp -d "${TMPDIR:-/tmp}/cast-examples-gate-test.XXXXXXXXXXXX")
cleanup() {
    rm -rf "$work"
}
trap cleanup EXIT HUP INT TERM

fake_cargo="$work/cargo"
calls="$work/calls"

cat >"$fake_cargo" <<'EOF'
#!/bin/sh
set -eu

: "${FAKE_CARGO_MODE:?}"
: "${FAKE_CARGO_CALLS:?}"
printf '%s\n' "$*" >>"$FAKE_CARGO_CALLS"

listing=0
for argument in "$@"; do
    if test "$argument" = --list; then
        listing=1
    fi
done

if test "$listing" -eq 1; then
    case "$FAKE_CARGO_MODE" in
        list-failure)
            echo "fake cargo list failure" >&2
            exit 17
            ;;
        missing-test)
            printf '%s\n' 'unrelated_test: test'
            exit 0
            ;;
        success | execution-failure)
            cat <<'TESTS'
every_gluon_package_example_passes_the_public_cast_cli: test
process_supervision::bounded_cast_child_supervisor_drop_kills_and_reaps_group: test
process_supervision::bounded_cast_child_supervisor_escalates_ignored_term_to_kill: test
process_supervision::bounded_cast_child_supervisor_kills_and_reaps_descendant_tree: test
process_supervision::bounded_cast_child_supervisor_rejects_exited_leader_with_descendant: test
process_supervision::bounded_cast_child_supervisor_rejects_stdout_overflow_and_reaps_group: test
process_supervision::bounded_cast_child_supervisor_reuses_one_cleanup_deadline: test
process_supervision::bounded_cast_child_supervisor_times_out_and_reaps_group: test
process_supervision::cast_child_supervisor_helper: test
planner::hermetic_tests::checked_in_package_examples_freeze_hermetically_and_reuse_exact_build_locks: test
planner::hermetic_tests::checked_in_metadata_only_example_fails_closed_before_execution: test
TESTS
            exit 0
            ;;
        *) exit 2 ;;
    esac
fi

case "$FAKE_CARGO_MODE" in
    execution-failure)
        echo "fake cargo execution failure" >&2
        exit 19
        ;;
    success) exit 0 ;;
    *) exit 2 ;;
esac
EOF
chmod 755 "$fake_cargo"

expect_failure() {
    mode=$1
    expected=$2
    output="$work/$mode.log"
    : >"$calls"
    if timeout 30s env \
        FAKE_CARGO_MODE="$mode" \
        FAKE_CARGO_CALLS="$calls" \
        make --no-print-directory -C "$root" CARGO="$fake_cargo" examples >"$output" 2>&1; then
        echo "FAIL: examples accepted fake cargo mode $mode" >&2
        cat "$output" >&2
        exit 1
    fi
    if ! grep -F -- "$expected" "$output" >/dev/null; then
        echo "FAIL: examples did not preserve failure '$expected' in mode $mode" >&2
        cat "$output" >&2
        exit 1
    fi
}

expect_failure list-failure "fake cargo list failure"
expect_failure missing-test "Error"
expect_failure execution-failure "fake cargo execution failure"

: >"$calls"
timeout 30s env \
    FAKE_CARGO_MODE=success \
    FAKE_CARGO_CALLS="$calls" \
    make --no-print-directory -C "$root" CARGO="$fake_cargo" examples >"$work/success.log" 2>&1

call_count=$(wc -l <"$calls")
if test "$call_count" -ne 7; then
    echo "FAIL: successful examples gate made $call_count fake cargo calls instead of 7" >&2
    cat "$calls" >&2
    exit 1
fi

echo "examples gate failure-propagation tests passed"
