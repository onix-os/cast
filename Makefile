PROJECT_NAME := $(shell awk -F'"' '/^\[package\]/{package=1; next} package && /^name = /{print $$2; exit}' bin/cast/Cargo.toml)
PROJECT_CAP  := $(shell echo $(PROJECT_NAME) | tr '[:lower:]' '[:upper:]')
CURRENT_VERSION := $(shell awk -F'"' '/^\[workspace.package\]/{package=1; next} package && /^version = /{print $$2; exit}' Cargo.toml)
LATEST_TAG   ?= $(shell git describe --tags --abbrev=0 2>/dev/null)
TOP_DIR      := $(CURDIR)
BUILD_DIR    := $(TOP_DIR)/target

ifeq ($(PROJECT_NAME),)
$(error Error: project name not found in bin/cast/Cargo.toml)
endif

$(info ------------------------------------------)
$(info Project: $(PROJECT_NAME))
$(info Version: $(CURRENT_VERSION))
$(info ------------------------------------------)

.PHONY: build b compile c run r test t verify help h clean release

SHELL := /bin/bash


build:
	@cargo build --release

b: build

compile:
	@cargo clean
	@make build

c: compile

ARGS ?=
DIR ?= $(TOP_DIR)

run:
	@cd $(DIR) && cargo run --manifest-path $(TOP_DIR)/Cargo.toml -p cast -- $(ARGS)

r: run

TEST_ARGS ?=

# Concurrency ceiling for the suite.
#
# Not a stylistic choice. Several boot-path budgets are *absolute* deadlines
# armed once and inherited by nested stages; production runs one boot
# publication at a time, while the suite runs that path in many concurrent
# tests. Above ~16 workers, contention alone exhausts those budgets and
# surfaces as `DeadlineExceeded` on work that is not slow — see
# `plans/future_impl.md` §2.1a. 16 is verified green; 24 is not.
#
# Override for a bisect with: make test TEST_THREADS=1
#
# NOTE: TEST_ARGS does not suppress --workspace, so `make test TEST_ARGS="-p forge --lib"`
# still runs every crate. The container crate mount tests wedge indefinitely if another
# workspace test run is active, so never run two at once (cost ~110min on 2026-08-10).
TEST_THREADS ?= 16

test:
	@cargo test --workspace $(TEST_ARGS) -- --test-threads=$(TEST_THREADS)

t: test

verify: build test

help:
	@echo
	@echo "Usage: make [target]"
	@echo
	@echo "Available targets:"
	@echo "  build        Build project"
	@echo "  compile      Configure and generate build files"
	@echo "  run          Run the main executable"
	@echo "  test         Run tests"
	@echo "  verify       Build and test the complete workspace"
	@echo "  release      Create a new release (TYPE=patch|minor|major)"
	@echo

h : help

clean:
	@echo "Cleaning build directory..."
	@rm -rf $(BUILD_DIR)
	@echo "Build directory cleaned."

TYPE ?= patch
HAS_REL := $(shell command -v git-rel 2>/dev/null)

release:
	@if [ -z "$(HAS_REL)" ]; then \
		echo "git-rel is not installed. Please install it first."; \
		exit 1; \
	fi
	@if [ -z "$(TYPE)" ]; then \
		echo "Release type not specified. Use 'make release TYPE=[patch|minor|major|m.m.p]'"; \
		exit 1; \
	fi
	@git rel $(TYPE)

include misc/make/project.mk
