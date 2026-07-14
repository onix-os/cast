
cmake_minimum_required(VERSION 3.16)
project(cast_factory_override_fixture VERSION 1.0.0 LANGUAGES C)

include(GNUInstallDirs)
enable_testing()

set(CAST_FACTORY_VARIANT "factory-default" CACHE STRING "Stone factory variant")
if(NOT CAST_FACTORY_VARIANT STREQUAL "stone-override")
    message(FATAL_ERROR
        "the Stone package patch was not applied: ${CAST_FACTORY_VARIANT}"
    )
endif()

add_executable(cast-factory-override-fixture hello.c)
target_compile_definitions(cast-factory-override-fixture PRIVATE
    CAST_FACTORY_VARIANT="${CAST_FACTORY_VARIANT}"
)
install(TARGETS cast-factory-override-fixture
    RUNTIME DESTINATION "${CMAKE_INSTALL_BINDIR}"
)

add_test(NAME cast-factory-override-fixture-runs
    COMMAND cast-factory-override-fixture
)
set_tests_properties(cast-factory-override-fixture-runs PROPERTIES
    PASS_REGULAR_EXPRESSION "Stone-native factory override: stone-override"
)
