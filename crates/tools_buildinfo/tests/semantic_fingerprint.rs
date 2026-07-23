use std::{
    ffi::OsString,
    fs,
    os::unix::{
        ffi::{OsStrExt as _, OsStringExt as _},
        fs::PermissionsExt as _,
    },
    path::{Path, PathBuf},
    process::Command,
};

use sha2::{Digest as _, Sha256};
use tempfile::TempDir;

#[path = "../src/semantic_fingerprint.rs"]
mod semantic_fingerprint;

#[path = "../src/native_build_context.rs"]
mod native_build_context;

#[path = "../src/tool_identity.rs"]
mod tool_identity;

include!("semantic_fingerprint/fixture_support.rs");

#[test]
fn build_exports_a_versioned_sha256_value() {
    let fingerprint = tools_buildinfo::get_semantic_fingerprint();
    let digest = fingerprint.strip_prefix("sha256:").unwrap();
    assert_eq!(digest.len(), 64);
    assert!(
        digest
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    );
}

#[test]
fn discovery_is_checkout_and_creation_order_independent() {
    let forward = fixture(BASE_FILES.iter().copied());
    let reverse = fixture(BASE_FILES.iter().rev().copied());

    let first = calculate(forward.path(), context()).unwrap();
    let second = calculate(reverse.path(), context().into_iter().rev()).unwrap();

    assert_eq!(first.value(), second.value());
    assert_eq!(first.relative_paths(), second.relative_paths());
    assert!(
        first
            .watched_paths()
            .iter()
            .any(|path| path.ends_with("crates/stone/src/lib.rs"))
    );
    assert_eq!(
        first.relative_paths(),
        &[
            "Cargo.lock",
            "Cargo.toml",
            "Makefile",
            "bin/cast/Cargo.toml",
            "bin/cast/src/main.rs",
            "crates/forge/Cargo.toml",
            "crates/forge/src/lib.rs",
            "crates/mason/Cargo.toml",
            "crates/mason/data/policy/default.glu",
            "crates/mason/data/policy/policy.glu",
            "crates/mason/data/policy/tuning/flags.glu",
            "crates/mason/data/policy/tuning/groups.glu",
            "crates/mason/src/lib.rs",
            "crates/stone/Cargo.toml",
            "crates/stone/build.rs",
            "crates/stone/gluon/stone.glu",
            "crates/stone/src/lib.rs",
            "flake.lock",
            "flake.nix",
        ]
    );
}

#[test]
fn source_archives_and_dirty_trees_are_identified_by_contents() {
    let repository = base_fixture();
    let original = value(repository.path());

    // No Git repository or status string participates in this calculation.
    write(
        repository.path(),
        "crates/stone/src/lib.rs",
        "pub fn read() { changed(); }\n",
    );
    assert_ne!(value(repository.path()), original);

    write(repository.path(), "crates/stone/src/extra.rs", "pub fn extra() {}\n");
    let with_extra_source = value(repository.path());
    fs::remove_file(repository.path().join("crates/stone/src/extra.rs")).unwrap();
    assert_ne!(value(repository.path()), with_extra_source);
}

#[test]
fn lock_manifests_policy_and_toolchain_files_are_all_semantic() {
    for path in [
        "Cargo.lock",
        "Cargo.toml",
        "bin/cast/Cargo.toml",
        "crates/forge/Cargo.toml",
        "crates/mason/Cargo.toml",
        "crates/stone/Cargo.toml",
        "crates/mason/data/policy/default.glu",
        "crates/mason/data/policy/policy.glu",
        "crates/mason/data/policy/tuning/flags.glu",
        "crates/mason/data/policy/tuning/groups.glu",
        "flake.nix",
        "flake.lock",
        "Makefile",
    ] {
        let repository = base_fixture();
        let original = value(repository.path());
        write(repository.path(), path, "changed semantic input\n");
        assert_ne!(value(repository.path()), original, "{path} must affect the fingerprint");
    }
}

