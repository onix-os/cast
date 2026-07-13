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

.DEFAULT_GOAL := cast

.PHONY: build cast get-started licenses fix lint test check fmt clean \
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
	echo "The AerynOS documentation lives at https://aerynos.dev"

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
	@$(CARGO) test --all

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
