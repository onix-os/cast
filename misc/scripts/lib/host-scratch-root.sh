# shellcheck shell=sh
#
# Resolve a repository-private scratch root for host-validation scripts.
#
# A saturated per-user /tmp must not prevent a host gate from starting when the
# repository filesystem has ample capacity, so the default scratch root lives
# under the repository's own `target/` (mirroring the fixture-evidence pattern in
# misc/make/execution-fixtures.mk). An explicit `TMPDIR` still overrides.
#
# Sourced (POSIX/dash-compatible) by scripts in misc/scripts/, so `$0` is the
# sourcing script's path and its directory's parent-of-parent is the repo root.
# Usage: source this file, then create scratch under "${CAST_HOST_SCRATCH_ROOT}".

CAST_HOST_SCRATCH_ROOT="${TMPDIR:-$(CDPATH= cd -- "$(dirname -- "$0")/../.." && pwd -P)/target/host-validation}"
mkdir -p -- "${CAST_HOST_SCRATCH_ROOT}"

# Non-destructive report of stale `cast-*` scratch artifacts left under the
# resolved root. Reports only; never deletes (an operator decides). Callers may
# invoke `cast_host_scratch_report` after their own cleanup for a quick audit.
cast_host_scratch_report() {
    [ -d "${CAST_HOST_SCRATCH_ROOT}" ] || return 0
    cast_host_scratch_stale=$(find "${CAST_HOST_SCRATCH_ROOT}" -maxdepth 1 -name 'cast-*' 2>/dev/null || true)
    if [ -n "${cast_host_scratch_stale}" ]; then
        printf 'stale host-scratch artifacts under %s (not removed):\n%s\n' \
            "${CAST_HOST_SCRATCH_ROOT}" "${cast_host_scratch_stale}" >&2
    fi
    unset cast_host_scratch_stale
}