#[test]
fn docs_tests_examples_generated_outputs_and_timestamps_are_excluded() {
    let repository = base_fixture();
    let original = value(repository.path());

    for path in [
        "README.md",
        "docs/design.md",
        "tests/fixtures/package.stone",
        "crates/mason/README.md",
        "crates/mason/tests/cli.rs",
        "crates/stone/examples/read.rs",
        "crates/stone/benches/read.rs",
        "target/debug/cast",
        ".direnv/profile",
        ".git/index",
    ] {
        write(repository.path(), path, "not a production input\n");
    }
    assert_eq!(value(repository.path()), original);

    // Rewriting identical bytes changes metadata but not semantic identity.
    write(repository.path(), "crates/stone/src/lib.rs", "pub fn read() {}\n");
    assert_eq!(value(repository.path()), original);
}

#[test]
fn target_profile_features_compiler_and_flags_are_length_prefixed_inputs() {
    let repository = base_fixture();
    let original = value(repository.path());

    for (name, changed) in [
        ("env.HOST", b"aarch64-unknown-linux-gnu".as_slice()),
        ("env.PROFILE", b"debug".as_slice()),
        ("env.TARGET", b"aarch64-unknown-linux-gnu".as_slice()),
        ("env.CARGO_FEATURE_EXPERIMENTAL", b"1".as_slice()),
        ("env.CARGO_CFG_TARGET_FEATURE", b"avx2".as_slice()),
        ("env.CARGO_ENCODED_RUSTFLAGS", b"-Ctarget-cpu=native".as_slice()),
        ("toolchain.rustc-vV", b"rustc other".as_slice()),
    ] {
        let mut changed_context = context();
        if let Some((_, value)) = changed_context.iter_mut().find(|(key, _)| key == name) {
            *value = changed.to_vec();
        } else {
            changed_context.push((name.to_owned(), changed.to_vec()));
        }
        let changed_value = calculate(repository.path(), changed_context).unwrap();
        assert_ne!(changed_value.value(), original, "{name} must affect the fingerprint");
    }

    // Length framing makes the two otherwise ambiguous byte streams distinct.
    let left = calculate(repository.path(), [("a".to_owned(), b"bc".to_vec())]).unwrap();
    let right = calculate(repository.path(), [("ab".to_owned(), b"c".to_vec())]).unwrap();
    assert_ne!(left.value(), right.value());
}

#[test]
fn native_tool_flags_and_dependency_selection_mutations_change_identity() {
    let repository = base_fixture();
    let workspace = Path::new("/workspace/os-tools");
    let original = native_value(repository.path(), &[], "1", workspace);

    for (key, value) in [
        ("CC_x86_64_unknown_linux_gnu", "clang"),
        ("CXX", "clang++"),
        ("AR", "llvm-ar"),
        ("ARFLAGS", "crsD"),
        ("RANLIB", "llvm-ranlib"),
        ("RANLIBFLAGS", "-D"),
        ("LD", "ld.lld"),
        ("AS", "llvm-as"),
        ("NM", "llvm-nm"),
        ("RUSTC_LINKER", "clang"),
        ("CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_LINKER", "clang"),
        ("CFLAGS_x86_64-unknown-linux-gnu", "-O3 -march=x86-64-v3"),
        ("CXXFLAGS", "-stdlib=libc++"),
        ("CPPFLAGS", "-DNATIVE_API=2"),
        ("LDFLAGS", "-Wl,-z,now"),
        ("CPATH", "/native/include"),
        ("C_INCLUDE_PATH", "/native/c/include"),
        ("CPLUS_INCLUDE_PATH", "/native/cxx/include"),
        ("LIBRARY_PATH", "/native/lib"),
        ("COMPILER_PATH", "/native/toolchain/bin"),
        ("LD_RUN_PATH", "/native/runtime/lib"),
        ("PKG_CONFIG_PATH", "/native/pkgconfig"),
        ("PKG_CONFIG_ALL_STATIC", "1"),
        ("CMAKE_GENERATOR", "Ninja"),
        ("LIBZSTD_NO_PKG_CONFIG", "1"),
        ("LIBZSTD_STATIC", "1"),
        ("LIBSQLITE3_SYS_USE_PKG_CONFIG", "0"),
        ("SQLITE3_LIB_DIR", "/native/sqlite/lib"),
        ("AWS_LC_SYS_C_STD", "11"),
        ("AWS_LC_SYS_STATIC", "1"),
        ("AWS_LC_SYS_CMAKE", "cmake-aws"),
        ("AWS_LC_SYS_CMAKE_GENERATOR", "Ninja"),
        ("SQLITE3_DLL_NAME", "sqlite3-custom"),
        ("NIX_CFLAGS_COMPILE", "-fstack-protector-strong"),
    ] {
        let changed = native_value(repository.path(), &[(key, value)], "1", workspace);
        assert_ne!(changed, original, "{key} must affect native implementation identity");
    }

    let changed_tool = native_value(repository.path(), &[], "2", workspace);
    assert_ne!(changed_tool, original, "resolved native tool versions must be semantic");
}

