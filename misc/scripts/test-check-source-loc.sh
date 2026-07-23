#!/usr/bin/env bash

set -euo pipefail

readonly script_dir=$(CDPATH= cd -- "${BASH_SOURCE[0]%/*}" && pwd -P)
readonly checker=${script_dir}/check-source-loc.sh
temporary=$(mktemp -d "${TMPDIR:-/tmp}/cast-source-loc.XXXXXX")
trap 'rm -rf -- "${temporary}"' EXIT HUP INT TERM
readonly repository=${temporary}/repository
readonly output=${temporary}/output

mkdir -p \
    "${repository}/src" \
    "${repository}/docs" \
    "${repository}/tests/fixtures/gluon/execution/bootstrap"
git -C "${temporary}" init -q repository

write_lines() {
    local count=$1
    local path=$2
    awk -v count="${count}" 'BEGIN { for (line = 1; line <= count; line++) print line }' > "${path}"
}

write_lines 1000 "${repository}/src/within limit.rs"
write_lines 1001 "${repository}/src/too long.rs"
write_lines 1002 "${repository}/docs/too long too.md"
write_lines 1200 "${repository}/Cargo.lock"
write_lines 1200 "${repository}/fixture.stone"
write_lines 1200 "${repository}/fixture.tar.gz"
write_lines 1200 "${repository}/tests/fixtures/gluon/execution/bootstrap/stone.index"
git -C "${repository}" add -- .

if bash "${checker}" --repo-root "${repository}" >"${output}" 2>&1; then
    echo 'LOC checker accepted oversized tracked source files' >&2
    exit 1
fi
grep -F '1001  src/too long.rs' "${output}" >/dev/null
grep -F '1002  docs/too long too.md' "${output}" >/dev/null
if grep -E 'Cargo\.lock|fixture\.(stone|tar\.gz)|stone\.index' "${output}" >/dev/null; then
    echo 'LOC checker reported an excluded generated or binary fixture' >&2
    exit 1
fi

write_lines 1000 "${repository}/src/too long.rs"
write_lines 1000 "${repository}/docs/too long too.md"
bash "${checker}" --repo-root "${repository}" >"${output}"
grep -F 'are at most 1000 lines' "${output}" >/dev/null

echo 'Source LOC checker tests passed.'
