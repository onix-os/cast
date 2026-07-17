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
    FAKE_PROOF_GENERATOR="$proof_generator" \
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

# Individually valid maxima must compose into the default status-channel bound
# rather than rejecting a production configuration at its documented edge.
CAST_FIXTURE_CI_KILL_AFTER_SECONDS=300 \
    run_wrapper success 21600 >"$work/maximum-bounds.out" 2>&1
jq -e '.result == "passed"' "$evidence/fixtures-ci-proof.json" >/dev/null
assert_bounded_inventory