#[test]
fn native_context_is_ordered_normalized_and_ignores_shadowed_or_irrelevant_state() {
    let workspace = Path::new("/workspace/os-tools");
    let values = [
        ("CC_x86_64_unknown_linux_gnu", "clang -fno-plt"),
        ("CC", "shadowed-gcc"),
        ("CFLAGS", "-O2"),
        ("TARGET_CFLAGS", "-fPIC"),
        ("LIBZSTD_NO_PKG_CONFIG", "1"),
    ];

    let forward = native_context(&values, "1", workspace);
    let reverse_values = values.into_iter().rev().collect::<Vec<_>>();
    let reverse = native_context(&reverse_values, "1", workspace);
    assert_eq!(forward, reverse, "environment iteration order must not be semantic");

    let changed_shadow = native_context(
        &[
            ("CC_x86_64_unknown_linux_gnu", "clang -fno-plt"),
            ("CC", "another-shadowed-compiler"),
            ("CFLAGS", "-O2"),
            ("TARGET_CFLAGS", "-fPIC"),
            ("LIBZSTD_NO_PKG_CONFIG", "1"),
        ],
        "1",
        workspace,
    );
    assert_eq!(forward, changed_shadow, "shadowed tool selectors must be canonical");

    let with_irrelevant = native_context(
        &[
            ("CC_x86_64_unknown_linux_gnu", "clang -fno-plt"),
            ("CC", "shadowed-gcc"),
            ("CFLAGS", "-O2"),
            ("TARGET_CFLAGS", "-fPIC"),
            ("LIBZSTD_NO_PKG_CONFIG", "1"),
            ("EDITOR", "not-a-build-input"),
        ],
        "1",
        workspace,
    );
    assert_eq!(forward, with_irrelevant, "unrelated ambient state must be ignored");

    let without_optional = native_context(&[], "1", workspace);
    let present_empty = native_context(&[("LIBZSTD_NO_PKG_CONFIG", "")], "1", workspace);
    assert_ne!(
        without_optional, present_empty,
        "present-empty dependency controls are distinct from absence"
    );

    let collected = native_build_context::collect(native_environment(&values), workspace, |_, command| {
        Ok(Some(fake_command_identity(command, "1")))
    })
    .unwrap();
    assert!(collected.watched_environment().contains(&"CFLAGS".to_owned()));
    assert!(
        collected
            .watched_environment()
            .contains(&"CC_x86_64_unknown_linux_gnu".to_owned())
    );
    assert!(!collected.watched_environment().contains(&"EDITOR".to_owned()));
}

#[test]
fn selected_native_tool_without_an_identity_is_rejected() {
    let error = native_build_context::collect(
        native_environment(&[("CC", "unidentifiable-compiler")]),
        Path::new("/workspace/os-tools"),
        |_, command| {
            if command[0] == OsString::from("unidentifiable-compiler") {
                Ok(None)
            } else {
                Ok(Some(fake_command_identity(command, "1")))
            }
        },
    )
    .unwrap_err();

    assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
    assert!(error.to_string().contains("selected native tool cc"));
    assert!(error.to_string().contains("unidentifiable-compiler"));
}

#[test]
fn identical_version_output_does_not_hide_changed_executable_bytes() {
    let temporary = tempfile::tempdir().unwrap();
    let executable = temporary.path().join("compiler");
    write_executable(
        &executable,
        "#!/bin/sh\n# compiler implementation one\nprintf 'compiler 1.0\\n'\n",
    );
    let first = identify_test_executable(&executable, true);

    write_executable(
        &executable,
        "#!/bin/sh\n# compiler implementation two\nprintf 'compiler 1.0\\n'\n",
    );
    let second = identify_test_executable(&executable, true);

    assert_ne!(
        first.encode(temporary.path()),
        second.encode(temporary.path()),
        "executable content SHA-256 must distinguish tools with the same path and version"
    );
}

