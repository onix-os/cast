#!/bin/sh

set -eu

mode=write
case "${1-}" in
    '') ;;
    --check) mode=check ;;
    *) printf 'usage: %s [--check]\n' "$0" >&2; exit 2 ;;
esac

root=$(CDPATH= cd -- "$(dirname -- "$0")/../.." && pwd)
fixture_root="$root/tests/fixtures/gluon/execution"
package_root="$fixture_root/packages"
source_root="$fixture_root/source-trees"
source_file_root="$fixture_root/source-files"
archive_root="$fixture_root/archives"
temporary=
uncompressed=
check_root=

cleanup() {
    if [ -n "$temporary" ]; then
        rm -f "$temporary"
    fi
    if [ -n "$uncompressed" ]; then
        rm -f "$uncompressed"
    fi
    if [ -n "$check_root" ]; then
        rm -rf "$check_root"
    fi
}

trap cleanup EXIT HUP INT TERM

mkdir -p "$archive_root"

package_count=0
source_less_count=0
for entry in "$package_root"/*; do
    test -d "$entry" && test ! -L "$entry" || {
        printf 'unexpected non-directory execution fixture package entry: %s\n' "$entry" >&2
        exit 1
    }
    fixture=$(basename "$entry")
    case "$fixture" in
        autotools|autotools-options|cargo|cargo-features|cargo-vendored|cmake|custom|daemon-generated|factory-override|generated-config|generated-shell|hooks-patch|meson|plugin-output|post-install-smoke-test|split) ;;
        *) printf 'unexpected execution fixture package: %s\n' "$entry" >&2; exit 1 ;;
    esac
    test -f "$entry/stone.glu" && test ! -L "$entry/stone.glu" || {
        printf 'execution fixture recipe is not a regular authored file: %s\n' "$entry/stone.glu" >&2
        exit 1
    }
    if [ "$fixture" = generated-config ] || [ "$fixture" = generated-shell ]; then
        test ! -e "$entry/sources.lock.glu" && test ! -L "$entry/sources.lock.glu" || {
            printf 'source-less execution fixture must not have a source lock: %s\n' "$entry/sources.lock.glu" >&2
            exit 1
        }
        source_less_count=$((source_less_count + 1))
    else
        test -f "$entry/sources.lock.glu" && test ! -L "$entry/sources.lock.glu" || {
            printf 'source-backed execution fixture lock is not a regular file: %s\n' "$entry/sources.lock.glu" >&2
            exit 1
        }
    fi
    package_count=$((package_count + 1))
done

test "$package_count" -eq 16 || {
    printf 'expected exactly sixteen archive-matrix package directories, found %s\n' "$package_count" >&2
    exit 1
}
test "$source_less_count" -eq 2 || {
    printf 'expected exactly two source-less archive-matrix fixtures, found %s\n' "$source_less_count" >&2
    exit 1
}

userspace_root="$root/tests/fixtures/gluon/userspace-profile"
test -d "$userspace_root" && test ! -L "$userspace_root" || {
    printf 'userspace-profile execution fixture is unavailable or unsafe: %s\n' "$userspace_root" >&2
    exit 1
}
for authored in package_set.glu roles.glu stone.glu; do
    test -f "$userspace_root/$authored" && test ! -L "$userspace_root/$authored" || {
        printf 'userspace-profile authored input is unavailable or unsafe: %s\n' "$userspace_root/$authored" >&2
        exit 1
    }
done
for forbidden in sources.lock.glu build.lock.glu; do
    test ! -e "$userspace_root/$forbidden" && test ! -L "$userspace_root/$forbidden" || {
        printf 'source-less userspace-profile must not contain %s\n' "$forbidden" >&2
        exit 1
    }
done

source_tree_count=0
for entry in "$source_root"/*; do
    test -d "$entry" && test ! -L "$entry" || {
        printf 'unexpected non-directory execution source-tree entry: %s\n' "$entry" >&2
        exit 1
    }
    case "$(basename "$entry")" in
        cast-autotools-fixture-1.0.0|\
        cast-autotools-options-fixture-1.0.0|\
        cast-cargo-features-fixture-1.0.0|\
        cast-cargo-vendored-fixture-1.0.0|\
        cast-cargo-fixture-1.0.0|\
        cast-cmake-fixture-1.0.0|\
        cast-custom-fixture-1.0.0|\
        cast-daemon-fixture-1.0.0|\
        cast-factory-override-fixture-1.0.0|\
        cast-hooks-fixture-1.0.0|\
        cast-meson-fixture-1.0.0|\
        cast-plugin-output-fixture-1.0.0|\
        cast-post-install-smoke-test-fixture-1.0.0|\
        cast-split-fixture-1.0.0) ;;
        *) printf 'unexpected execution source tree: %s\n' "$entry" >&2; exit 1 ;;
    esac
    source_tree_count=$((source_tree_count + 1))
done

test "$source_tree_count" -eq 14 || {
    printf 'expected exactly fourteen source-backed execution fixture trees, found %s\n' "$source_tree_count" >&2
    exit 1
}

source_file_count=0
for entry in "$source_file_root"/*; do
    test -f "$entry" && test ! -L "$entry" || {
        printf 'unexpected non-regular execution source-file entry: %s\n' "$entry" >&2
        exit 1
    }
    case "$(basename "$entry")" in
        cast-hooks-fixture-1.0.0-pre-setup.patch) ;;
        *) printf 'unexpected execution source file: %s\n' "$entry" >&2; exit 1 ;;
    esac
    source_file_count=$((source_file_count + 1))
done

test "$source_file_count" -eq 1 || {
    printf 'expected exactly one independent execution source file, found %s\n' "$source_file_count" >&2
    exit 1
}

if [ "$mode" = check ]; then
    check_root=$(mktemp -d "${TMPDIR:-/tmp}/cast-execution-fixtures.XXXXXX")
fi

for fixture in \
    cast-autotools-fixture-1.0.0 \
    cast-autotools-options-fixture-1.0.0 \
    cast-cargo-features-fixture-1.0.0 \
    cast-cargo-vendored-fixture-1.0.0 \
    cast-cargo-fixture-1.0.0 \
    cast-cmake-fixture-1.0.0 \
    cast-custom-fixture-1.0.0 \
    cast-daemon-fixture-1.0.0 \
    cast-factory-override-fixture-1.0.0 \
    cast-hooks-fixture-1.0.0 \
    cast-meson-fixture-1.0.0 \
    cast-plugin-output-fixture-1.0.0 \
    cast-post-install-smoke-test-fixture-1.0.0 \
    cast-split-fixture-1.0.0
do
    source="$source_root/$fixture"
    case "$fixture" in
        cast-cargo-vendored-fixture-1.0.0)
            suffix=tar.gz
            compression=gzip
            ;;
        cast-hooks-fixture-1.0.0)
            suffix=tar.xz
            compression=xz
            ;;
        cast-daemon-fixture-1.0.0)
            suffix=tar.zst
            compression=zstd
            ;;
        *)
            suffix=tar
            compression=none
            ;;
    esac
    output="$archive_root/$fixture.$suffix"
    if [ "$mode" = check ]; then
        temporary="$check_root/$fixture.$suffix"
        uncompressed="$check_root/$fixture.raw.tar"
    else
        temporary="$archive_root/.$fixture.$suffix.tmp"
        uncompressed="$archive_root/.$fixture.raw-tar.tmp"
    fi

    test -d "$source"
    rm -f "$temporary"
    rm -f "$uncompressed"
    tar \
        --format=ustar \
        --sort=name \
        --mtime=@1700000000 \
        --owner=0 \
        --group=0 \
        --numeric-owner \
        --mode='u+rwX,go+rX,go-w' \
        -C "$source_root" \
        -cf "$uncompressed" \
        "$fixture"
    case "$compression" in
        none)
            mv -f "$uncompressed" "$temporary"
            uncompressed=
            ;;
        gzip)
            gzip --no-name --best --stdout "$uncompressed" > "$temporary"
            ;;
        xz)
            xz --threads=1 --check=crc64 --best --stdout "$uncompressed" > "$temporary"
            ;;
        zstd)
            zstd --quiet --no-progress --single-thread -19 --check --stdout "$uncompressed" > "$temporary"
            ;;
        *)
            printf 'unsupported execution fixture compression: %s\n' "$compression" >&2
            exit 1
            ;;
    esac
    if [ -n "$uncompressed" ]; then
        rm -f "$uncompressed"
        uncompressed=
    fi
    chmod 0644 "$temporary"
    if [ "$mode" = check ]; then
        test -f "$output"
        test ! -L "$output"
        if ! cmp -s "$temporary" "$output"; then
            printf 'execution fixture archive is stale: %s\n' "$output" >&2
            exit 1
        fi
        rm -f "$temporary"
    else
        mv -f "$temporary" "$output"
    fi
    temporary=
done

raw_patch=cast-hooks-fixture-1.0.0-pre-setup.patch
source="$source_file_root/$raw_patch"
output="$archive_root/$raw_patch"
if [ "$mode" = check ]; then
    temporary="$check_root/$raw_patch"
else
    temporary="$archive_root/.$raw_patch.tmp"
fi
rm -f "$temporary"
cat "$source" > "$temporary"
chmod 0644 "$temporary"
if [ "$mode" = check ]; then
    test -f "$output"
    test ! -L "$output"
    if ! cmp -s "$temporary" "$output"; then
        printf 'execution fixture raw source is stale: %s\n' "$output" >&2
        exit 1
    fi
    rm -f "$temporary"
else
    mv -f "$temporary" "$output"
fi
temporary=

count=0
for entry in "$archive_root"/*; do
    test -e "$entry" || {
        printf 'execution fixture archive directory is empty: %s\n' "$archive_root" >&2
        exit 1
    }
    test -f "$entry" && test ! -L "$entry" || {
        printf 'unexpected non-regular execution fixture archive entry: %s\n' "$entry" >&2
        exit 1
    }
    case "$(basename "$entry")" in
        cast-autotools-fixture-1.0.0.tar|\
        cast-autotools-options-fixture-1.0.0.tar|\
        cast-cargo-features-fixture-1.0.0.tar|\
        cast-cargo-vendored-fixture-1.0.0.tar.gz|\
        cast-cargo-fixture-1.0.0.tar|\
        cast-cmake-fixture-1.0.0.tar|\
        cast-custom-fixture-1.0.0.tar|\
        cast-daemon-fixture-1.0.0.tar.zst|\
        cast-factory-override-fixture-1.0.0.tar|\
        cast-hooks-fixture-1.0.0.tar.xz|\
        cast-hooks-fixture-1.0.0-pre-setup.patch|\
        cast-meson-fixture-1.0.0.tar|\
        cast-plugin-output-fixture-1.0.0.tar|\
        cast-post-install-smoke-test-fixture-1.0.0.tar|\
        cast-split-fixture-1.0.0.tar) ;;
        *) printf 'unexpected execution fixture archive: %s\n' "$entry" >&2; exit 1 ;;
    esac
    count=$((count + 1))
done

test "$count" -eq 15 || {
    printf 'expected exactly fifteen source-backed execution fixture artifacts, found %s\n' "$count" >&2
    exit 1
}
