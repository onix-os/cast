run_chmod_failure_bin="$work/run-chmod-failure-bin"
mkdir "$run_chmod_failure_bin"
cat >"$run_chmod_failure_bin/chmod" <<'EOF'
#!/bin/sh
set -eu
: "${REAL_CHMOD:?}"
if [ "${1-}" = 700 ] && [ "$#" -eq 2 ]; then
    case "$2" in
        */.fixtures-ci-run.*) exit 74 ;;
    esac
fi
exec "$REAL_CHMOD" "$@"
EOF
chmod 755 "$run_chmod_failure_bin/chmod"
set +e
RUN_WRAPPER_TEST_PATH_PREFIX="$run_chmod_failure_bin" \
    run_wrapper success 10 >"$work/run-chmod-failure.out" 2>&1
run_chmod_status=$?
set -e
test "$run_chmod_status" -eq 1
grep -Fq 'fixture CI run staging has no authenticated identity:' \
    "$work/run-chmod-failure.out"
test ! -e "$evidence/fixtures-ci-proof.json"
test ! -e "$outer_state/unit"
test "$(find "$evidence" -mindepth 1 -maxdepth 1 \
    -name '.fixtures-ci-run.*' -print | wc -l)" -eq 1
unbound_stale_run=$(find "$evidence" -mindepth 1 -maxdepth 1 \
    -name '.fixtures-ci-run.*' -print -quit)
test -d "$unbound_stale_run"
test ! -L "$unbound_stale_run"
test "$(stat -c '%a' "$unbound_stale_run")" = 700

stale_proof_snapshot="$work/stale-proof-snapshot.json"
stale_log_snapshot="$work/stale-log-snapshot"
run_wrapper success 10 >"$work/stale-boundary-baseline.out" 2>&1
jq -e '.result == "passed"' "$evidence/fixtures-ci-proof.json" >/dev/null
assert_bounded_inventory
cp -- "$evidence/fixtures-ci-proof.json" "$stale_proof_snapshot"
cp -- "$evidence/fixtures-ci.log" "$stale_log_snapshot"

expect_stale_rejection() {
    stale_label=$1
    stale_message=$2
    set +e
    run_wrapper success 10 >"$work/$stale_label.out" 2>&1
    stale_status=$?
    set -e
    test "$stale_status" -eq 1
    grep -Fq "$stale_message" "$work/$stale_label.out"
    cmp -s "$stale_proof_snapshot" "$evidence/fixtures-ci-proof.json"
    cmp -s "$stale_log_snapshot" "$evidence/fixtures-ci.log"
    test ! -e "$outer_state/unit"
    test ! -e "$outer_state/command-pid"
    test ! -e "$outer_state/environment"
}

# All candidates are authenticated before any stale staging is removed. The
# alphabetically first valid run must remain untouched when a later candidate
# has unsafe metadata.
stale_valid="$evidence/.fixtures-ci-run.AValidRun001"
stale_bad_mode="$evidence/.fixtures-ci-run.ZBadMode0000"
stale_flat_target="$work/stale-flat-target"
mkdir "$stale_valid" "$stale_bad_mode"
printf '%s\n' 'flat-target-must-survive' >"$stale_flat_target"
chmod 700 "$stale_valid"
chmod 750 "$stale_bad_mode"
printf '%s\n' 'preserve-until-full-validation' >"$stale_valid/payload"
mkfifo -m 600 "$stale_valid/leftover-output.fifo"
ln -s "$stale_flat_target" "$stale_valid/target-link"
printf '%s\n' 'bad-mode' >"$stale_bad_mode/payload"
expect_stale_rejection stale-mode \
    'stale fixture CI run must be caller-owned with mode 700:'
grep -Fqx 'preserve-until-full-validation' "$stale_valid/payload"
test -p "$stale_valid/leftover-output.fifo"
test -L "$stale_valid/target-link"
test -f "$stale_bad_mode/payload"
chmod 700 "$stale_bad_mode"
run_wrapper success 10 >"$work/stale-mode-recovery.out" 2>&1
test ! -e "$stale_valid"
test ! -e "$stale_bad_mode"
grep -Fqx 'flat-target-must-survive' "$stale_flat_target"
assert_bounded_inventory
cp -- "$evidence/fixtures-ci-proof.json" "$stale_proof_snapshot"
cp -- "$evidence/fixtures-ci.log" "$stale_log_snapshot"

stale_malformed="$evidence/.fixtures-ci-run.short"
mkdir -m 700 "$stale_malformed"
expect_stale_rejection stale-malformed \
    'fixture CI found a malformed stale run name:'
rmdir "$stale_malformed"

stale_bad_token="$evidence/.fixtures-ci-run.Bad-Token001"
mkdir -m 700 "$stale_bad_token"
expect_stale_rejection stale-token \
    'fixture CI found a malformed stale run name:'
rmdir "$stale_bad_token"

stale_link_target="$work/stale-link-target"
stale_link="$evidence/.fixtures-ci-run.SymlinkRun01"
mkdir "$stale_link_target"
printf '%s\n' 'must-survive' >"$stale_link_target/payload"
ln -s "$stale_link_target" "$stale_link"
expect_stale_rejection stale-symlink \
    'stale fixture CI run must be a non-symlink directory:'
grep -Fqx 'must-survive' "$stale_link_target/payload"
rm "$stale_link"

stale_file="$evidence/.fixtures-ci-run.RegularFile1"
printf '%s\n' 'must-remain-a-file' >"$stale_file"
expect_stale_rejection stale-file \
    'stale fixture CI run must be a non-symlink directory:'
grep -Fqx 'must-remain-a-file' "$stale_file"
rm "$stale_file"

stale_nested="$evidence/.fixtures-ci-run.NestedRun001"
mkdir -m 700 "$stale_nested"
mkdir "$stale_nested/child"
expect_stale_rejection stale-nested \
    'stale fixture CI run contains a nested directory:'
test -d "$stale_nested/child"
rmdir "$stale_nested/child" "$stale_nested"

set +e
run_wrapper current-run-nested 10 >"$work/current-run-nested.out" 2>&1
current_nested_status=$?
set -e
test "$current_nested_status" -eq 1
grep -Fq 'fixture CI run staging contains a nested directory:' \
    "$work/current-run-nested.out"
test ! -e "$evidence/fixtures-ci-proof.json"
current_nested_run=$(find "$evidence" -mindepth 1 -maxdepth 1 \
    -name '.fixtures-ci-run.*' -print -quit)
test -n "$current_nested_run"
test -d "$current_nested_run/injected-directory"
rmdir "$current_nested_run/injected-directory"
run_wrapper success 10 >"$work/current-run-nested-recovery.out" 2>&1
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