#[test]
fn replacing_a_rustc_wrapper_at_the_same_selector_changes_identity() {
    let temporary = tempfile::tempdir().unwrap();
    let wrapper = temporary.path().join("rustc-wrapper");
    write_executable(&wrapper, "#!/bin/sh\n# wrapper one\nexec \"$@\"\n");
    let first = identify_test_executable(&wrapper, false);

    write_executable(&wrapper, "#!/bin/sh\n# wrapper two\nexec \"$@\"\n");
    let second = identify_test_executable(&wrapper, false);

    assert_ne!(
        first.encode(temporary.path()),
        second.encode(temporary.path()),
        "Cargo wrappers have no version protocol, so their exact bytes must be bound"
    );
}

#[test]
fn native_wrapper_identity_binds_the_delegated_compiler_bytes() {
    let temporary = tempfile::tempdir().unwrap();
    let wrapper = temporary.path().join("sccache");
    let compiler = temporary.path().join("clang");
    write_executable(&wrapper, "#!/bin/sh\nexec \"$@\"\n");
    write_executable(&compiler, "#!/bin/sh\n# compiler one\nprintf 'clang 1.0\\n'\n");

    let command = vec![wrapper.clone().into_os_string(), compiler.clone().into_os_string()];
    assert_eq!(
        native_build_context::delegated_compiler("cc", &command, None),
        Some(compiler.as_os_str())
    );
    assert_eq!(
        native_build_context::delegated_compiler("archiver", &command, None),
        None
    );

    let mut first = tool_identity::CommandIdentity::new(identify_test_executable(&wrapper, false));
    first.push_delegated(identify_test_executable(&compiler, false));

    write_executable(&compiler, "#!/bin/sh\n# compiler two\nprintf 'clang 1.0\\n'\n");
    let mut second = tool_identity::CommandIdentity::new(identify_test_executable(&wrapper, false));
    second.push_delegated(identify_test_executable(&compiler, false));

    assert_ne!(
        first.encode(temporary.path()),
        second.encode(temporary.path()),
        "the wrapper and delegated compiler must both be byte-bound"
    );
}

#[test]
fn equivalent_workspace_executable_paths_have_one_identity() {
    let first_workspace = tempfile::tempdir().unwrap();
    let second_workspace = tempfile::tempdir().unwrap();
    let relative = Path::new("toolchain/bin/compiler");
    let first_path = first_workspace.path().join(relative);
    let second_path = second_workspace.path().join(relative);
    fs::create_dir_all(first_path.parent().unwrap()).unwrap();
    fs::create_dir_all(second_path.parent().unwrap()).unwrap();
    let contents = "#!/bin/sh\nprintf 'compiler 1.0\\n'\n";
    write_executable(&first_path, contents);
    write_executable(&second_path, contents);

    let first = identify_test_executable(&first_path, true);
    let second = identify_test_executable(&second_path, true);
    assert_eq!(first.resolved_path(), first_path.canonicalize().unwrap());
    assert_eq!(second.resolved_path(), second_path.canonicalize().unwrap());

    assert_eq!(
        first.encode(first_workspace.path()),
        second.encode(second_workspace.path()),
        "canonical paths beneath equivalent workspaces must use the workspace token"
    );
}

#[test]
fn workspace_paths_and_equivalent_tool_aliases_have_one_native_identity() {
    let first = native_context(
        &[
            ("CC", "/workspace/first/toolchain/bin/clang --target=x86_64-linux"),
            ("CFLAGS", "-I/workspace/first/native/include"),
        ],
        "1",
        Path::new("/workspace/first"),
    );
    let second = native_context(
        &[
            (
                "CC_x86_64_unknown_linux_gnu",
                "/different/checkout/toolchain/bin/clang --target=x86_64-linux",
            ),
            ("CFLAGS", "-I/different/checkout/native/include"),
        ],
        "1",
        Path::new("/different/checkout"),
    );

    assert_eq!(first, second);
}

#[test]
fn ambient_cmake_build_tool_aliases_are_irrelevant_to_the_static_cc_builder() {
    let workspace = Path::new("/workspace/os-tools");
    let first = native_context(
        &[
            ("MAKE", "make-one"),
            ("NINJA", "ninja-one"),
            ("NMAKE", "nmake-one"),
            ("CMAKE_MAKE_PROGRAM", "cmake-make-one"),
        ],
        "1",
        workspace,
    );
    let second = native_context(
        &[
            ("MAKE", "make-two"),
            ("NINJA", "ninja-two"),
            ("NMAKE", "nmake-two"),
            ("CMAKE_MAKE_PROGRAM", "cmake-make-two"),
        ],
        "1",
        workspace,
    );

    assert_eq!(first, second, "the locked aws-lc build never enters CMake");
}

