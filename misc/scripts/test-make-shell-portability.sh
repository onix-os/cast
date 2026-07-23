#!/bin/sh

set -eu

root=$(CDPATH= cd -- "$(timeout 10s dirname -- "$0")/../.." && pwd -P)
work=$(timeout 10s mktemp -d "${TMPDIR:-/tmp}/cast-make-shell-test.XXXXXXXXXXXX")
cleanup() {
    timeout 10s rm -rf -- "$work"
}
trap cleanup EXIT HUP INT TERM

shell_assignment_pattern='^[[:space:]]*((override|export|private)[[:space:]]+)*SHELL[[:space:]]*[?:+!]*=.*$'
set +e
root_assignments=$(timeout 10s rg -n "$shell_assignment_pattern" "$root/Makefile")
root_assignment_status=$?
set -e
if test "$root_assignment_status" -ne 0 \
    || test "$root_assignments" != '1:SHELL := bash'; then
    printf 'root Makefile must contain exactly one canonical SHELL := bash assignment:\n%s\n' \
        "${root_assignments:-<none>}" >&2
    exit 1
fi

set +e
fragment_assignments=$(timeout 10s rg -n "$shell_assignment_pattern" \
    "$root/misc/make" --glob '*.mk')
fragment_assignment_status=$?
set -e
case "$fragment_assignment_status" in
    1) ;;
    0)
        printf 'included Make fragments must inherit the root shell assignment:\n%s\n' \
            "$fragment_assignments" >&2
        exit 1
        ;;
    *) exit "$fragment_assignment_status" ;;
esac

set +e
hardcoded_bash=$(timeout 10s rg -n -F '/bin/bash' \
    "$root/Makefile" "$root/misc/make" --glob '*.mk')
hardcoded_bash_status=$?
set -e
case "$hardcoded_bash_status" in
    1) ;;
    0)
        printf 'Make inputs must not hardcode /bin/bash:\n%s\n' \
            "$hardcoded_bash" >&2
        exit 1
        ;;
    *) exit "$hardcoded_bash_status" ;;
esac

bare_bash_pattern='^\t(?:[[:space:]@+\-]*bash|.*[[:space:];&|]+bash)(?=[[:space:];&|]|$)'
set +e
bare_bash_commands=$(timeout 10s rg --pcre2 -n "$bare_bash_pattern" \
    "$root/Makefile" "$root/misc/make" --glob '*.mk')
bare_bash_status=$?
set -e
case "$bare_bash_status" in
    1) ;;
    0)
        printf 'Make recipes must invoke quoted $(SHELL), not a bare Bash command:\n%s\n' \
            "$bare_bash_commands" >&2
        exit 1
        ;;
    *) exit "$bare_bash_status" ;;
esac

require_exact_recipe() {
    recipe_file=$1
    expected_recipe=$2
    if ! timeout 10s grep -Fqx -- "$expected_recipe" "$recipe_file"; then
        printf 'Make helper must use the canonical quoted shell invocation: %s\n' \
            "$expected_recipe" >&2
        exit 1
    fi
}

require_exact_recipe "$root/Makefile" \
    '	@timeout 120s "$(SHELL)" "$(TOP_DIR)/misc/scripts/check-source-loc.sh"'
require_exact_recipe "$root/Makefile" \
    '	@timeout 120s "$(SHELL)" "$(TOP_DIR)/misc/scripts/test-check-source-loc.sh"'
require_exact_recipe "$root/misc/make/python-module-fixture-tests.mk" \
    '	@timeout 300s "$(SHELL)" "$(TOP_DIR)/misc/scripts/test-python-module-host.sh"'
require_exact_recipe "$root/misc/make/gettext-localization-fixture-tests.mk" \
    '	@timeout 180s "$(SHELL)" "$(TOP_DIR)/misc/scripts/test-gettext-localization-host.sh"'
require_exact_recipe "$root/misc/make/multiple-sources-fixture-tests.mk" \
    '	@timeout 180s "$(SHELL)" "$(TOP_DIR)/misc/scripts/test-multiple-sources-compilers.sh"'

real_bash=$(command -v bash)
case "$real_bash" in
    /*) ;;
    *) printf 'test Bash must resolve to an absolute path: %s\n' "$real_bash" >&2; exit 1 ;;
esac
if test ! -f "$real_bash" || test ! -x "$real_bash"; then
    printf 'test Bash is unavailable or unsafe: %s\n' "$real_bash" >&2
    exit 1
fi

fakebin="$work/bin"
probe="$work/probe.mk"
receipts="$work/receipts"
basename_marker="$work/basename-shells"
override_marker="$work/override-shells"
timeout 10s mkdir -p -- "$fakebin"

timeout 10s cat >"$fakebin/bash" <<'EOF'
#!/bin/sh
set -eu
: "${MAKE_SHELL_REAL_BASH:?}"
: "${MAKE_SHELL_BASENAME_MARKER:?}"
printf 'basename\n' >>"$MAKE_SHELL_BASENAME_MARKER"
exec "$MAKE_SHELL_REAL_BASH" "$@"
EOF
timeout 10s cat >"$work/override-bash" <<'EOF'
#!/bin/sh
set -eu
: "${MAKE_SHELL_REAL_BASH:?}"
: "${MAKE_SHELL_OVERRIDE_MARKER:?}"
printf 'override\n' >>"$MAKE_SHELL_OVERRIDE_MARKER"
exec "$MAKE_SHELL_REAL_BASH" "$@"
EOF
timeout 10s chmod 755 "$fakebin/bash" "$work/override-bash"

timeout 10s cat >"$probe" <<'EOF'
SHELL := bash

.PHONY: outer inner

outer:
	@printf 'outer\n' >>"$(RECEIPTS)"
	@$(MAKE) --no-print-directory -f "$(PROBE)" inner

inner:
	@printf 'inner\n' >>"$(RECEIPTS)"
EOF

timeout 30s env \
    PATH="$fakebin:$PATH" \
    MAKE_SHELL_REAL_BASH="$real_bash" \
    MAKE_SHELL_BASENAME_MARKER="$basename_marker" \
    MAKE_SHELL_OVERRIDE_MARKER="$override_marker" \
    make --no-print-directory -f "$probe" outer \
    PROBE="$probe" RECEIPTS="$receipts"
if test "$(timeout 10s cat "$receipts")" != "$(printf 'outer\ninner')" \
    || test ! -s "$basename_marker" || test -e "$override_marker"; then
    printf 'SHELL := bash did not resolve recursively through PATH\n' >&2
    exit 1
fi

timeout 10s rm -f -- "$receipts" "$basename_marker" "$override_marker"
timeout 30s env \
    PATH="$fakebin:$PATH" \
    MAKE_SHELL_REAL_BASH="$real_bash" \
    MAKE_SHELL_BASENAME_MARKER="$basename_marker" \
    MAKE_SHELL_OVERRIDE_MARKER="$override_marker" \
    make --no-print-directory -f "$probe" \
    SHELL="$work/override-bash" outer PROBE="$probe" RECEIPTS="$receipts"
if test "$(timeout 10s cat "$receipts")" != "$(printf 'outer\ninner')" \
    || test -e "$basename_marker" || test ! -s "$override_marker"; then
    printf 'command-line SHELL override did not propagate to recursive Make\n' >&2
    exit 1
fi

printf 'Make shell portability tests passed\n'
