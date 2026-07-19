#!/usr/bin/dash

# Supplemental only: this compiles and tests the exact locked source and raw
# corpus with host CMake/CTest in a disposable directory. It is not a Stone,
# container, transaction, rollback, boot, or Nix-compatibility proof.
set -eu

root=$(CDPATH= cd -- "$(timeout 10s dirname -- "$0")/../.." && pwd)
fixture_root="$root/tests/fixtures/gluon/execution"
tree_name=cast-external-test-vectors-fixture-1.0.0
archive="$fixture_root/archives/$tree_name.tar"
raw="$fixture_root/archives/$tree_name-vectors.json"
authored="$fixture_root/source-trees/$tree_name"
expected_archive_sha256=c04932c66a3399d95fda58b459b7f3e903454e5d2963f3529667216fdad50404
expected_raw_sha256=c957dce5105c8add6e581f9aa0ee34002cbff8c9d403fab5132cebf0a8c7685c
marker='cast external test vectors fixture: 3 independently locked vectors verified'

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
timeout 10s mkdir -p "$source_root" "$build" "$stage" "$host_home"
timeout 30s tar -xf "$archive" -C "$source_root"
extracted="$source_root/$tree_name"

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

timeout 120s env HOME="$host_home" XDG_CACHE_HOME="$temporary/cache" \
    LANG=C LC_ALL=C TZ=Pacific/Kiritimati SOURCE_DATE_EPOCH=1 \
    cmake -S "$extracted" -B "$build" -G Ninja \
        -DBUILD_TESTING=ON \
        -DCAST_EXTERNAL_VECTOR_FILE=external-test-vectors.json \
        -DCMAKE_BUILD_TYPE=Release
timeout 120s env HOME="$host_home" XDG_CACHE_HOME="$temporary/cache" \
    cmake --build "$build"

set +e
timeout 30s env HOME="$host_home" XDG_CACHE_HOME="$temporary/cache" \
    ctest --test-dir "$build" --output-on-failure \
    > "$temporary/missing.stdout" 2> "$temporary/missing.stderr"
missing_status=$?
set -e
test "$missing_status" -ne 0
timeout 10s test ! -e "$build/external-test-vectors.json"

timeout 10s cp --preserve=mode,timestamps -- \
    "$raw" "$build/external-test-vectors.json"
timeout 30s env HOME="$host_home" XDG_CACHE_HOME="$temporary/cache" \
    ctest --test-dir "$build" --output-on-failure \
    > "$temporary/valid.stdout" 2> "$temporary/valid.stderr"
timeout 10s grep -Fq '100% tests passed' "$temporary/valid.stdout"

timeout 30s env HOME="$host_home" XDG_CACHE_HOME="$temporary/cache" \
    DESTDIR="$stage" cmake --install "$build" --prefix /usr
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
self_test=$(timeout 10s env HOME="$host_home" XDG_CACHE_HOME="$temporary/cache" \
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
timeout 30s env HOME="$host_home" XDG_CACHE_HOME="$temporary/cache" \
    ctest --test-dir "$build" --output-on-failure \
    > "$temporary/tampered.stdout" 2> "$temporary/tampered.stderr"
tampered_status=$?
set -e
test "$tampered_status" -ne 0
timeout 10s grep -Fq 'external vector corpus disagrees with the codec' \
    "$temporary/tampered.stdout"

printf '%s\n' "external-test-vectors: supplemental host CMake/CTest validation passed ($marker)"