#[test]
fn executable_cmake_contexts_fail_closed() {
    let workspace = Path::new("/workspace/os-tools");
    for key in [
        "AWS_LC_SYS_CMAKE_TOOLCHAIN_FILE_x86_64_unknown_linux_gnu",
        "AWS_LC_SYS_CMAKE_TOOLCHAIN_FILE",
        "CMAKE_TOOLCHAIN_FILE_x86_64-unknown-linux-gnu",
        "CMAKE_TOOLCHAIN_FILE_x86_64_unknown_linux_gnu",
        "HOST_CMAKE_TOOLCHAIN_FILE",
        "CMAKE_TOOLCHAIN_FILE",
        "CMAKE_GENERATOR_PLATFORM",
        "CMAKE_GENERATOR_TOOLSET",
        "CMAKE_GENERATOR_INSTANCE",
    ] {
        let error = native_build_context::collect(native_environment(&[(key, "selected")]), workspace, |_, command| {
            Ok(Some(fake_command_identity(command, "1")))
        })
        .unwrap_err();

        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
        assert!(error.to_string().contains(key));
        assert!(error.to_string().contains("static cc-rs builder contract"));
    }
}

#[test]
fn aws_lc_external_bindgen_lanes_fail_closed() {
    let workspace = Path::new("/workspace/os-tools");
    for (control, value) in [
        ("AWS_LC_SYS_EXTERNAL_BINDGEN", "1"),
        ("AWS_LC_SYS_NO_PREFIX", "yes"),
        ("AWS_LC_SYS_PREGENERATING_BINDINGS", "ON"),
    ] {
        let error = native_build_context::collect(native_environment(&[(control, value)]), workspace, |_, command| {
            Ok(Some(fake_command_identity(command, "1")))
        })
        .unwrap_err();

        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
        assert!(error.to_string().contains(control));
        assert!(error.to_string().contains("external bindgen toolchain"));
    }
}

#[test]
fn aws_lc_bindgen_controls_use_crate_target_precedence() {
    let workspace = Path::new("/workspace/os-tools");
    let target_control = "AWS_LC_SYS_EXTERNAL_BINDGEN_x86_64_unknown_linux_gnu";

    native_build_context::collect(
        native_environment(&[(target_control, "0"), ("AWS_LC_SYS_EXTERNAL_BINDGEN", "1")]),
        workspace,
        |_, command| Ok(Some(fake_command_identity(command, "1"))),
    )
    .unwrap();

    let error = native_build_context::collect(
        native_environment(&[(target_control, "true"), ("AWS_LC_SYS_EXTERNAL_BINDGEN", "0")]),
        workspace,
        |_, command| Ok(Some(fake_command_identity(command, "1"))),
    )
    .unwrap_err();
    assert!(error.to_string().contains("AWS_LC_SYS_EXTERNAL_BINDGEN"));

    // This control does not disable pregenerated bindings, but it does enable
    // AWS-LC's external Go/Perl source generation and must also fail closed.
    let error = native_build_context::collect(
        native_environment(&[("AWS_LC_SYS_NO_PREGENERATED_SRC", "1")]),
        workspace,
        |_, command| Ok(Some(fake_command_identity(command, "1"))),
    )
    .unwrap_err();
    assert!(error.to_string().contains("Go/Perl"));
}

