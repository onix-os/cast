#!/usr/bin/env bash
# SPDX-FileCopyrightText: 2026 AerynOS Developers
# SPDX-License-Identifier: MPL-2.0

set -euo pipefail

readonly -a allowed=(
    '.github/dependabot.yml'
    '.github/workflows/ci.yaml'
    '.github/workflows/release.yaml'
)

usage() {
    cat <<'EOF'
Usage: check-config-formats.sh [--tracked-paths0 FILE]

Reject tracked YAML and KDL files outside the exact external-service allowlist.
FILE must contain NUL-delimited tracked paths. Without the option, paths are read
from `git ls-files -z`.
EOF
}

tracked_paths_file=''
case "${1-}" in
    '') ;;
    --tracked-paths0)
        if [[ $# -ne 2 ]]; then
            usage >&2
            exit 2
        fi
        tracked_paths_file=$2
        ;;
    -h | --help)
        usage
        exit 0
        ;;
    *)
        usage >&2
        exit 2
        ;;
esac

declare -A seen_paths=()
declare -a found=()
declare -a missing=()
declare -a unexpected=()

inspect_path() {
    local path=$1

    # Git paths are byte strings. Only the ASCII extension is case-insensitive;
    # the complete allowlisted path remains deliberately case-sensitive.
    if [[ "${path}" =~ \.[Yy][Aa][Mm][Ll]$ \
        || "${path}" =~ \.[Yy][Mm][Ll]$ \
        || "${path}" =~ \.[Kk][Dd][Ll]$ ]]; then
        found+=("${path}")
        case "${path}" in
            '.github/dependabot.yml' \
                | '.github/workflows/ci.yaml' \
                | '.github/workflows/release.yaml')
                seen_paths["${path}"]=1
                ;;
            *) unexpected+=("${path}") ;;
        esac
    fi
}

if [[ -n "${tracked_paths_file}" ]]; then
    while IFS= read -r -d '' path; do
        inspect_path "${path}"
    done < "${tracked_paths_file}"
else
    while IFS= read -r -d '' path; do
        inspect_path "${path}"
    done < <(git ls-files -z)
fi

for path in "${allowed[@]}"; do
    if [[ ! -v "seen_paths[${path}]" ]]; then
        missing+=("${path}")
    fi
done

if (( ${#found[@]} != 0 )); then
    mapfile -t found < <(printf '%s\n' "${found[@]}" | LC_ALL=C sort)
fi
if (( ${#missing[@]} != 0 )); then
    mapfile -t missing < <(printf '%s\n' "${missing[@]}" | LC_ALL=C sort)
fi
if (( ${#unexpected[@]} != 0 )); then
    mapfile -t unexpected < <(printf '%s\n' "${unexpected[@]}" | LC_ALL=C sort)
fi

if (( ${#missing[@]} != 0 || ${#unexpected[@]} != 0 )); then
    echo "Unexpected tracked YAML/KDL files." >&2
    echo "OS Tools-owned configuration must be Gluon; only these external interfaces are allowed:" >&2
    printf '%s\n' "${allowed[@]}" >&2
    echo >&2
    echo "Tracked YAML/KDL files:" >&2
    if (( ${#found[@]} != 0 )); then
        printf '%s\n' "${found[@]}" >&2
    else
        echo "(none)" >&2
    fi
    if (( ${#missing[@]} != 0 )); then
        echo >&2
        echo "Missing required external interfaces:" >&2
        printf '%s\n' "${missing[@]}" >&2
    fi
    if (( ${#unexpected[@]} != 0 )); then
        echo >&2
        echo "Disallowed tracked YAML/KDL files:" >&2
        printf '%s\n' "${unexpected[@]}" >&2
    fi
    exit 1
fi

echo "Configuration format allowlist is clean."
