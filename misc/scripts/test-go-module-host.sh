#!/usr/bin/dash

# Supplemental only: this proves the checked-in module builds and tests twice
# with identical bytes using only its vendor tree under a hostile host
# environment. It is not a Stone, container, transaction, rollback, boot, or
# Nix-compatibility proof.
set -eu

root=$(CDPATH= cd -- "$(timeout 10s dirname -- "$0")/../.." && pwd)
fixture_root="$root/tests/fixtures/gluon/execution"
tree_name=cast-go-module-fixture-1.0.0
archive="$fixture_root/archives/$tree_name.tar.zst"
authored="$fixture_root/source-trees/$tree_name"
expected_archive_sha256=f4c4eb74304956e3f3e650d2004c78a39c4d4c009e447b9c28b3819370dbc78f
identity='cast go module fixture: vendored dependency v0.1.0: declarative userspace'

temporary=$(timeout 10s mktemp -d "${TMPDIR:-/tmp}/cast-go-module-host.XXXXXXXX")
cleanup() {
    timeout 30s chmod -R u+w "$temporary" 2>/dev/null || :
    timeout 30s rm -rf "$temporary"
}
trap cleanup EXIT HUP INT TERM
timeout 10s chmod 0700 "$temporary"

archive_hash=$(timeout 10s sha256sum "$archive")
test "${archive_hash%% *}" = "$expected_archive_sha256"
test "$(timeout 10s stat -c '%a:%s' "$archive")" = 644:2000

source_root="$temporary/source"
timeout 10s mkdir -p "$source_root"
timeout 30s tar --zstd -xf "$archive" -C "$source_root"
extracted="$source_root/$tree_name"

timeout 10s cat >"$temporary/expected-inventory" <<'EOF'
d 755 cmd
d 755 cmd/cast-go-module-fixture
d 755 internal
d 755 internal/application
d 755 vendor
d 755 vendor/fixtures.invalid
d 755 vendor/fixtures.invalid/cast
d 755 vendor/fixtures.invalid/cast/go-message
f 644 LICENSE
f 644 README.md
f 644 cmd/cast-go-module-fixture/main.go
f 644 cmd/cast-go-module-fixture/main_test.go
f 644 go.mod
f 644 go.sum
f 644 internal/application/application.go
f 644 internal/application/application_test.go
f 644 vendor/fixtures.invalid/cast/go-message/message.go
f 644 vendor/modules.txt
EOF
timeout 10s sort "$temporary/expected-inventory" -o "$temporary/expected-inventory"
timeout 10s find "$extracted" -mindepth 1 -printf '%y %m %P\n' \
    >"$temporary/observed-inventory-unsorted"
timeout 10s sort "$temporary/observed-inventory-unsorted" \
    -o "$temporary/observed-inventory"
timeout 10s cmp "$temporary/expected-inventory" "$temporary/observed-inventory"

for relative in \
    LICENSE \
    README.md \
    cmd/cast-go-module-fixture/main.go \
    cmd/cast-go-module-fixture/main_test.go \
    go.mod \
    go.sum \
    internal/application/application.go \
    internal/application/application_test.go \
    vendor/fixtures.invalid/cast/go-message/message.go \
    vendor/modules.txt
do
    timeout 10s cmp "$authored/$relative" "$extracted/$relative"
done

if timeout 10s grep -Eq '^[[:space:]]*replace([[:space:](]|$)' "$extracted/go.mod"; then
    printf '%s\n' 'go-module fixture contains a forbidden replace directive' >&2
    exit 1
else
    status=$?
    test "$status" -eq 1
fi
test ! -e "$extracted/go.work"
test ! -e "$extracted/vendor/fixtures.invalid/cast/go-message/go.mod"

go_host() {
    home=$1
    cache=$2
    modules=$3
    shift 3
    timeout 120s env -i \
        PATH="$PATH" \
        HOME="$home" \
        GOCACHE="$cache" \
        GOMODCACHE="$modules" \
        GOENV=off \
        GOWORK=off \
        GOTOOLCHAIN=local \
        GOPROXY=off \
        GOSUMDB=off \
        GONOSUMDB='*' \
        GONOPROXY='*' \
        GOFLAGS= \
        GO111MODULE=on \
        CGO_ENABLED=0 \
        GOOS=linux \
        GOARCH=amd64 \
        GOAMD64=v1 \
        LANG=C \
        LC_ALL=C \
        TZ=Pacific/Kiritimati \
        SOURCE_DATE_EPOCH=1 \
        "$@"
}

for name in a b; do
    build="$temporary/build-$name"
    home="$temporary/home-$name"
    cache="$temporary/cache-$name"
    modules="$temporary/modules-$name"
    timeout 10s mkdir -p "$build" "$home" "$cache" "$modules"
    timeout 30s cp -R "$extracted/." "$build/"
    (
        cd "$build"
        go_host "$home" "$cache" "$modules" \
            go test -mod=vendor -trimpath -count=1 ./...
        go_host "$home" "$cache" "$modules" \
            go build -mod=vendor -trimpath -buildvcs=false \
            -ldflags='-buildid= -s -w' \
            -o "$temporary/cast-go-module-fixture-$name" \
            ./cmd/cast-go-module-fixture
    )
    output=$(timeout 30s "$temporary/cast-go-module-fixture-$name" --self-test)
    test "$output" = "$identity"
    if timeout 10s find "$modules" -type f -print -quit | timeout 10s grep -q .; then
        printf '%s\n' 'go-module fixture populated the module download cache' >&2
        exit 1
    else
        status=$?
        test "$status" -eq 1
    fi
done
timeout 10s cmp "$temporary/cast-go-module-fixture-a" \
    "$temporary/cast-go-module-fixture-b"

timeout 10s mkdir -p "$temporary/tampered" "$temporary/tampered-home" \
    "$temporary/tampered-cache" "$temporary/tampered-modules"
timeout 30s cp -R "$extracted/." "$temporary/tampered/"
timeout 10s rm "$temporary/tampered/vendor/fixtures.invalid/cast/go-message/message.go"
if (
    cd "$temporary/tampered"
    go_host "$temporary/tampered-home" "$temporary/tampered-cache" \
        "$temporary/tampered-modules" \
        go build -mod=vendor -trimpath -buildvcs=false \
        -ldflags='-buildid= -s -w' -o "$temporary/forbidden" \
        ./cmd/cast-go-module-fixture
); then
    printf '%s\n' 'go-module fixture built after its vendored dependency was removed' >&2
    exit 1
fi
test ! -e "$temporary/forbidden"

printf '%s\n' 'go-module: supplemental deterministic hostile-host validation passed'