#[test]
fn aws_lc_cmake_builder_and_fallback_lanes_fail_closed() {
    let workspace = Path::new("/workspace/os-tools");
    for (control, value) in [
        ("AWS_LC_SYS_CMAKE_BUILDER", "1"),
        ("AWS_LC_SYS_STATIC", "0"),
        ("AWS_LC_SYS_NO_ASM", "true"),
        ("AWS_LC_SYS_SANITIZER", "address"),
    ] {
        let error = native_build_context::collect(native_environment(&[(control, value)]), workspace, |_, command| {
            Ok(Some(fake_command_identity(command, "1")))
        })
        .unwrap_err();
        assert!(error.to_string().contains(control));
        assert!(error.to_string().contains("static cc-rs builder"));
    }

    // Upstream checks an explicit false CMAKE_BUILDER before its NO_ASM and
    // sanitizer fallbacks, so this combination remains on the modeled cc-rs
    // path and all effective compiler inputs are still byte-bound.
    native_build_context::collect(
        native_environment(&[
            ("AWS_LC_SYS_CMAKE_BUILDER", "0"),
            ("AWS_LC_SYS_NO_ASM", "1"),
            ("AWS_LC_SYS_SANITIZER", "address"),
        ]),
        workspace,
        |_, command| Ok(Some(fake_command_identity(command, "1"))),
    )
    .unwrap();

    let error = native_build_context::collect(
        native_environment_for_platform(
            "x86_64-unknown-linux-ohos",
            "x86_64-unknown-linux-ohos",
            "linux",
            "ohos",
            &[],
        ),
        workspace,
        |_, command| Ok(Some(fake_command_identity(command, "1"))),
    )
    .unwrap_err();
    assert!(error.to_string().contains("CARGO_CFG_TARGET_ENV=ohos"));
}

#[test]
fn aws_lc_builder_controls_use_crate_target_precedence() {
    let workspace = Path::new("/workspace/os-tools");
    let suffix = "AWS_LC_SYS_CMAKE_BUILDER_x86_64_unknown_linux_gnu";

    native_build_context::collect(
        native_environment(&[
            (suffix, "0"),
            ("AWS_LC_SYS_CMAKE_BUILDER", "1"),
            ("AWS_LC_SYS_NO_ASM", "1"),
        ]),
        workspace,
        |_, command| Ok(Some(fake_command_identity(command, "1"))),
    )
    .unwrap();

    let error = native_build_context::collect(
        native_environment(&[(suffix, "1"), ("AWS_LC_SYS_CMAKE_BUILDER", "0")]),
        workspace,
        |_, command| Ok(Some(fake_command_identity(command, "1"))),
    )
    .unwrap_err();
    assert!(error.to_string().contains("AWS_LC_SYS_CMAKE_BUILDER=true"));

    native_build_context::collect(
        native_environment(&[
            ("AWS_LC_SYS_STATIC_x86_64_unknown_linux_gnu", "1"),
            ("AWS_LC_SYS_STATIC", "0"),
        ]),
        workspace,
        |_, command| Ok(Some(fake_command_identity(command, "1"))),
    )
    .unwrap();
}

#[test]
fn unsupported_target_and_external_library_lanes_fail_closed() {
    let workspace = Path::new("/workspace/os-tools");
    let error = native_build_context::collect(
        native_environment_for_platform(
            "x86_64-unknown-linux-gnu",
            "x86_64-pc-windows-msvc",
            "windows",
            "msvc",
            &[],
        ),
        workspace,
        |_, command| Ok(Some(fake_command_identity(command, "1"))),
    )
    .unwrap_err();
    assert!(error.to_string().contains("currently Linux-only"));

    for value in ["", "0", "1"] {
        let error = native_build_context::collect(
            native_environment(&[("ZSTD_SYS_USE_PKG_CONFIG", value)]),
            workspace,
            |_, command| Ok(Some(fake_command_identity(command, "1"))),
        )
        .unwrap_err();
        assert!(error.to_string().contains("ZSTD_SYS_USE_PKG_CONFIG must be absent"));
    }

    for value in ["", "1", "yes"] {
        let error = native_build_context::collect(
            native_environment(&[("LIBSQLITE3_SYS_USE_PKG_CONFIG", value)]),
            workspace,
            |_, command| Ok(Some(fake_command_identity(command, "1"))),
        )
        .unwrap_err();
        assert!(error.to_string().contains("must be absent or exactly 0"));
    }
    native_build_context::collect(
        native_environment(&[("LIBSQLITE3_SYS_USE_PKG_CONFIG", "0")]),
        workspace,
        |_, command| Ok(Some(fake_command_identity(command, "1"))),
    )
    .unwrap();
}

