SHELL := bash

TOP_DIR := $(CURDIR)
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

.DEFAULT_GOAL := cast

include misc/make/tests.mk
include misc/make/help.mk

.PHONY: build cast get-started licenses fix lint test check fmt clean \
	binary-layout product-names config-formats config-formats-test \
	make-shell-portability-test source-loc source-loc-test migrate migrate-redo libstone

build:
	@$(CARGO) build --workspace

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

fix:
	@echo "Applying clippy fixes..."
	@$(CARGO) clippy --fix --allow-dirty --allow-staged --workspace -- --no-deps
	@echo "Applying cargo fmt..."
	@$(CARGO) fmt --all
	@echo "Fixing typos..."
	@typos -w --exclude target/license-list-data/

lint: binary-layout product-names config-formats
	@echo "Running clippy..."
	@$(CARGO) clippy --workspace -- --no-deps
	@echo "Running clippy on the feature-gated harness-free cache-clean proof..."
	@$(CARGO) clippy -p mason --features cache-clean-test-support \
		--test cache_clean -- --no-deps
	@echo "Running clippy on the feature-gated harness-free Mason fixture..."
	@$(CARGO) clippy -p mason --features delegated-fixture-test-support \
		--test delegated_execution_fixture -- --no-deps
	@echo "Running cargo fmt..."
	@$(CARGO) fmt --all -- --check
	@echo "Checking for typos..."
	@typos --exclude target/license-list-data/

config-formats:
	@"$(TOP_DIR)/misc/scripts/check-config-formats.sh"

config-formats-test:
	@"$(TOP_DIR)/misc/scripts/test-check-config-formats.sh"

binary-layout:
	@"$(TOP_DIR)/misc/scripts/check-binary-layout.sh"

product-names:
	@"$(TOP_DIR)/misc/scripts/check-product-names.sh"

make-shell-portability-test:
	@timeout 120s "$(TOP_DIR)/misc/scripts/test-make-shell-portability.sh"

source-loc:
	@timeout 120s "$(SHELL)" "$(TOP_DIR)/misc/scripts/check-source-loc.sh"

source-loc-test:
	@timeout 120s "$(SHELL)" "$(TOP_DIR)/misc/scripts/test-check-source-loc.sh"

# Container activation uses fork-like namespace creation. Keep each libtest
# process to one active test worker; production single-task behavior is proved
# separately by harness-free container and delegated Mason integration targets.
test: host-storage-safety-test lint config-formats-test examples-gate-test delegated-fixture-runner-test cache-clean-test execution-capability-preflight-test mason-generated-routing-test mason-elf-debug-route-test
	@echo "Running tests in all packages..."
	@$(CARGO) test --all --no-fail-fast -- --test-threads=1

include misc/make/forge-focused-tests.mk
include misc/make/examples.mk
include misc/make/execution-fixtures.mk

check: host-storage-safety-test make-shell-portability-test
	@$(CARGO) check --workspace --all-targets
	@$(CARGO) check -p mason --features cache-clean-test-support \
		--test cache_clean
	@$(CARGO) check -p mason --features delegated-fixture-test-support \
		--test delegated_execution_fixture

fmt:
	@$(CARGO) fmt --all

clean:
	@$(CARGO) clean

include misc/make/database.mk
include misc/make/libstone.mk
