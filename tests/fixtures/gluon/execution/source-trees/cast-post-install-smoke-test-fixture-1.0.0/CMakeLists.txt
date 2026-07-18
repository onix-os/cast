cmake_minimum_required(VERSION 3.16)
project(cast_post_install_smoke_test_fixture VERSION 1.0.0 LANGUAGES C)

include(GNUInstallDirs)
enable_testing()

add_executable(staged-probe staged-probe.c)
install(TARGETS staged-probe RUNTIME DESTINATION "${CMAKE_INSTALL_BINDIR}")
install(DIRECTORY DESTINATION "${CMAKE_INSTALL_DATADIR}/cast")

add_test(NAME staged-probe-build-tree COMMAND staged-probe)
set_tests_properties(staged-probe-build-tree PROPERTIES
    PASS_REGULAR_EXPRESSION "staged-probe: build-tree check passed"
)