#[test]
fn aws_lc_compiler_precedence_binds_only_the_effective_override() {
    let workspace = Path::new("/workspace/os-tools");
    let selectors = [
        ("AWS_LC_SYS_TARGET_CC_x86_64_unknown_linux_gnu", "selected-aws-cc"),
        ("AWS_LC_SYS_TARGET_CC", "shadowed-aws-target-cc"),
        ("TARGET_CC_x86_64_unknown_linux_gnu", "shadowed-target-cc-suffixed"),
        ("TARGET_CC", "shadowed-target-cc"),
        ("AWS_LC_SYS_CC_x86_64_unknown_linux_gnu", "shadowed-aws-cc-suffixed"),
        ("AWS_LC_SYS_CC", "shadowed-aws-cc"),
        ("CC_x86_64_unknown_linux_gnu", "shadowed-cc-suffixed"),
        ("CC", "shadowed-cc"),
    ];
    let selected = native_context(&selectors, "1", workspace);
    let aws_cc = |inputs: &[ExplicitInput]| {
        inputs
            .iter()
            .filter(|(key, _)| key.starts_with("native.tool.aws-lc-cc."))
            .cloned()
            .collect::<Vec<_>>()
    };

    let changed_shadows = native_context(
        &[
            selectors[0],
            ("AWS_LC_SYS_TARGET_CC", "different-shadow-1"),
            ("TARGET_CC_x86_64_unknown_linux_gnu", "different-shadow-2"),
            ("TARGET_CC", "different-shadow-3"),
            ("AWS_LC_SYS_CC_x86_64_unknown_linux_gnu", "different-shadow-4"),
            ("AWS_LC_SYS_CC", "different-shadow-5"),
            ("CC_x86_64_unknown_linux_gnu", "different-shadow-6"),
            ("CC", "different-shadow-7"),
        ],
        "1",
        workspace,
    );
    assert_eq!(
        aws_cc(&selected),
        aws_cc(&changed_shadows),
        "shadowed AWS-LC compiler selectors must not affect identity"
    );

    let changed_selected = native_context(
        &[
            (
                "AWS_LC_SYS_TARGET_CC_x86_64_unknown_linux_gnu",
                "different-selected-aws-cc",
            ),
            selectors[1],
            selectors[2],
            selectors[3],
            selectors[4],
            selectors[5],
            selectors[6],
            selectors[7],
        ],
        "1",
        workspace,
    );
    assert_ne!(selected, changed_selected);
    assert!(selected.iter().any(|(key, _)| key == "native.tool.aws-lc-cc.identity"));
}

#[test]
fn cc_rs_exact_tool_paths_with_spaces_remain_one_executable() {
    let temporary = tempfile::tempdir().unwrap();
    let compiler = temporary.path().join("compiler with spaces");
    write_executable(&compiler, "#!/bin/sh\nprintf 'compiler 1.0\\n'\n");

    let selector = compiler.to_str().unwrap();
    let mut saw_compiler = false;
    native_build_context::collect(
        native_environment(&[("CC", selector)]),
        temporary.path(),
        |role, command| {
            if role == "cc" {
                saw_compiler = true;
                assert_eq!(command, &[compiler.clone().into_os_string()]);
            }
            Ok(Some(fake_command_identity(command, "1")))
        },
    )
    .unwrap();

    assert!(saw_compiler);
}

#[test]
fn direct_native_tool_selectors_with_spaces_are_never_shell_split() {
    let temporary = tempfile::tempdir().unwrap();
    let tool = temporary.path().join("direct tool with spaces");
    write_executable(&tool, "#!/bin/sh\nprintf 'tool 1.0\\n'\n");
    let selector = tool.to_str().unwrap();
    let mut seen = Vec::new();

    native_build_context::collect(
        native_environment(&[
            ("LD", selector),
            ("AS", selector),
            ("NM", selector),
            ("RUSTC_LINKER", selector),
        ]),
        temporary.path(),
        |role, command| {
            if matches!(role, "linker" | "assembler" | "symbol-reader" | "rust-linker") {
                assert_eq!(command, &[tool.clone().into_os_string()]);
                seen.push(role.to_owned());
            }
            Ok(Some(fake_command_identity(command, "1")))
        },
    )
    .unwrap();

    seen.sort();
    assert_eq!(seen, ["assembler", "linker", "rust-linker", "symbol-reader"]);
}

