#!/usr/bin/dash

# Supplemental only: this checks exact staged bytes with flake-provided host
# tools. It is not a Stone/container execution, activation, daemon, device,
# authorization, boot, transaction, rollback, or Nix-compatibility proof.
set -eu

root=$(CDPATH= cd -- "$(timeout 10s dirname -- "$0")/../.." && pwd)
fixture_root="$root/tests/fixtures/gluon/execution"
archive="$fixture_root/archives/cast-system-integration-assets-fixture-1.0.0.tar"
authored="$fixture_root/source-trees/cast-system-integration-assets-fixture-1.0.0"
expected_sha256=27d04653529db216023599d2f6122f503acb59e0ebe26c5bce351dc970d58113

temporary=$(timeout 10s mktemp -d "${TMPDIR:-/tmp}/cast-system-integration-host.XXXXXXXX")
cleanup() {
    timeout 30s rm -rf "$temporary"
}
trap cleanup EXIT HUP INT TERM
timeout 10s chmod 0700 "$temporary"

actual_hash=$(timeout 10s sha256sum "$archive")
test "${actual_hash%% *}" = "$expected_sha256"

source_root="$temporary/source"
stage="$temporary/stage"
timeout 10s mkdir -p "$source_root" "$stage"
timeout 30s tar -xf "$archive" -C "$source_root"
extracted="$source_root/cast-system-integration-assets-fixture-1.0.0"

expected_inventory="$temporary/expected-inventory"
observed_inventory="$temporary/observed-inventory"
unsorted_inventory="$temporary/unsorted-inventory"
printf '%s\n' \
    'd 755 integration' \
    'f 644 LICENSE' \
    'f 644 integration/70-cast-system-integration-fixture.rules' \
    'f 644 integration/cast-system-integration-fixture.service' \
    'f 644 integration/cast-system-integration-fixture.sysusers' \
    'f 644 integration/cast-system-integration-fixture.tmpfiles' \
    'f 644 integration/io.cast.SystemIntegrationFixture.policy' \
    'f 644 integration/io.cast.SystemIntegrationFixture.rules' \
    'f 755 integration/cast-system-integration-fixture' \
    > "$expected_inventory"
timeout 10s sort "$expected_inventory" -o "$expected_inventory"
timeout 10s find "$extracted" -mindepth 1 -printf '%y %m %P\n' > "$unsorted_inventory"
timeout 10s sort "$unsorted_inventory" -o "$observed_inventory"
timeout 10s cmp "$expected_inventory" "$observed_inventory"

for relative in \
    LICENSE \
    integration/70-cast-system-integration-fixture.rules \
    integration/cast-system-integration-fixture \
    integration/cast-system-integration-fixture.service \
    integration/cast-system-integration-fixture.sysusers \
    integration/cast-system-integration-fixture.tmpfiles \
    integration/io.cast.SystemIntegrationFixture.policy \
    integration/io.cast.SystemIntegrationFixture.rules
do
    timeout 10s cmp "$authored/$relative" "$extracted/$relative"
done

timeout 10s install -Dm755 "$extracted/integration/cast-system-integration-fixture" \
    "$stage/usr/libexec/cast-system-integration-fixture"
timeout 10s install -Dm644 "$extracted/integration/cast-system-integration-fixture.service" \
    "$stage/usr/lib/systemd/system/cast-system-integration-fixture.service"
timeout 10s install -Dm644 "$extracted/integration/cast-system-integration-fixture.sysusers" \
    "$stage/usr/lib/sysusers.d/cast-system-integration-fixture.conf"
timeout 10s install -Dm644 "$extracted/integration/cast-system-integration-fixture.tmpfiles" \
    "$stage/usr/lib/tmpfiles.d/cast-system-integration-fixture.conf"
timeout 10s install -Dm644 "$extracted/integration/70-cast-system-integration-fixture.rules" \
    "$stage/usr/lib/udev/rules.d/70-cast-system-integration-fixture.rules"
