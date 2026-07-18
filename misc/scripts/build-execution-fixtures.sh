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
git_source_root="$fixture_root/git-source-trees"
archive_root="$fixture_root/archives"
git_bundle_root="$fixture_root/git-bundles"
font_builder="$root/misc/scripts/build-font-family-fixture-fonts.sh"
temporary=
uncompressed=
check_root=
git_work=

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
    if [ -n "$git_work" ]; then
        timeout 30s rm -rf "$git_work"
    fi
}

trap cleanup EXIT HUP INT TERM

timeout 30s mkdir -p "$archive_root" "$git_bundle_root"
test -f "$font_builder" && test ! -L "$font_builder" && test -x "$font_builder" || {
    printf 'font fixture generator is unavailable or unsafe: %s\n' "$font_builder" >&2
    exit 1
}
timeout 120s dash "$font_builder" --check

package_count=0
source_less_count=0
for entry in "$package_root"/*; do
    test -d "$entry" && test ! -L "$entry" || {
        printf 'unexpected non-directory execution fixture package entry: %s\n' "$entry" >&2
        exit 1
    }
    fixture=$(basename "$entry")
    case "$fixture" in
        autotools|autotools-options|cargo|cargo-features|cargo-vendored|cmake|custom|daemon-generated|desktop-integration|factory-override|font-family|generated-config|generated-shell|gettext-localization|go-module|header-only-library|hooks-patch|meson|multiple-sources|plugin-output|post-install-smoke-test|python-module|split|system-integration-assets) ;;
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

test "$package_count" -eq 24 || {
    printf 'expected exactly twenty-four source-matrix package directories, found %s\n' "$package_count" >&2
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
        cast-desktop-integration-fixture-1.0.0|\
        cast-factory-override-fixture-1.0.0|\
        cast-font-family-fixture-1.0.0|\
        cast-gettext-localization-fixture-1.0.0|\
        cast-go-module-fixture-1.0.0|\
        cast-header-only-library-fixture-1.0.0|\
        cast-hooks-fixture-1.0.0|\
        cast-meson-fixture-1.0.0|\
        cast-multiple-sources-fixture-1.0.0|\
        cast-plugin-output-fixture-1.0.0|\
        cast-post-install-smoke-test-fixture-1.0.0|\
        cast-python-module-fixture-1.0.0|\
        cast-split-fixture-1.0.0|\
        cast-system-integration-assets-fixture-1.0.0) ;;
        *) printf 'unexpected execution source tree: %s\n' "$entry" >&2; exit 1 ;;
    esac
    source_tree_count=$((source_tree_count + 1))
done

test "$source_tree_count" -eq 22 || {
    printf 'expected exactly twenty-two archive-backed execution fixture trees, found %s\n' "$source_tree_count" >&2
    exit 1
}

source_file_count=0
for entry in "$source_file_root"/*; do
    test -f "$entry" && test ! -L "$entry" || {
        printf 'unexpected non-regular execution source-file entry: %s\n' "$entry" >&2
        exit 1
    }
    case "$(basename "$entry")" in
        cast-hooks-fixture-1.0.0-pre-setup.patch|\
        cast-multiple-sources-schema-1.0.0.h) ;;
        *) printf 'unexpected execution source file: %s\n' "$entry" >&2; exit 1 ;;
    esac
    source_file_count=$((source_file_count + 1))
done

test "$source_file_count" -eq 2 || {
    printf 'expected exactly two independent execution source files, found %s\n' "$source_file_count" >&2
    exit 1
}

git_source_tree_count=0
for entry in "$git_source_root"/*; do
    test -d "$entry" && test ! -L "$entry" || {
        printf 'unexpected non-directory execution Git source-tree entry: %s\n' "$entry" >&2
        exit 1
    }
    case "$(basename "$entry")" in
        cast-multiple-sources-protocol-1.0.0) ;;
        *) printf 'unexpected execution Git source tree: %s\n' "$entry" >&2; exit 1 ;;
    esac
    git_source_tree_count=$((git_source_tree_count + 1))
done

test "$git_source_tree_count" -eq 1 || {
    printf 'expected exactly one Git-backed execution fixture tree, found %s\n' "$git_source_tree_count" >&2
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
    cast-desktop-integration-fixture-1.0.0 \
    cast-factory-override-fixture-1.0.0 \
    cast-font-family-fixture-1.0.0 \
    cast-gettext-localization-fixture-1.0.0 \
    cast-go-module-fixture-1.0.0 \
    cast-header-only-library-fixture-1.0.0 \
    cast-hooks-fixture-1.0.0 \
    cast-meson-fixture-1.0.0 \
    cast-multiple-sources-fixture-1.0.0 \
    cast-plugin-output-fixture-1.0.0 \
    cast-post-install-smoke-test-fixture-1.0.0 \
    cast-python-module-fixture-1.0.0 \
    cast-split-fixture-1.0.0 \
    cast-system-integration-assets-fixture-1.0.0
do
    source="$source_root/$fixture"
    case "$fixture" in
        cast-cargo-vendored-fixture-1.0.0)
            suffix=tar.gz
            compression=gzip
            ;;
        cast-hooks-fixture-1.0.0|cast-multiple-sources-fixture-1.0.0)
            suffix=tar.xz
            compression=xz
            ;;
        cast-daemon-fixture-1.0.0|cast-go-module-fixture-1.0.0)
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

for raw_source in \
    cast-hooks-fixture-1.0.0-pre-setup.patch \
    cast-multiple-sources-schema-1.0.0.h
do
    source="$source_file_root/$raw_source"
    output="$archive_root/$raw_source"
    if [ "$mode" = check ]; then
        temporary="$check_root/$raw_source"
    else
        temporary="$archive_root/.$raw_source.tmp"
    fi
    timeout 30s rm -f "$temporary"
    timeout 30s cat "$source" > "$temporary"
    timeout 30s chmod 0644 "$temporary"
    if [ "$mode" = check ]; then
        test -f "$output"
        test ! -L "$output"
        if ! timeout 30s cmp -s "$temporary" "$output"; then
            printf 'execution fixture raw source is stale: %s\n' "$output" >&2
            exit 1
        fi
        timeout 30s rm -f "$temporary"
    else
        timeout 30s mv -f "$temporary" "$output"
    fi
    temporary=
done

git_fixture=cast-multiple-sources-protocol-1.0.0
git_source="$git_source_root/$git_fixture"
git_bundle=cast-multiple-sources-protocol-1.0.0.bundle
git_output="$git_bundle_root/$git_bundle"
git_work=$(timeout 30s mktemp -d "${TMPDIR:-/tmp}/cast-execution-git.XXXXXX")
git_repository="$git_work/repository"
git_home="$git_work/home"
git_xdg="$git_work/xdg"
git_validation="$git_work/validation"
timeout 30s mkdir -p "$git_repository" "$git_home" "$git_xdg"

if ! timeout 30s find "$git_source" -name .git -print -quit > "$git_validation"; then
    printf 'could not validate execution Git fixture administration entries: %s\n' "$git_source" >&2
    exit 1
fi
test ! -s "$git_validation" || {
    printf 'execution Git fixture source tree contains a forbidden .git entry: %s\n' "$git_source" >&2
    exit 1
}
if ! timeout 30s find "$git_source" -type l -print -quit > "$git_validation"; then
    printf 'could not validate execution Git fixture symlinks: %s\n' "$git_source" >&2
    exit 1
fi
test ! -s "$git_validation" || {
    printf 'execution Git fixture source tree contains a forbidden symlink: %s\n' "$git_source" >&2
    exit 1
}
if ! timeout 30s find "$git_source" ! -type d ! -type f ! -type l -print -quit > "$git_validation"; then
    printf 'could not validate execution Git fixture inode kinds: %s\n' "$git_source" >&2
    exit 1
fi
test ! -s "$git_validation" || {
    printf 'execution Git fixture source tree contains a forbidden special file: %s\n' "$git_source" >&2
    exit 1
}
if ! timeout 30s find "$git_source" -type f -links +1 -print -quit > "$git_validation"; then
    printf 'could not validate execution Git fixture hardlinks: %s\n' "$git_source" >&2
    exit 1
fi
test ! -s "$git_validation" || {
    printf 'execution Git fixture source tree contains a forbidden multiply-linked file: %s\n' "$git_source" >&2
    exit 1
}

timeout 30s cp -R "$git_source/." "$git_repository/"
timeout 30s find "$git_repository" -type d -exec timeout 30s chmod 0755 {} +
timeout 30s find "$git_repository" -type f -exec timeout 30s chmod 0644 {} +

fixture_git() {
    timeout 30s env -i \
        PATH="$PATH" \
        HOME="$git_home" \
        XDG_CONFIG_HOME="$git_xdg" \
        LC_ALL=C \
        TZ=UTC \
        GIT_CONFIG_NOSYSTEM=1 \
        GIT_CONFIG_SYSTEM=/dev/null \
        GIT_CONFIG_GLOBAL=/dev/null \
        GIT_DEFAULT_HASH=sha1 \
        GIT_NO_REPLACE_OBJECTS=1 \
        GIT_TERMINAL_PROMPT=0 \
        GIT_AUTHOR_NAME="Cast Fixture" \
        GIT_AUTHOR_EMAIL="cast-fixture@fixtures.invalid" \
        GIT_AUTHOR_DATE="2023-11-14T22:13:20Z" \
        GIT_COMMITTER_NAME="Cast Fixture" \
        GIT_COMMITTER_EMAIL="cast-fixture@fixtures.invalid" \
        GIT_COMMITTER_DATE="2023-11-14T22:13:20Z" \
        git \
            -c core.hooksPath=/dev/null \
            -c commit.gpgSign=false \
            -c tag.gpgSign=false \
            "$@"
}

fixture_git -C "$git_repository" init \
    --quiet \
    --initial-branch=main \
    --object-format=sha1
fixture_git -C "$git_repository" config core.autocrlf false
fixture_git -C "$git_repository" config core.fileMode true
fixture_git -C "$git_repository" config core.ignoreCase false
fixture_git -C "$git_repository" config core.precomposeUnicode false
fixture_git -C "$git_repository" config core.symlinks true
fixture_git -C "$git_repository" config user.name "Cast Fixture"
fixture_git -C "$git_repository" config user.email "cast-fixture@fixtures.invalid"
fixture_git -C "$git_repository" add --all --force
fixture_git -C "$git_repository" commit \
    --quiet \
    --no-gpg-sign \
    -m "cast multiple sources protocol fixture"
git_commit=$(fixture_git -C "$git_repository" rev-parse HEAD)
test "$git_commit" = 4f124a6f438b061a836e332d67e803a69a7bf2d3 || {
    printf 'execution Git fixture commit drifted: %s\n' "$git_commit" >&2
    exit 1
}
if [ "$mode" = check ]; then
    temporary="$check_root/$git_bundle"
else
    temporary="$git_bundle_root/.$git_bundle.tmp"
fi
timeout 30s rm -f "$temporary"
fixture_git -C "$git_repository" \
    -c pack.threads=1 \
    -c pack.window=0 \
    -c pack.depth=0 \
    -c core.compression=0 \
    bundle create "$temporary" refs/heads/main
fixture_git -C "$git_repository" bundle verify "$temporary" >/dev/null 2>&1
timeout 30s chmod 0644 "$temporary"
if [ "$mode" = check ]; then
    test -f "$git_output"
    test ! -L "$git_output"
    if ! timeout 30s cmp -s "$temporary" "$git_output"; then
        printf 'execution fixture Git bundle is stale: %s\n' "$git_output" >&2
        exit 1
    fi
    timeout 30s rm -f "$temporary"
else
    timeout 30s mv -f "$temporary" "$git_output"
fi
temporary=
timeout 30s rm -rf "$git_work"
git_work=

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
        cast-desktop-integration-fixture-1.0.0.tar|\
        cast-factory-override-fixture-1.0.0.tar|\
        cast-font-family-fixture-1.0.0.tar|\
        cast-gettext-localization-fixture-1.0.0.tar|\
        cast-go-module-fixture-1.0.0.tar.zst|\
        cast-header-only-library-fixture-1.0.0.tar|\
        cast-hooks-fixture-1.0.0.tar.xz|\
        cast-hooks-fixture-1.0.0-pre-setup.patch|\
        cast-meson-fixture-1.0.0.tar|\
        cast-multiple-sources-fixture-1.0.0.tar.xz|\
        cast-multiple-sources-schema-1.0.0.h|\
        cast-plugin-output-fixture-1.0.0.tar|\
        cast-post-install-smoke-test-fixture-1.0.0.tar|\
        cast-python-module-fixture-1.0.0.tar|\
        cast-split-fixture-1.0.0.tar|\
        cast-system-integration-assets-fixture-1.0.0.tar) ;;
        *) printf 'unexpected execution fixture archive: %s\n' "$entry" >&2; exit 1 ;;
    esac
    count=$((count + 1))
done

test "$count" -eq 24 || {
    printf 'expected exactly twenty-four archive/raw execution fixture artifacts, found %s\n' "$count" >&2
    exit 1
}

git_bundle_count=0
for entry in "$git_bundle_root"/*; do
    test -f "$entry" && test ! -L "$entry" || {
        printf 'unexpected non-regular execution Git bundle entry: %s\n' "$entry" >&2
        exit 1
    }
    case "$(basename "$entry")" in
        cast-multiple-sources-protocol-1.0.0.bundle) ;;
        *) printf 'unexpected execution fixture Git bundle: %s\n' "$entry" >&2; exit 1 ;;
    esac
    git_bundle_count=$((git_bundle_count + 1))
done

test "$git_bundle_count" -eq 1 || {
    printf 'expected exactly one execution fixture Git bundle, found %s\n' "$git_bundle_count" >&2
    exit 1
}
