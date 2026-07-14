# SPDX-FileCopyrightText: 2026 AerynOS Developers
# SPDX-License-Identifier: MPL-2.0

cmake_minimum_required(VERSION 3.16)
project(cast_daemon_fixture VERSION 1.0.0 LANGUAGES C)

include(GNUInstallDirs)
enable_testing()

file(MAKE_DIRECTORY "${CMAKE_CURRENT_BINARY_DIR}/generated")
configure_file(src/config.h.in generated/config.h @ONLY)
configure_file(packaging/cast-daemon.conf.in generated/cast-daemon.conf @ONLY)
configure_file(packaging/cast-daemon.service.in generated/cast-daemon.service @ONLY)
configure_file(packaging/cast-daemon.8.in generated/cast-daemon.8 @ONLY)

add_executable(cast-daemon-fixture src/daemon.c)
target_include_directories(cast-daemon-fixture PRIVATE "${CMAKE_CURRENT_BINARY_DIR}/generated")

add_test(NAME cast-daemon-self-test COMMAND cast-daemon-fixture --self-test)
add_test(
    NAME cast-daemon-generated-config
    COMMAND cast-daemon-fixture --check-config "${CMAKE_CURRENT_BINARY_DIR}/generated/cast-daemon.conf"
)

install(TARGETS cast-daemon-fixture RUNTIME DESTINATION "${CMAKE_INSTALL_SBINDIR}")
install(FILES "${CMAKE_CURRENT_BINARY_DIR}/generated/cast-daemon.conf"
    DESTINATION "${CMAKE_INSTALL_DATADIR}/defaults/cast-daemon-fixture"
)
install(FILES "${CMAKE_CURRENT_BINARY_DIR}/generated/cast-daemon.service"
    DESTINATION "${CMAKE_INSTALL_LIBDIR}/systemd/system"
)
install(FILES "${CMAKE_CURRENT_BINARY_DIR}/generated/cast-daemon.8"
    DESTINATION "${CMAKE_INSTALL_MANDIR}/man8"
)
