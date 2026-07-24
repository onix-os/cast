#!/usr/bin/env bash

set -euo pipefail

readonly max_lines=1000

usage() {
    printf 'Usage: %s [--repo-root DIR]\n' "${0##*/}"
}

repo_root=''
while (( $# != 0 )); do
    case "$1" in
        --repo-root)
            (( $# >= 2 )) || { usage >&2; exit 2; }
            repo_root=$2
            shift 2
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
done

if [[ -z "${repo_root}" ]]; then
    repo_root=$(git rev-parse --show-toplevel)
fi
repo_root=$(cd -- "${repo_root}" && pwd -P)
git -C "${repo_root}" rev-parse --is-inside-work-tree >/dev/null

tracked_list=$(mktemp "${TMPDIR:-/tmp}/cast-source-loc-files.XXXXXX")
cleanup() {
    rm -f -- "${tracked_list}"
}
trap cleanup EXIT HUP INT TERM
git -C "${repo_root}" ls-files -z > "${tracked_list}"

is_generated_or_binary_fixture() {
    local path=${1,,}
    local name=${path##*/}

    case "${name}" in
        *.lock | *-lock.json | *-lock.yaml | *-lock.yml | npm-shrinkwrap.json | yarn.lock | .cargo-checksum.json)
            return 0
            ;;
    esac
    case "${path}" in
        *.stone | *.tar | *.tar.gz | *.tar.bz2 | *.tar.xz | *.tar.zst \
            | *.tgz | *.tbz | *.tbz2 | *.txz | *.gz | *.bz2 | *.xz | *.zst \
            | *.zip | *.7z | *.rar | *.lz4 \
            | tests/fixtures/gluon/execution/bootstrap/stone.index)
            return 0
            ;;
    esac
    return 1
}

is_owned_text() {
    local path=${1,,}
    local name=${path##*/}

    is_generated_or_binary_fixture "${path}" && return 1
    case "${path}" in
        *.rs | *.glu | *.lua | *.sh | *.bash | *.zsh | *.fish | *.mk | *.make \
            | *.md | *.markdown | *.rst | *.adoc | *.txt | *.toml | *.nix \
            | *.c | *.h | *.cc | *.cpp | *.cxx | *.hh | *.hpp | *.hxx \
            | *.sql | *.json | *.json5 | *.yaml | *.yml | *.xml \
            | *.service | *.socket | *.timer | *.target | *.path | *.mount \
            | *.automount | *.slice | *.conf | *.cfg | *.ini | *.desktop \
            | *.rules | *.py | *.pyi | *.patch | *.diff | *.in | *.cmake \
            | *.ac | *.m4 | *.pc | *.[1-9])
            return 0
            ;;
    esac
    case "${name}" in
        makefile | makefile.* | gnumakefile | meson.build | meson_options.txt \
            | configure | codeowners | license | license.* | copying | copying.* \
            | notice | notice.* | authors | authors.* | readme | changelog \
            | contributing | security | dockerfile | containerfile | justfile \
            | .envrc | .gitignore | .gitattributes | .editorconfig)
            return 0
            ;;
    esac
    return 1
}

declare -a violating_paths=()
declare -a violating_counts=()
declare -a missing_paths=()
checked=0

while IFS= read -r -d '' path; do
    is_owned_text "${path}" || continue
    absolute=${repo_root}/${path}
    if [[ ! -f "${absolute}" ]]; then
        missing_paths+=("${path}")
        continue
    fi
    lines=$(awk 'END { print NR }' < "${absolute}")
    (( checked += 1 ))
    if (( lines > max_lines )); then
        violating_paths+=("${path}")
        violating_counts+=("${lines}")
    fi
done < "${tracked_list}"

if (( ${#missing_paths[@]} != 0 )); then
    printf 'Tracked text paths missing from the worktree:\n' >&2
    printf '  %s\n' "${missing_paths[@]}" >&2
fi
if (( ${#violating_paths[@]} != 0 )); then
    printf 'Tracked repo-owned text files exceed %d lines:\n' "${max_lines}" >&2
    for index in "${!violating_paths[@]}"; do
        printf '%7d  %s\n' "${violating_counts[index]}" "${violating_paths[index]}" >&2
    done
fi
if (( ${#missing_paths[@]} != 0 || ${#violating_paths[@]} != 0 )); then
    exit 1
fi

printf 'Source LOC limit satisfied: %d tracked text files are at most %d lines.\n' "${checked}" "${max_lines}"
