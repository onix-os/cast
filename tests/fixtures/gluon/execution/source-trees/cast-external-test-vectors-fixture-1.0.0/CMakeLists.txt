cmake_minimum_required(VERSION 3.16)
project(cast_external_test_vectors_fixture VERSION 1.0.0 LANGUAGES C)

include(GNUInstallDirs)
include(CTest)

set(CAST_EXTERNAL_VECTOR_FILE "" CACHE STRING "Build-directory-relative conformance corpus")
if(NOT CAST_EXTERNAL_VECTOR_FILE STREQUAL "external-test-vectors.json")
    message(FATAL_ERROR "CAST_EXTERNAL_VECTOR_FILE must name the admitted external corpus")
endif()

add_executable(cast-external-test-vectors-fixture frame_codec.c)
target_compile_features(cast-external-test-vectors-fixture PRIVATE c_std_11)
install(
    TARGETS cast-external-test-vectors-fixture
    RUNTIME DESTINATION "${CMAKE_INSTALL_BINDIR}"
)

if(BUILD_TESTING)
    add_test(
        NAME cast-external-test-vectors-fixture-consumes-locked-corpus
        COMMAND
            cast-external-test-vectors-fixture
            --vectors
            "${CMAKE_BINARY_DIR}/${CAST_EXTERNAL_VECTOR_FILE}"
    )
    set_tests_properties(
        cast-external-test-vectors-fixture-consumes-locked-corpus
        PROPERTIES
            PASS_REGULAR_EXPRESSION
                "cast external test vectors fixture: 3 independently locked vectors verified"
    )
endif()
