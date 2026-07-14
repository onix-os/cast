# SPDX-FileCopyrightText: 2026 AerynOS Developers
# SPDX-License-Identifier: MPL-2.0

SHELL := /bin/bash

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

.DEFAULT_GOAL := cast

.PHONY: build cast get-started licenses fix lint test examples execution-fixtures bootstrap-fixtures fixtures-ci fixture-sources fixture-sources-check check fmt clean \
	binary-layout product-names config-formats config-formats-test migrate migrate-redo \
	libstone help

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

test: lint config-formats-test
	@echo "Running tests in all packages..."
	@$(CARGO) test --all --no-fail-fast

examples:
	@echo "Checking every Gluon package example through the public Cast CLI..."
	@$(CARGO) test -p cast --test gluon_examples -- --list | \
		grep -Fqx 'every_gluon_package_example_passes_the_public_cast_cli: test'
	@$(CARGO) test -p cast --test gluon_examples \
		every_gluon_package_example_passes_the_public_cast_cli -- \
		--exact --nocapture
	@echo "Freezing every Gluon package example through the hermetic planner..."
	@$(CARGO) test -p mason --lib -- --list | \
		grep -Fqx 'planner::hermetic_tests::checked_in_package_examples_freeze_hermetically_and_reuse_exact_build_locks: test'
	@$(CARGO) test -p mason --lib \
		planner::hermetic_tests::checked_in_package_examples_freeze_hermetically_and_reuse_exact_build_locks -- \
		--exact --nocapture
	@echo "Proving metadata-only providers fail before frozen execution..."
	@$(CARGO) test -p mason --lib -- --list | \
		grep -Fqx 'planner::hermetic_tests::checked_in_metadata_only_example_fails_closed_before_execution: test'
	@$(CARGO) test -p mason --lib \
		planner::hermetic_tests::checked_in_metadata_only_example_fails_closed_before_execution -- \
		--exact --nocapture

fixture-sources:
	@"$(TOP_DIR)/misc/scripts/build-execution-fixtures.sh"

fixture-sources-check:
	@"$(TOP_DIR)/misc/scripts/build-execution-fixtures.sh" --check

execution-fixtures: fixture-sources-check
	@echo "Checking locked offline execution-source fixtures..."
	@$(CARGO) test -p mason --lib \
		planner::hermetic_tests::offline_execution_fixture_archives_are_real_locked_and_complete -- \
		--exact --list | \
		grep -Fqx 'planner::hermetic_tests::offline_execution_fixture_archives_are_real_locked_and_complete: test'
	@$(CARGO) test -p mason --lib \
		planner::hermetic_tests::offline_execution_fixture_archives_are_real_locked_and_complete -- \
		--exact --nocapture
	@echo "Checking the declarative pinned Stone bootstrap manifest and index..."
	@$(CARGO) test -p mason --lib \
		planner::hermetic_tests::bootstrap::pinned_bootstrap_manifest_is_bounded_and_index_authoritative -- \
		--exact --list | \
		grep -Fqx 'planner::hermetic_tests::bootstrap::pinned_bootstrap_manifest_is_bounded_and_index_authoritative: test'
	@$(CARGO) test -p mason --lib \
		planner::hermetic_tests::bootstrap::pinned_bootstrap_manifest_is_bounded_and_index_authoritative -- \
		--exact --nocapture
	@echo "Resolving all six execution fixtures against the pinned real Stone index..."
	@$(CARGO) test -p mason --lib \
		planner::hermetic_tests::bootstrap::all_execution_fixtures_resolve_exactly_the_pinned_real_stone_closure -- \
		--exact --list | \
		grep -Fqx 'planner::hermetic_tests::bootstrap::all_execution_fixtures_resolve_exactly_the_pinned_real_stone_closure: test'
	@$(CARGO) test -p mason --lib \
		planner::hermetic_tests::bootstrap::all_execution_fixtures_resolve_exactly_the_pinned_real_stone_closure -- \
		--exact --nocapture

