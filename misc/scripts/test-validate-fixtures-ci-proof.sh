#!/bin/sh

set -eu

root=$(CDPATH= cd -- "$(dirname -- "$0")/../.." && pwd -P)
validator="$root/misc/scripts/validate-fixtures-ci-proof.sh"
generator="$root/misc/scripts/test-support/write-fixtures-ci-proof-v2.sh"
ledger_calculator="$root/misc/scripts/calculate-fixtures-ci-ledger.sh"
work=$(mktemp -d "${TMPDIR:-/tmp}/cast-proof-v2-validator-test.XXXXXXXXXXXX")
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

for helper in "$validator" "$generator" "$ledger_calculator"; do
    if [ -L "$helper" ] || [ ! -f "$helper" ] || [ ! -x "$helper" ]; then
        printf 'fixture proof test helper is unavailable or unsafe: %s\n' "$helper" >&2
        exit 1
    fi
done
command -v jq >/dev/null 2>&1 || {
    printf 'jq is required for fixture proof validator tests\n' >&2
    exit 1
}

commit=0123456789abcdef0123456789abcdef01234567
commit64=0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef
ledger_vector="$work/ledger-vector.entries"
printf '%s\n' \
    'a.stone|2|1e0bbd6c686ba050b8eb03ffeedc64fdc9d80947fce821abbe5d6dc8d252c5ac' \
    >"$ledger_vector"
test "$("$ledger_calculator" "$ledger_vector")" = \
    7163d5acedc73cb5c7a73a31f24a73925cddc9a6323f33c2ac3a6d235e4cb519
valid="$work/valid.json"
"$generator" "$valid" "$commit"
"$validator" "$valid" "$commit"

expect_rejected() {
    label=$1
    candidate=$2
    candidate_commit=${3:-$commit}
    set +e
    "$validator" "$candidate" "$candidate_commit" \
        >"$work/$label.out" 2>"$work/$label.err"
    status=$?
    set -e
    if [ "$status" -eq 0 ]; then
        printf 'fixture proof validator accepted adversarial case %s\n' "$label" >&2
        exit 1
    fi
}

mutate_and_reject() {
    label=$1
    filter=$2
    candidate="$work/$label.json"
    jq "$filter" "$valid" >"$candidate"
    chmod 644 "$candidate"
    expect_rejected "$label" "$candidate"
}

valid64="$work/valid64.json"
"$generator" "$valid64" "$commit64"
"$validator" "$valid64" "$commit64"
expect_rejected wrong-expected-commit "$valid" fedcba9876543210fedcba9876543210fedcba98

overwrite="$work/overwrite.json"
printf '%s\n' stale >"$overwrite"
"$generator" "$overwrite" "$commit"
"$validator" "$overwrite" "$commit"

set +e
"$generator" relative-proof.json "$commit" >"$work/generator-relative.out" \
    2>"$work/generator-relative.err"
relative_status=$?
"$generator" "$work/bad-commit.json" BAD >"$work/generator-commit.out" \
    2>"$work/generator-commit.err"
commit_status=$?
set -e
test "$relative_status" -eq 2
test "$commit_status" -eq 2
test ! -e "$work/bad-commit.json"

empty="$work/empty.json"
: >"$empty"
chmod 644 "$empty"
expect_rejected empty "$empty"

malformed="$work/malformed.json"
printf '%s\n' '{' >"$malformed"
chmod 644 "$malformed"
expect_rejected malformed "$malformed"

multiple="$work/multiple.json"
cat "$valid" "$valid" >"$multiple"
chmod 644 "$multiple"
expect_rejected multiple-documents "$multiple"

duplicate_top="$work/duplicate-top.json"
sed '0,/"schema": "cast.fixtures-ci-proof.v2",/s//"schema": "cast.fixtures-ci-proof.v2",\n  "schema": "cast.fixtures-ci-proof.v2",/' \
    "$valid" >"$duplicate_top"
chmod 644 "$duplicate_top"
expect_rejected duplicate-top-key "$duplicate_top"

duplicate_nested="$work/duplicate-nested.json"
sed '0,/"fixture_count": 24,/s//"fixture_count": 24,\n    "fixture_count": 24,/' \
    "$valid" >"$duplicate_nested"
chmod 644 "$duplicate_nested"
expect_rejected duplicate-nested-key "$duplicate_nested"

wrong_mode="$work/wrong-mode.json"
cp "$valid" "$wrong_mode"
chmod 600 "$wrong_mode"
expect_rejected wrong-mode "$wrong_mode"

symlink="$work/symlink.json"
ln -s "$valid" "$symlink"
expect_rejected symlink "$symlink"

hardlink="$work/hardlink.json"
ln "$valid" "$hardlink"
expect_rejected hardlink "$valid"
rm "$hardlink"
"$validator" "$valid" "$commit"

exact_bound="$work/exact-bound.json"
cp "$valid" "$exact_bound"
exact_size=$(stat -c '%s' "$exact_bound")
padding=$((131072 - exact_size))
test "$padding" -gt 0
dd if=/dev/zero bs=1 count="$padding" status=none \
    | tr '\000' ' ' >>"$exact_bound"