#[test]
fn aws_lc_cxx_uses_crate_target_precedence() {
    let workspace = Path::new("/workspace/os-tools");
    let mut selected = None;
    native_build_context::collect(
        native_environment(&[
            ("AWS_LC_SYS_TARGET_CXX_x86_64_unknown_linux_gnu", "selected-aws-cxx"),
            ("AWS_LC_SYS_TARGET_CXX", "shadowed-aws-target-cxx"),
            ("TARGET_CXX_x86_64_unknown_linux_gnu", "shadowed-target-cxx"),
            ("TARGET_CXX", "shadowed-target-cxx-base"),
            ("AWS_LC_SYS_CXX", "shadowed-aws-cxx"),
            ("CXX", "shadowed-cxx"),
        ]),
        workspace,
        |role, command| {
            if role == "aws-lc-cxx" {
                selected = command.first().cloned();
            }
            Ok(Some(fake_command_identity(command, "1")))
        },
    )
    .unwrap();

    assert_eq!(selected, Some(OsString::from("selected-aws-cxx")));
}

#[test]
fn replacing_an_aws_lc_compiler_at_one_selector_changes_native_identity() {
    let temporary = tempfile::tempdir().unwrap();
    let compiler = temporary.path().join("aws-cc");
    let values = &[(
        "AWS_LC_SYS_TARGET_CC_x86_64_unknown_linux_gnu",
        compiler.to_str().unwrap(),
    )];
    let collect = || {
        native_build_context::collect(native_environment(values), temporary.path(), |role, command| {
            if role == "aws-lc-cc" {
                let executable = identify_test_executable(Path::new(&command[0]), true);
                Ok(Some(tool_identity::CommandIdentity::new(executable)))
            } else {
                Ok(Some(fake_command_identity(command, "1")))
            }
        })
        .unwrap()
        .inputs()
    };

    write_executable(
        &compiler,
        "#!/bin/sh\n# implementation one\nprintf 'aws compiler 1.0\\n'\n",
    );
    let first = collect();
    write_executable(
        &compiler,
        "#!/bin/sh\n# implementation two\nprintf 'aws compiler 1.0\\n'\n",
    );
    let second = collect();

    assert_ne!(
        first, second,
        "AWS-LC compiler bytes must remain semantic when selector and version are unchanged"
    );
}

#[test]
fn cross_compile_prefix_discovery_fails_closed_without_explicit_tools() {
    let error = native_build_context::collect(
        native_environment_for(
            "x86_64-unknown-linux-gnu",
            "aarch64-unknown-linux-gnu",
            &[("CROSS_COMPILE", "aarch64-linux-gnu-")],
        ),
        Path::new("/workspace/os-tools"),
        |_, command| Ok(Some(fake_command_identity(command, "1"))),
    )
    .unwrap_err();

    assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
    assert!(error.to_string().contains("cross build"));
    for selector in ["CC", "CXX", "AR", "RANLIB"] {
        assert!(error.to_string().contains(selector));
    }
    assert!(error.to_string().contains("CROSS_COMPILE"));
}

#[test]
fn cross_build_with_explicit_compiler_and_archive_tools_is_content_bound() {
    let environment = native_environment_for(
        "x86_64-unknown-linux-gnu",
        "aarch64-unknown-linux-gnu",
        &[
            ("TARGET_CC", "aarch64-cc"),
            ("TARGET_CXX", "aarch64-cxx"),
            ("TARGET_AR", "aarch64-ar"),
            ("TARGET_RANLIB", "aarch64-ranlib"),
            ("CROSS_COMPILE", "ignored-prefix-"),
        ],
    );
    let first = native_build_context::collect(environment.clone(), Path::new("/workspace/os-tools"), |_, command| {
        Ok(Some(fake_command_identity(command, "1")))
    })
    .unwrap()
    .inputs();
    let second = native_build_context::collect(environment, Path::new("/workspace/os-tools"), |role, command| {
        let revision = if role == "archiver" { "2" } else { "1" };
        Ok(Some(fake_command_identity(command, revision)))
    })
    .unwrap()
    .inputs();

    assert_ne!(first, second, "the explicit cross archiver bytes must be semantic");
}

#[test]
fn workspace_normalization_does_not_collapse_near_prefix_paths() {
    let values = &[("CFLAGS", "-I/workspace/fooish/native/include")];
    let near_prefix = native_context(values, "1", Path::new("/workspace/foo"));
    let unrelated = native_context(values, "1", Path::new("/somewhere/else"));

    assert_eq!(near_prefix, unrelated);
}
