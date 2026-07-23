#!/usr/bin/env bash

set -euo pipefail
umask 077

readonly root="${TOP_DIR:-$(pwd)}"
readonly fixtures="$root/tests/fixtures/gluon/execution"
readonly archive="$fixtures/archives/cast-multiple-sources-fixture-1.0.0.tar.xz"
readonly raw="$fixtures/archives/cast-multiple-sources-schema-1.0.0.h"
readonly bundle="$fixtures/git-bundles/cast-multiple-sources-protocol-1.0.0.bundle"
readonly archive_sha256="35c3c296ce08dd0ae1ebf27c782acf77f0f577f080b5ebe467c6c646f9ec7324"
readonly raw_sha256="6c4caab665188429a1f0eda479c4aa11d1f61240f09ac0d201cf80d20fa5e330"
readonly bundle_sha256="26531c6b1aa55b55c592f8f513f4e584c0c696a1888ef0f9948cfd265d57c5ed"
readonly commit="4f124a6f438b061a836e332d67e803a69a7bf2d3"
readonly branch="main"
readonly identity="cast multiple sources fixture: archive-main+git-protocol-v2+raw-schema-v3"

temporary="$(timeout 10s mktemp -d "${TMPDIR:-/tmp}/cast-multiple-sources-compilers.XXXXXXXX")"
cleanup() {
    timeout 10s rm -rf -- "$temporary"
}
trap cleanup EXIT HUP INT TERM

verify_digest() {
    local expected="$1"
    local path="$2"
    local actual
    timeout 10s test -f "$path"
    actual="$(timeout 10s sha256sum "$path")"
    actual="${actual%% *}"
    timeout 10s test "$actual" = "$expected"
}

verify_digest "$archive_sha256" "$archive"
verify_digest "$raw_sha256" "$raw"
verify_digest "$bundle_sha256" "$bundle"

readonly application="$temporary/application"
readonly shared="$temporary/shared"
readonly repository="$temporary/repository"
readonly vendor="$temporary/vendor-protocol"
readonly git_home="$temporary/git-home"
readonly -a git_environment=(
    "GIT_ATTR_NOSYSTEM=1"
    "GIT_CONFIG_GLOBAL=/dev/null"
    "GIT_CONFIG_NOSYSTEM=1"
    "HOME=$git_home"
    "LC_ALL=C"
    "PATH=$PATH"
)
timeout 10s install -d -m 700 "$application" "$shared" "$vendor" "$git_home"
timeout 30s tar -xJf "$archive" --strip-components=1 -C "$application"
timeout 10s cp --preserve=mode,timestamps -- "$raw" "$shared/protocol-schema.h"

# Bundles do not carry a symbolic HEAD. Select the pinned branch explicitly and
# force a hostile default so this test cannot silently depend on host Git setup.
timeout 30s env -i "${git_environment[@]}" git \
    -c init.defaultBranch=fixture-unborn \
    -c protocol.file.allow=always clone \
    --no-checkout --quiet --branch "$branch" "$bundle" "$repository"
resolved_commit="$(
    timeout 10s env -i "${git_environment[@]}" git \
        -C "$repository" rev-parse --verify HEAD
)"
timeout 10s test "$resolved_commit" = "$commit"
timeout 30s env -i "${git_environment[@]}" git \
    -C "$repository" archive \
    --format=tar --output="$temporary/vendor.tar" "$commit"
timeout 30s tar -xf "$temporary/vendor.tar" -C "$vendor"
timeout 10s test ! -e "$vendor/.git"

(
    cd "$application"
    CAST_SOURCE_DIR="$shared" timeout 10s bash -euo pipefail -c \
        'test ! -e src/protocol-schema.h && cp --preserve=mode,timestamps -- "${CAST_SOURCE_DIR}/protocol-schema.h" src/protocol-schema.h'
)
timeout 10s cmp -s "$shared/protocol-schema.h" "$application/src/protocol-schema.h"

for compiler in gcc clang; do
    compiler_path="$(timeout 10s sh -c 'command -v "$1"' sh "$compiler")"
    executable="$temporary/cast-multiple-sources-fixture-$compiler"
    timeout 30s "$compiler_path" \
        -std=c11 -Wall -Wextra -Wpedantic -Werror \
        -I"$vendor/include" \
        "$application/src/main.c" \
        -o "$executable"
    output="$(timeout 10s "$executable" --self-test)"
    timeout 10s test "$output" = "$identity"
done

printf '%s\n' \
    'multiple-sources: supplemental GCC/Clang source-composition check passed' \
    'note: this is not Meson, container, Stone-package, or delegated-reproduction proof'