chmod 644 "$exact_bound"
test "$(stat -c '%s' "$exact_bound")" -eq 131072
"$validator" "$exact_bound" "$commit"
printf ' ' >>"$exact_bound"
expect_rejected over-byte-bound "$exact_bound"

mutate_and_reject v1-schema '.schema = "cast.fixtures-ci-proof.v1"'
mutate_and_reject dirty-tree '.git_tree = "dirty"'
mutate_and_reject wrong-selection '.selection = "autotools"'
mutate_and_reject optional-execution '.required_execution = false'
mutate_and_reject wrong-ledger-schema '.bundle_ledger_schema = "cast.fixtures-ci.bundle.v0"'
mutate_and_reject failed-result '.result = "failed"'
mutate_and_reject extra-top-key '.unexpected = true'
mutate_and_reject reordered-top-keys '{git_commit: .git_commit} + .'

mutate_and_reject execution-total '.totals.execution_count = 31'
mutate_and_reject bundle-total '.totals.bundle_validation_count = 47'
mutate_and_reject stone-total '.totals.stone_count = 103'
mutate_and_reject manifest-total '.totals.manifest_count = 31'
mutate_and_reject artifact-total '.totals.artifact_count = 135'
mutate_and_reject byte-total '.totals.artifact_bytes += 1'
mutate_and_reject byte-total-zero '.totals.artifact_bytes = 0'
mutate_and_reject byte-total-overflow '.totals.artifact_bytes = 4294967297'
mutate_and_reject extra-total-key '.totals.unexpected = 1'

mutate_and_reject fixture-order '.fixtures |= reverse'
mutate_and_reject fixture-name '.fixtures[0].name = "other"'
mutate_and_reject fixture-extra-key '.fixtures[0].unexpected = true'
mutate_and_reject plan-empty '.fixtures[0].plans.first.byte_count = 0'
mutate_and_reject plan-oversized \
    '.fixtures[0].plans.first.byte_count = 16777217 | .fixtures[0].plans.repeat.byte_count = 16777217'
mutate_and_reject plan-repeat-drift '.fixtures[0].plans.repeat.byte_count += 1'
mutate_and_reject plan-identity-drift \
    '.fixtures[0].plans.first.derivation_id = "ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff"'
mutate_and_reject plan-uppercase \
    '.fixtures[0].plans.first.sha256 = "FFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFF"'

mutate_and_reject lock-first-outcome '.fixtures[0].build_locks.first.write_outcome = "unchanged"'
mutate_and_reject lock-repeat-outcome '.fixtures[0].build_locks.repeat.write_outcome = "written"'
mutate_and_reject lock-oversized \
    '.fixtures[0].build_locks.first.byte_count = 1048577 | .fixtures[0].build_locks.repeat.byte_count = 1048577'
mutate_and_reject lock-repeat-drift '.fixtures[0].build_locks.repeat.byte_count += 1'
mutate_and_reject publication-drift '.fixtures[0].publications.repeat = "published"'

mutate_and_reject artifact-unsafe-name '.fixtures[0].artifacts.entries[0].name = "../escape.stone"'
mutate_and_reject artifact-unsorted '.fixtures[0].artifacts.entries |= reverse'
mutate_and_reject artifact-duplicate-name \
    '.fixtures[0].artifacts.entries[1].name = .fixtures[0].artifacts.entries[0].name'
mutate_and_reject artifact-kind '.fixtures[0].artifacts.entries[0].kind = "manifest-bin"'
mutate_and_reject artifact-empty '.fixtures[0].artifacts.entries[0].byte_count = 0'
mutate_and_reject artifact-oversized '.fixtures[0].artifacts.entries[0].byte_count = 134217729'
mutate_and_reject artifact-total-oversized '.fixtures[0].artifacts.total_bytes = 268435457'
mutate_and_reject artifact-sum '.fixtures[0].artifacts.total_bytes += 1'
mutate_and_reject artifact-count '.fixtures[0].artifacts.artifact_count -= 1'
mutate_and_reject stone-count '.fixtures[0].artifacts.stone_count = 8'
mutate_and_reject manifest-missing '.fixtures[0].artifacts.entries |= map(select(.kind != "manifest-jsonc"))'
mutate_and_reject ledger-uppercase '.fixtures[0].artifacts.ledger_sha256 |= ascii_upcase'
mutate_and_reject coordinated-ledger-forgery \
    '.fixtures[0].artifacts.ledger_sha256 = "ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff"
     | .fixtures[0].bundle_observations[].ledger_sha256 = "ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff"'
mutate_and_reject artifact-extra-key '.fixtures[0].artifacts.entries[0].unexpected = true'

mutate_and_reject observation-order '.fixtures[0].bundle_observations |= reverse'
mutate_and_reject observation-missing '.fixtures[0].bundle_observations |= .[0:2]'
mutate_and_reject observation-count '.fixtures[0].bundle_observations[0].artifact_count -= 1'
mutate_and_reject observation-bytes '.fixtures[0].bundle_observations[0].total_bytes += 1'
mutate_and_reject observation-ledger \
    '.fixtures[0].bundle_observations[0].ledger_sha256 = "ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff"'
mutate_and_reject observation-extra-key '.fixtures[0].bundle_observations[0].unexpected = true'

printf '%s\n' 'fixture CI proof v2 validator tests passed'