timeout 10s install -Dm644 "$extracted/integration/io.cast.SystemIntegrationFixture.rules" \
    "$stage/usr/share/polkit-1/rules.d/io.cast.SystemIntegrationFixture.rules"
timeout 10s install -Dm644 "$extracted/integration/io.cast.SystemIntegrationFixture.policy" \
    "$stage/usr/share/polkit-1/actions/io.cast.SystemIntegrationFixture.policy"
timeout 10s install -Dm644 "$extracted/LICENSE" \
    "$stage/usr/share/licenses/cast-system-integration-assets-fixture/LICENSE"

timeout 10s dash "$stage/usr/libexec/cast-system-integration-fixture" --self-test
timeout 30s env SYSTEMD_UNIT_PATH=/usr/lib/systemd/system systemd-analyze \
    --root="$stage" \
    --recursive-errors=no \
    --man=no \
    --generators=no \
    verify cast-system-integration-fixture.service
timeout 30s systemd-sysusers \
    --dry-run \
    --root="$stage" \
    "$stage/usr/lib/sysusers.d/cast-system-integration-fixture.conf"
timeout 30s systemd-tmpfiles \
    --create \
    --root="$stage" \
    --graceful \
    -E \
    "$stage/usr/lib/tmpfiles.d/cast-system-integration-fixture.conf"
test -d "$stage/var/lib/cast-system-integration"
test ! -L "$stage/var/lib/cast-system-integration"
test "$(timeout 10s stat -c '%a' "$stage/var/lib/cast-system-integration")" = 750
timeout 30s udevadm verify \
    --root="$stage" \
    --resolve-names=never \
    --no-summary \
    --no-style \
    /usr/lib/udev/rules.d/70-cast-system-integration-fixture.rules
timeout 30s xmllint \
    --nonet \
    --noout \
    "$stage/usr/share/polkit-1/actions/io.cast.SystemIntegrationFixture.policy"

service="$stage/usr/lib/systemd/system/cast-system-integration-fixture.service"
sysusers="$stage/usr/lib/sysusers.d/cast-system-integration-fixture.conf"
tmpfiles="$stage/usr/lib/tmpfiles.d/cast-system-integration-fixture.conf"
udev="$stage/usr/lib/udev/rules.d/70-cast-system-integration-fixture.rules"
policy="$stage/usr/share/polkit-1/actions/io.cast.SystemIntegrationFixture.policy"
rules="$stage/usr/share/polkit-1/rules.d/io.cast.SystemIntegrationFixture.rules"
timeout 10s grep -Fqx 'ExecStart=/usr/libexec/cast-system-integration-fixture' "$service"
timeout 10s grep -Fqx 'User=cast-system-integration' "$service"
timeout 10s grep -Fqx 'Group=cast-system-integration' "$service"
timeout 10s grep -Fqx 'u cast-system-integration - "Cast system integration fixture" /var/lib/cast-system-integration -' "$sysusers"
timeout 10s grep -Fqx 'd /var/lib/cast-system-integration 0750 cast-system-integration cast-system-integration -' "$tmpfiles"
timeout 10s grep -Fq 'GROUP="cast-system-integration"' "$udev"
timeout 10s grep -Fq 'ENV{SYSTEMD_WANTS}+="cast-system-integration-fixture.service"' "$udev"
test "$(timeout 10s grep -Fc 'io.cast.SystemIntegrationFixture.manage' "$policy")" -eq 1
test "$(timeout 10s grep -Fc 'io.cast.SystemIntegrationFixture.manage' "$rules")" -eq 1
timeout 10s grep -Fq 'subject.isInGroup("cast-system-integration")' "$rules"
timeout 10s grep -Fq 'return polkit.Result.AUTH_ADMIN;' "$rules"
if timeout 10s grep -Eq 'polkit\.Result\.YES|=>|(^|[^[:alnum:]_])(const|let)[[:space:]]' "$rules"; then
    printf '%s\n' 'supplemental polkit rule contract accepted an over-broad or non-ES5 construct' >&2
    exit 1
else
    status=$?
    test "$status" -eq 1
fi

printf '%s\n' 'system-integration-assets: supplemental staged host validation passed'
