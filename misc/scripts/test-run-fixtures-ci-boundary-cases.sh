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

locale_archive_directory="$work/locale archives=valid"
valid_locale_archive="$locale_archive_directory/locale-archive"
locale_archive_symlink="$locale_archive_directory/locale-archive-link"
mkdir -p "$locale_archive_directory"
printf '%s\n' 'test locale archive' >"$valid_locale_archive"
chmod 444 "$valid_locale_archive"
ln -s "$valid_locale_archive" "$locale_archive_symlink"

LOCALE_ARCHIVE="$valid_locale_archive" \
FAKE_REQUIRE_LOCALE_ARCHIVE=1 \
FAKE_EXPECTED_LOCALE_ARCHIVE="$valid_locale_archive" \
    run_wrapper success 10 >"$work/locale-archive-valid.out" 2>&1
test "$(grep -Fxc -- "--setenv=LOCALE_ARCHIVE=$valid_locale_archive" \
    "$outer_state/systemd-run-args" || :)" -eq 1
test "$(grep -Fxc -- '--property=UnsetEnvironment=LOCALE_ARCHIVE' \
    "$outer_state/systemd-run-args" || :)" -eq 0
test "$(grep -Fxc -- '--property=UnsetEnvironment=LOCPATH' \
    "$outer_state/systemd-run-args" || :)" -eq 1
test "$(grep -Fxc -- '--property=UnsetEnvironment=LOCALE_ARCHIVE_2_27' \
    "$outer_state/systemd-run-args" || :)" -eq 1
test "$(cat "$outer_state/locale-archive-effective")" = "$valid_locale_archive"
jq -e '.result == "passed"' "$evidence/fixtures-ci-proof.json" >/dev/null
assert_bounded_inventory

(
    unset LOCALE_ARCHIVE
    FAKE_REQUIRE_LOCALE_ARCHIVE_UNSET=1 \
        run_wrapper success 10 >"$work/locale-archive-unset.out" 2>&1
)
test "$(grep -Fxc -- '--property=UnsetEnvironment=LOCALE_ARCHIVE' \
    "$outer_state/systemd-run-args" || :)" -eq 1
test "$(grep -Fc -- '--setenv=LOCALE_ARCHIVE=' \
    "$outer_state/systemd-run-args" || :)" -eq 0
test "$(cat "$outer_state/locale-archive-effective")" = '<unset>'
jq -e '.result == "passed"' "$evidence/fixtures-ci-proof.json" >/dev/null
assert_bounded_inventory

set +e
LOCALE_ARCHIVE=relative/locale-archive \
    run_wrapper success 10 >"$work/locale-archive-relative.out" 2>&1
status=$?
set -e
test "$status" -eq 2
grep -Fq 'LOCALE_ARCHIVE must name an absolute path: relative/locale-archive' \
    "$work/locale-archive-relative.out"
test ! -e "$outer_state/environment"

set +e
LOCALE_ARCHIVE="$locale_archive_directory/missing" \
    run_wrapper success 10 >"$work/locale-archive-missing.out" 2>&1
status=$?
set -e
test "$status" -eq 1
grep -Fq 'LOCALE_ARCHIVE must name a readable regular non-symlink file:' \
    "$work/locale-archive-missing.out"
test ! -e "$outer_state/environment"

set +e
LOCALE_ARCHIVE="$locale_archive_symlink" \
    run_wrapper success 10 >"$work/locale-archive-symlink.out" 2>&1
status=$?
set -e
test "$status" -eq 1
grep -Fq 'LOCALE_ARCHIVE must name a readable regular non-symlink file:' \
    "$work/locale-archive-symlink.out"
test ! -e "$outer_state/environment"

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
    "$real_bash" "$runner" >"$work/hostile-evidence.out" 2>&1
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
    "$real_bash" "$runner" >"$work/oversized.out" 2>&1
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
