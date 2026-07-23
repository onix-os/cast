
cmake_minimum_required(VERSION 3.16)
project(cast_split_fixture VERSION 1.0.0 LANGUAGES C)

include(GNUInstallDirs)
enable_testing()

add_library(cast-split SHARED libcastsplit.c)
set_target_properties(cast-split PROPERTIES
    OUTPUT_NAME cast-split
    VERSION 1.0.0
    SOVERSION 1
)
target_include_directories(cast-split PUBLIC
    "$<BUILD_INTERFACE:${CMAKE_CURRENT_SOURCE_DIR}>"
    "$<INSTALL_INTERFACE:${CMAKE_INSTALL_INCLUDEDIR}>"
)

add_executable(cast-split-fixture main.c)
target_link_libraries(cast-split-fixture PRIVATE cast-split)
add_test(NAME cast-split-fixture-runs COMMAND cast-split-fixture)

configure_file(cast-split.pc.in cast-split.pc @ONLY)

install(TARGETS cast-split cast-split-fixture
    LIBRARY DESTINATION "${CMAKE_INSTALL_LIBDIR}"
    RUNTIME DESTINATION "${CMAKE_INSTALL_BINDIR}"
)
install(FILES libcastsplit.h DESTINATION "${CMAKE_INSTALL_INCLUDEDIR}/cast-split")
install(FILES "${CMAKE_CURRENT_BINARY_DIR}/cast-split.pc"
    DESTINATION "${CMAKE_INSTALL_LIBDIR}/pkgconfig"
)
install(FILES README.md DESTINATION "${CMAKE_INSTALL_DOCDIR}")
install(FILES cast-split-fixture.1 DESTINATION "${CMAKE_INSTALL_MANDIR}/man1")
