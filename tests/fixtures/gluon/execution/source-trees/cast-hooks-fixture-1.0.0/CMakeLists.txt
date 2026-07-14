# SPDX-FileCopyrightText: 2026 AerynOS Developers
# SPDX-License-Identifier: MPL-2.0

cmake_minimum_required(VERSION 3.16)
project(cast_hooks_fixture VERSION 1.0.0 LANGUAGES C)

include(GNUInstallDirs)
enable_testing()

add_executable(cast-hooks-fixture hello.c)
install(TARGETS cast-hooks-fixture RUNTIME DESTINATION "${CMAKE_INSTALL_BINDIR}")

add_test(NAME cast-hooks-fixture-runs COMMAND cast-hooks-fixture)
set_tests_properties(cast-hooks-fixture-runs PROPERTIES
    PASS_REGULAR_EXPRESSION "pre_setup hook applied"
)