bootstrap-fixtures:
	@echo "Fetching and verifying the exact contentful Stone bootstrap closure..."
	@$(CARGO) test -p mason --lib \
		planner::hermetic_tests::bootstrap::fetch_pinned_bootstrap_package_files -- \
		--ignored --exact --list | \
		grep -Fqx 'planner::hermetic_tests::bootstrap::fetch_pinned_bootstrap_package_files: test'
	@$(CARGO) test -p mason --lib \
		planner::hermetic_tests::bootstrap::fetch_pinned_bootstrap_package_files -- \
		--ignored --exact --nocapture
	@echo "Materializing the complete closure as a production-format offline root mirror..."
	@$(CARGO) test -p mason --lib \
		planner::hermetic_tests::bootstrap::contentful_bootstrap_materializes_a_complete_offline_root_mirror -- \
		--ignored --exact --list | \
		grep -Fqx 'planner::hermetic_tests::bootstrap::contentful_bootstrap_materializes_a_complete_offline_root_mirror: test'
	@$(CARGO) test -p mason --lib \
		planner::hermetic_tests::bootstrap::contentful_bootstrap_materializes_a_complete_offline_root_mirror -- \
		--ignored --exact --nocapture
	@echo "Building, packaging, and reproducing all six fixtures from the contentful closure..."
	@$(CARGO) test -p mason --lib \
		planner::hermetic_tests::bootstrap::all_execution_fixtures_build_package_and_reproduce_from_the_contentful_closure -- \
		--ignored --exact --list | \
		grep -Fqx 'planner::hermetic_tests::bootstrap::all_execution_fixtures_build_package_and_reproduce_from_the_contentful_closure: test'
	@CAST_REQUIRE_EXECUTION=$(REQUIRE_EXECUTION) $(CARGO) test -p mason --lib \
		planner::hermetic_tests::bootstrap::all_execution_fixtures_build_package_and_reproduce_from_the_contentful_closure -- \
		--ignored --exact --nocapture

fixtures-ci: execution-fixtures
	@$(MAKE) --no-print-directory bootstrap-fixtures REQUIRE_EXECUTION=1

check:
	@$(CARGO) check --workspace --all-targets

fmt:
	@$(CARGO) fmt --all

clean:
	@$(CARGO) clean

migrate:
	@set -eu; \
	for db in meta layout state; do \
		diesel \
			--config-file "$(TOP_DIR)/crates/forge/src/db/$$db/diesel.toml" \
			--database-url "sqlite://$(TOP_DIR)/crates/forge/src/db/$$db/test.db" \
			migration run; \
	done

migrate-redo:
	@set -eu; \
	for db in meta layout state; do \
		diesel \
			--config-file "$(TOP_DIR)/crates/forge/src/db/$$db/diesel.toml" \
			--database-url "sqlite://$(TOP_DIR)/crates/forge/src/db/$$db/test.db" \
			migration redo; \
	done

libstone:
	@set -eu; \
	output="$$(mktemp)"; \
	trap 'rm -f "$$output"' EXIT; \
	$(CARGO) build -p libstone --release; \
	clang "$(TOP_DIR)/crates/libstone/examples/$(EXAMPLE).c" \
		-o "$$output" \
		-I"$(TOP_DIR)/crates/libstone/src" \
		-lstone -L"$(TOP_DIR)/target/release" \
		-Wl,-rpath,"$(TOP_DIR)/target/release"; \
	if [[ "$${USE_VALGRIND:-0}" == 1 ]]; then \
		time valgrind --track-origins=yes "$$output" "$(STONE)"; \
	else \
		time "$$output" "$(STONE)"; \
	fi

help:
	@echo
	@echo "Usage: make [target]"
	@echo
	@echo "Available targets:"
	@echo "  build         Build the complete workspace"
	@echo "  cast          Build Cast with MODE=$(MODE) (default)"
	@echo "  get-started   Build and install Cast and its data"
	@echo "  test          Run lints and all workspace tests"
	@echo "  examples      Check, evaluate, freeze, and fail-close the Gluon examples"
	@echo "  execution-fixtures  Verify real offline source archives and Gluon locks"
	@echo "  bootstrap-fixtures  Fetch the pinned closure and build all six real fixtures twice"
	@echo "                    Set REQUIRE_EXECUTION=1 to reject namespace-capability skips"
	@echo "  fixtures-ci    Required-capability six-fixture execution and reproduction gate"
	@echo "  fixture-sources  Rebuild deterministic offline execution-source archives"
	@echo "  check         Check all workspace targets"
	@echo "  fix           Apply clippy, formatting, and typo fixes"
	@echo "  fmt           Format the workspace"
	@echo "  binary-layout  Require Cast to be the sole executable target"
	@echo "  product-names  Reject active references to retired product names"
	@echo "  config-formats  Reject YAML/KDL outside external-service interfaces"
	@echo "  config-formats-test  Test the configuration-format gate"
	@echo "  migrate       Apply all Forge database migrations"
	@echo "  migrate-redo  Reapply all Forge database migrations"
	@echo "  libstone      Build and run the C libstone example"
	@echo "  clean         Remove Cargo build artifacts"
	@echo
