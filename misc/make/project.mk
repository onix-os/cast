CARGO ?= cargo
MODE ?= onboarding
PREFIX ?= $(HOME)/.local
BIN_DIR ?= $(PREFIX)/bin
DATA_DIR ?= $(PREFIX)/share
CONFIG_DIR ?= $(HOME)/.config
LICENSE_DIR ?= $(TOP_DIR)/target/license-list-data
EXAMPLE ?= read
STONE ?= $(TOP_DIR)/tests/fixtures/bash-completion-2.11-1-1-x86_64.stone
REQUIRE_EXECUTION ?= 0
FIXTURE ?= all
EXECUTION_FIXTURE_NAMES := autotools autotools-options cargo cargo-features cargo-vendored cmake custom daemon-generated desktop-integration external-test-vectors factory-override font-family generated-config generated-shell gettext-localization go-module header-only-library hooks-patch meson multiple-sources pgo-workload plugin-output post-install-smoke-test python-module relation-policy split system-integration-assets userspace-profile
VALID_EXECUTION_FIXTURES := all $(EXECUTION_FIXTURE_NAMES)
# Capture the literal command-line value once. A recursive make variable such
# as '$$(shell ...)' must never be re-expanded into a bootstrap shell recipe.
FIXTURE_SELECTION := $(strip $(value FIXTURE))
VALID_FIXTURE_SELECTION := $(if $(word 2,$(FIXTURE_SELECTION)),,$(filter $(VALID_EXECUTION_FIXTURES),$(FIXTURE_SELECTION)))
EXECUTION_REQUIREMENT := $(strip $(value REQUIRE_EXECUTION))
VALID_EXECUTION_REQUIREMENT := $(if $(word 2,$(EXECUTION_REQUIREMENT)),,$(filter 0 1,$(EXECUTION_REQUIREMENT)))
BOOTSTRAP_TMP_DIR := $(TOP_DIR)/target/bootstrap-fixtures/tmp
BOOTSTRAP_PACKAGE_STORE := $(TOP_DIR)/target/bootstrap-fixtures/packages

include misc/make/tests.mk
include misc/make/forge-focused-tests.mk
include misc/make/examples.mk
include misc/make/execution-fixtures.mk
include misc/make/database.mk
include misc/make/libstone.mk

.PHONY: cast get-started licenses check source-loc source-loc-test migrate migrate-redo libstone

cast:
	@$(CARGO) build --profile $(MODE) -p cast

get-started: cast licenses
	@set -eu; \
	echo; \
	echo "Installing cast to $(BIN_DIR)..."; \
	install -d "$(BIN_DIR)"; \
	install -m 755 "$(TOP_DIR)/target/$(MODE)/cast" "$(BIN_DIR)/cast"; \
	rm -rf "$(DATA_DIR)/cast"; \
	install -d "$(DATA_DIR)/cast/licenses" "$(CONFIG_DIR)/cast"; \
	cp -R "$(TOP_DIR)/crates/mason/data/policy" "$(DATA_DIR)/cast/"; \
	cp "$(LICENSE_DIR)/text/"* "$(DATA_DIR)/cast/licenses/"; \
	cp -R "$(TOP_DIR)/crates/mason/data/profile.d" "$(CONFIG_DIR)/cast/"; \
	echo; \
	echo "Installed files:"; \
	ls -hlF "$(BIN_DIR)/cast" "$(DATA_DIR)/cast" "$(CONFIG_DIR)/cast"; \
	echo; \
	case ":$$PATH:" in \
		*:"$(BIN_DIR)":*) echo "$(BIN_DIR) is already in PATH." ;; \
		*) echo "$(BIN_DIR) is not in PATH yet; add it before running the tools." ;; \
	esac; \
	echo; \
	echo "Cast documentation lives at https://github.com/onix-os/os-tools"

licenses:
	@"$(TOP_DIR)/misc/scripts/fetch-licenses.sh" "$(LICENSE_DIR)"

source-loc:
	@"$(SHELL)" "$(TOP_DIR)/misc/scripts/check-source-loc.sh"

source-loc-test:
	@"$(SHELL)" "$(TOP_DIR)/misc/scripts/test-check-source-loc.sh"

check: host-storage-safety-test
	@$(CARGO) check --workspace --all-targets
	@$(CARGO) check -p mason --features cache-clean-test-support \
		--test cache_clean
	@$(CARGO) check -p mason --features delegated-fixture-test-support \
		--test delegated_execution_fixture
