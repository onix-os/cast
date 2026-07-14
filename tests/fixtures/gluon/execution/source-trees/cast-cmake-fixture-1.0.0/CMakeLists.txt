# SPDX-FileCopyrightText: 2026 AerynOS Developers
# SPDX-License-Identifier: MPL-2.0

cmake_minimum_required(VERSION 3.16)
project(cast_cmake_fixture VERSION 1.0.0 LANGUAGES C)

include(GNUInstallDirs)
enable_testing()

add_executable(cast-cmake-fixture hello.c)
install(TARGETS cast-cmake-fixture RUNTIME DESTINATION "${CMAKE_INSTALL_BINDIR}")
add_test(NAME cast-cmake-fixture-runs COMMAND cast-cmake-fixture)
