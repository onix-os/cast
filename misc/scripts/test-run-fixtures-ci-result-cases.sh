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

current_case=failure-fifo-descendant
fifo_descendant_gate="$gates/failure-fifo-descendant"
rm -rf "$fifo_descendant_gate.ready" "$fifo_descendant_gate.release" \
    "$fifo_descendant_gate.drain-started" "$fifo_descendant_gate.drained" \
    "$fifo_descendant_gate.natural-exit"
mkfifo -m 600 "$fifo_descendant_gate.release"
set +e
FAKE_DESCENDANT_GATE="$fifo_descendant_gate" \
    run_wrapper failure-fifo-descendant 10 \
    >"$work/failure-fifo-descendant.out" 2>&1
status=$?
set -e
test "$status" -eq 37
fifo_descendant=$(cat "$outer_state/descendant-pid")
require_receipt "$fifo_descendant_gate.ready" 'FIFO descendant readiness'
require_receipt "$fifo_descendant_gate.drain-started" \
    'FIFO descendant drain start'
require_receipt "$fifo_descendant_gate.drained" \
    'FIFO descendant drain completion'
test ! -e "$fifo_descendant_gate.natural-exit"
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
