
cmake_minimum_required(VERSION 3.16)
project(cast_cmake_fixture VERSION 1.0.0 LANGUAGES C)

include(GNUInstallDirs)
enable_testing()
find_package(ZLIB REQUIRED)

add_executable(cast-cmake-fixture hello.c)
target_link_libraries(cast-cmake-fixture PRIVATE ZLIB::ZLIB)
install(TARGETS cast-cmake-fixture RUNTIME DESTINATION "${CMAKE_INSTALL_BINDIR}")
add_test(NAME cast-cmake-fixture-runs COMMAND cast-cmake-fixture)
set_tests_properties(cast-cmake-fixture-runs PROPERTIES
    PASS_REGULAR_EXPRESSION "cast cmake fixture: zlib round-trip verified"
)
