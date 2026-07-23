#!/usr/bin/dash

# Supplemental only: this compiles and tests the exact locked source and raw
# corpus with host CMake/CTest in a disposable directory. It is not a Stone,
# container, transaction, rollback, boot, or Nix-compatibility proof.
set -eu
umask 022

root=$(CDPATH= cd -- "$(timeout 10s dirname -- "$0")/../.." && pwd)
fixture_root="$root/tests/fixtures/gluon/execution"
tree_name=cast-external-test-vectors-fixture-1.0.0
archive="$fixture_root/archives/$tree_name.tar"
raw="$fixture_root/archives/$tree_name-vectors.json"
authored="$fixture_root/source-trees/$tree_name"
expected_archive_sha256=c04932c66a3399d95fda58b459b7f3e903454e5d2963f3529667216fdad50404
expected_raw_sha256=c957dce5105c8add6e581f9aa0ee34002cbff8c9d403fab5132cebf0a8c7685c
marker='cast external test vectors fixture: 3 independently locked vectors verified'

fixture_path=${PATH-}
test -n "$fixture_path" || {
    printf '%s\n' 'external-test-vectors: PATH is unavailable' >&2
    exit 1
}
cmake_bin=$(command -v cmake)
ctest_bin=$(command -v ctest)
ninja_bin=$(command -v ninja)
fixture_cc=$(command -v clang)
for required_tool in "$cmake_bin" "$ctest_bin" "$ninja_bin" "$fixture_cc"; do
    case "$required_tool" in
        /*) ;;
        *)
            printf 'external-test-vectors: tool did not resolve to an absolute path: %s\n' \
                "$required_tool" >&2
            exit 1
            ;;
    esac
    test -x "$required_tool" || {
        printf 'external-test-vectors: tool is not executable: %s\n' \
            "$required_tool" >&2
        exit 1
    }
done

temporary=$(timeout 10s mktemp -d "${TMPDIR:-/tmp}/cast-external-test-vectors-host.XXXXXXXX")
cleanup() {
    timeout 30s chmod -R u+w "$temporary" 2>/dev/null || :
    timeout 30s rm -rf "$temporary"
}
trap cleanup EXIT HUP INT TERM
timeout 10s chmod 0700 "$temporary"

archive_hash=$(timeout 10s sha256sum "$archive")
raw_hash=$(timeout 10s sha256sum "$raw")
test "${archive_hash%% *}" = "$expected_archive_sha256"
test "${raw_hash%% *}" = "$expected_raw_sha256"
timeout 10s cmp "$raw" "$fixture_root/source-files/$tree_name-vectors.json"

source_root="$temporary/source"
build="$temporary/build"
stage="$temporary/stage"
host_home="$temporary/host-home"
host_cache="$temporary/cache"
host_config="$temporary/config"
host_tmp="$temporary/tmp"
timeout 10s mkdir -p \
    "$source_root" "$build" "$stage" "$host_home" \
    "$host_cache" "$host_config" "$host_tmp"
timeout 30s tar -xf "$archive" -C "$source_root"
extracted="$source_root/$tree_name"

# Start from an empty environment so no ambient compiler, linker, CMake,
# CTest, or build-tool policy can cross the boundary. Only the unsuffixed
# policy inputs consumed by the pinned Nix cc-wrapper are retained. The
# compiler and generator tools are resolved once from the dev-shell PATH and
# supplied explicitly.
sanitized_fixture_env() {
    fixture_timeout=$1
    shift
    timeout "$fixture_timeout" env -i \
        PATH="$fixture_path" \
        HOME="$host_home" \
        XDG_CACHE_HOME="$host_cache" \
        XDG_CONFIG_HOME="$host_config" \
        TMPDIR="$host_tmp" \
        LANG=C \
        LC_ALL=C \
        TZ=Pacific/Kiritimati \
        SOURCE_DATE_EPOCH=1 \
        CC="$fixture_cc" \
        NIX_CFLAGS_COMPILE="${NIX_CFLAGS_COMPILE-}" \
        NIX_ENFORCE_NO_NATIVE="${NIX_ENFORCE_NO_NATIVE-}" \
        NIX_HARDENING_ENABLE="${NIX_HARDENING_ENABLE-}" \
        NIX_LDFLAGS="${NIX_LDFLAGS-}" \
        "$@"
}

printf '%s\n' \
    'f 644 CMakeLists.txt' \
    'f 644 frame_codec.c' \
    > "$temporary/expected-inventory"
timeout 10s find "$extracted" -mindepth 1 -printf '%y %m %P\n' \
    > "$temporary/inventory-unsorted"
timeout 10s sort "$temporary/inventory-unsorted" -o "$temporary/observed-inventory"
timeout 10s cmp "$temporary/expected-inventory" "$temporary/observed-inventory"
timeout 10s cmp "$authored/CMakeLists.txt" "$extracted/CMakeLists.txt"
timeout 10s cmp "$authored/frame_codec.c" "$extracted/frame_codec.c"

sanitized_fixture_env 120s "$cmake_bin" \
    -S "$extracted" -B "$build" -G Ninja \
        -DCMAKE_C_COMPILER="$fixture_cc" \
        -DCMAKE_MAKE_PROGRAM="$ninja_bin" \
        -DBUILD_TESTING=ON \
        -DCAST_EXTERNAL_VECTOR_FILE=external-test-vectors.json \
        -DCMAKE_BUILD_TYPE=Release
sanitized_fixture_env 120s "$cmake_bin" --build "$build"

set +e
sanitized_fixture_env 30s "$ctest_bin" \
    --test-dir "$build" --output-on-failure \
    > "$temporary/missing.stdout" 2> "$temporary/missing.stderr"
missing_status=$?
set -e
test "$missing_status" -ne 0
timeout 10s test ! -e "$build/external-test-vectors.json"

timeout 10s cp --preserve=mode,timestamps -- \
    "$raw" "$build/external-test-vectors.json"
sanitized_fixture_env 30s "$ctest_bin" \
    --test-dir "$build" --output-on-failure \
    > "$temporary/valid.stdout" 2> "$temporary/valid.stderr"
timeout 10s grep -Fq '100% tests passed' "$temporary/valid.stdout"

sanitized_fixture_env 30s DESTDIR="$stage" \
    "$cmake_bin" --install "$build" --prefix /usr
printf '%s\n' \
    'd 755 usr' \
    'd 755 usr/bin' \
    'f 755 usr/bin/cast-external-test-vectors-fixture' \
    > "$temporary/expected-stage-inventory"
timeout 10s find "$stage" -mindepth 1 -printf '%y %m %P\n' \
    > "$temporary/stage-inventory-unsorted"
timeout 10s sort "$temporary/stage-inventory-unsorted" \
    -o "$temporary/observed-stage-inventory"
timeout 10s cmp "$temporary/expected-stage-inventory" \
    "$temporary/observed-stage-inventory"
timeout 10s test -x "$stage/usr/bin/cast-external-test-vectors-fixture"
self_test=$(sanitized_fixture_env 10s \
    "$stage/usr/bin/cast-external-test-vectors-fixture" --self-test)
test "$self_test" = 'cast external test vectors fixture: codec self-test passed'

printf '%s\n' \
    '{"schema":1,"vectors":[' \
    '{"plain":0,"encoded":"00"},' \
    '{"plain":42,"encoded":"ff"},' \
    '{"plain":255,"encoded":"ff"}' \
    ']}' \
    > "$build/external-test-vectors.json"
set +e
sanitized_fixture_env 30s "$ctest_bin" \
    --test-dir "$build" --output-on-failure \
    > "$temporary/tampered.stdout" 2> "$temporary/tampered.stderr"
tampered_status=$?
set -e
test "$tampered_status" -ne 0
timeout 10s grep -Fq 'external vector corpus disagrees with the codec' \
    "$temporary/tampered.stdout"

printf '%s\n' "external-test-vectors: supplemental host CMake/CTest validation passed ($marker)"
