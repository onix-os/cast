.PHONY: userspace-profile-declaration-test

USERSPACE_PROFILE_FIXTURE_DIR := $(TOP_DIR)/tests/fixtures/gluon/userspace-profile
USERSPACE_PROFILE_RECIPE := $(USERSPACE_PROFILE_FIXTURE_DIR)/stone.glu
USERSPACE_PROFILE_CAST := $(TOP_DIR)/target/$(MODE)/cast

# This is deliberately named a declaration test: it proves the authored
# source-less closure without pretending that typechecking emitted a .stone.
# The contentful execution matrix must own the later build/reproduction proof.
userspace-profile-declaration-test: cast
	@set -eu; \
	root="$$(timeout 10s mktemp -d "$(TOP_DIR)/target/userspace-profile-declaration.XXXXXXXXXXXX")"; \
	trap 'timeout 10s rm -rf -- "$$root"' EXIT HUP INT TERM; \
	timeout 10s install -d -m 700 \
		"$$root/cache" "$$root/config" "$$root/data" "$$root/forge"; \
	timeout 10s sha256sum \
		"$(USERSPACE_PROFILE_FIXTURE_DIR)/package_set.glu" \
		"$(USERSPACE_PROFILE_FIXTURE_DIR)/roles.glu" \
		"$(USERSPACE_PROFILE_RECIPE)" >"$$root/authored.before"; \
	common_args="--build-cache-dir $$root/cache --config-dir $$root/config --data-dir $$root/data --resolver-root $$root/forge"; \
	timeout 30s "$(USERSPACE_PROFILE_CAST)" $$common_args recipe check \
		"$(USERSPACE_PROFILE_RECIPE)"; \
	timeout 30s "$(USERSPACE_PROFILE_CAST)" $$common_args recipe eval \
		"$(USERSPACE_PROFILE_RECIPE)" >"$$root/first.eval"; \
	timeout 30s "$(USERSPACE_PROFILE_CAST)" $$common_args recipe eval \
		"$(USERSPACE_PROFILE_RECIPE)" >"$$root/second.eval"; \
	timeout 10s cmp -s "$$root/first.eval" "$$root/second.eval"; \
	timeout 10s grep -Fqx '    sources: [],' "$$root/first.eval"; \
	timeout 10s test "$$(timeout 10s grep -Fxc '                steps: [],' "$$root/first.eval")" -eq 5; \
	timeout 10s test "$$(timeout 10s grep -Fc '                        name: "bash",' "$$root/first.eval")" -eq 1; \
	timeout 10s test "$$(timeout 10s grep -Fc '                        name: "uutils-coreutils",' "$$root/first.eval")" -eq 1; \
	timeout 10s test "$$(timeout 10s grep -Fc '                        name: "findutils",' "$$root/first.eval")" -eq 1; \
	timeout 10s test "$$(timeout 10s grep -Fc '                        name: "ca-certificates",' "$$root/first.eval")" -eq 1; \
	timeout 10s test "$$(timeout 10s grep -Fc '                        name: "xz",' "$$root/first.eval")" -eq 1; \
	timeout 10s test "$$(timeout 10s find "$(USERSPACE_PROFILE_FIXTURE_DIR)" -maxdepth 1 -type f \
		\( -name '*.yaml' -o -name '*.yml' -o -name '*.kdl' -o -name 'sources.lock.glu' -o -name 'build.lock.glu' \) \
		-print | timeout 10s wc -l)" -eq 0; \
	timeout 10s sha256sum \
		"$(USERSPACE_PROFILE_FIXTURE_DIR)/package_set.glu" \
		"$(USERSPACE_PROFILE_FIXTURE_DIR)/roles.glu" \
		"$(USERSPACE_PROFILE_RECIPE)" >"$$root/authored.after"; \
	timeout 10s cmp -s "$$root/authored.before" "$$root/authored.after"; \
	timeout 10s echo 'userspace-profile: source-less Gluon declaration is deterministic and unchanged'
