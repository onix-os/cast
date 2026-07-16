#!/usr/bin/env bash

set -euo pipefail

readonly script_dir=$(CDPATH= cd -- "${BASH_SOURCE[0]%/*}" && pwd -P)
readonly checker=${script_dir}/check-source-loc.sh
temporary=$(timeout 10s mktemp -d "${TMPDIR:-/tmp}/cast-source-loc.XXXXXX")
trap 'timeout 10s rm -rf -- "${temporary}"' EXIT HUP INT TERM
readonly repository=${temporary}/repository
readonly output=${temporary}/output

timeout 10s mkdir -p \
    "${repository}/src" \
    "${repository}/docs" \
    "${repository}/tests/fixtures/gluon/execution/bootstrap"
timeout 30s git -C "${temporary}" init -q repository

write_lines() {
    local count=$1
    local path=$2
    timeout 10s awk -v count="${count}" 'BEGIN { for (line = 1; line <= count; line++) print line }' > "${path}"
}

write_lines 1000 "${repository}/src/within limit.rs"
write_lines 1001 "${repository}/src/too long.rs"
write_lines 1002 "${repository}/docs/too long too.md"
write_lines 1200 "${repository}/Cargo.lock"
write_lines 1200 "${repository}/fixture.stone"
write_lines 1200 "${repository}/fixture.tar.gz"
write_lines 1200 "${repository}/tests/fixtures/gluon/execution/bootstrap/stone.index"
timeout 30s git -C "${repository}" add -- .

if timeout 60s bash "${checker}" --repo-root "${repository}" >"${output}" 2>&1; then
    echo 'LOC checker accepted oversized tracked source files' >&2
    exit 1
fi
timeout 10s grep -F '1001  src/too long.rs' "${output}" >/dev/null
timeout 10s grep -F '1002  docs/too long too.md' "${output}" >/dev/null
if timeout 10s grep -E 'Cargo\.lock|fixture\.(stone|tar\.gz)|stone\.index' "${output}" >/dev/null; then
    echo 'LOC checker reported an excluded generated or binary fixture' >&2
    exit 1
fi

write_lines 1000 "${repository}/src/too long.rs"
write_lines 1000 "${repository}/docs/too long too.md"
timeout 60s bash "${checker}" --repo-root "${repository}" >"${output}"
timeout 10s grep -F 'are at most 1000 lines' "${output}" >/dev/null

echo 'Source LOC checker tests passed.'
