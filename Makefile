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

test:
	@cargo test --workspace $(TEST_ARGS) -- --test-threads=1

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
