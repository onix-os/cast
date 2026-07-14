#!/bin/sh
# SPDX-FileCopyrightText: 2026 AerynOS Developers
# SPDX-License-Identifier: MPL-2.0

set -eu

mode=write
case "${1-}" in
    '') ;;
    --check) mode=check ;;
    *) printf 'usage: %s [--check]\n' "$0" >&2; exit 2 ;;
esac

root=$(CDPATH= cd -- "$(dirname -- "$0")/../.." && pwd)
fixture_root="$root/tests/fixtures/gluon/execution"
source_root="$fixture_root/source-trees"
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

if [ "$mode" = check ]; then
    check_root=$(mktemp -d "${TMPDIR:-/tmp}/cast-execution-fixtures.XXXXXX")
fi

for fixture in \
    cast-autotools-fixture-1.0.0 \
    cast-cargo-vendored-fixture-1.0.0 \
    cast-cargo-fixture-1.0.0 \
    cast-cmake-fixture-1.0.0 \
    cast-custom-fixture-1.0.0 \
    cast-daemon-fixture-1.0.0 \
    cast-factory-override-fixture-1.0.0 \
    cast-hooks-fixture-1.0.0 \
    cast-meson-fixture-1.0.0 \
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
        cast-cargo-vendored-fixture-1.0.0.tar.gz|\
        cast-cargo-fixture-1.0.0.tar|\
        cast-cmake-fixture-1.0.0.tar|\
        cast-custom-fixture-1.0.0.tar|\
        cast-daemon-fixture-1.0.0.tar.zst|\
        cast-factory-override-fixture-1.0.0.tar|\
        cast-hooks-fixture-1.0.0.tar.xz|\
        cast-meson-fixture-1.0.0.tar|\
        cast-split-fixture-1.0.0.tar) ;;
        *) printf 'unexpected execution fixture archive: %s\n' "$entry" >&2; exit 1 ;;
    esac
    count=$((count + 1))
done

test "$count" -eq 10 || {
    printf 'expected exactly ten execution fixture archives, found %s\n' "$count" >&2
    exit 1
}
