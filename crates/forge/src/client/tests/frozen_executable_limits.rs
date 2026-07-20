#[test]
fn frozen_executable_limits_accept_the_boundary_and_reject_the_next_value() {
    assert!(require_frozen_executable_package_count(MAX_FROZEN_EXECUTABLE_PACKAGES).is_ok());
    assert!(matches!(
        require_frozen_executable_package_count(MAX_FROZEN_EXECUTABLE_PACKAGES + 1),
        Err(Error::FrozenExecutablePackageLimit { limit, actual })
            if limit == MAX_FROZEN_EXECUTABLE_PACKAGES
                && actual == MAX_FROZEN_EXECUTABLE_PACKAGES + 1
    ));
    assert!(require_frozen_executable_binding_count(MAX_FROZEN_EXECUTABLE_BINDINGS).is_ok());
    assert!(matches!(
        require_frozen_executable_binding_count(MAX_FROZEN_EXECUTABLE_BINDINGS + 1),
        Err(Error::FrozenExecutableBindingLimit { limit, actual })
            if limit == MAX_FROZEN_EXECUTABLE_BINDINGS
                && actual == MAX_FROZEN_EXECUTABLE_BINDINGS + 1
    ));

    let binding = FrozenExecutableBinding {
        package: package::Id::from("limit-provider"),
        path: PathBuf::from("/usr/bin/limit-tool"),
    };
    let package = binding.package.clone();

    let path_prefix = "/usr/bin/";
    let accepted_path = format!(
        "{path_prefix}{}",
        "a".repeat(MAX_FROZEN_EXECUTABLE_PATH_BYTES - path_prefix.len())
    );
    let accepted_binding = FrozenExecutableBinding {
        package: package.clone(),
        path: PathBuf::from(&accepted_path),
    };
    assert_eq!(accepted_path.len(), MAX_FROZEN_EXECUTABLE_PATH_BYTES);
    require_frozen_executable_path(&accepted_binding).unwrap();
    let rejected_binding = FrozenExecutableBinding {
        package: package.clone(),
        path: PathBuf::from(format!("{accepted_path}a")),
    };
    assert!(matches!(
        require_frozen_executable_path(&rejected_binding),
        Err(Error::FrozenExecutablePathByteLimit { limit, actual })
            if limit == MAX_FROZEN_EXECUTABLE_PATH_BYTES
                && actual == MAX_FROZEN_EXECUTABLE_PATH_BYTES + 1
    ));
    assert!(frozen_executable_symlink_target_length_is_admitted(
        MAX_FROZEN_EXECUTABLE_SYMLINK_TARGET_BYTES
    ));
    assert!(!frozen_executable_symlink_target_length_is_admitted(
        MAX_FROZEN_EXECUTABLE_SYMLINK_TARGET_BYTES + 1
    ));

    let mut closure_bytes = MAX_FROZEN_EXECUTABLE_CLOSURE_ID_BYTES - package.as_str().len();
    account_frozen_closure_id_bytes(&package, &mut closure_bytes).unwrap();
    assert_eq!(closure_bytes, MAX_FROZEN_EXECUTABLE_CLOSURE_ID_BYTES);
    assert!(matches!(
        account_frozen_closure_id_bytes(&package, &mut closure_bytes),
        Err(Error::FrozenExecutableClosureIdByteLimit { limit, actual })
            if limit == MAX_FROZEN_EXECUTABLE_CLOSURE_ID_BYTES
                && actual == MAX_FROZEN_EXECUTABLE_CLOSURE_ID_BYTES + package.as_str().len()
    ));

    let mut binding_bytes = MAX_TOTAL_FROZEN_EXECUTABLE_BINDING_BYTES - 1;
    account_frozen_binding_bytes(&binding, 1, &mut binding_bytes).unwrap();
    assert_eq!(binding_bytes, MAX_TOTAL_FROZEN_EXECUTABLE_BINDING_BYTES);
    assert!(matches!(
        account_frozen_binding_bytes(&binding, 1, &mut binding_bytes),
        Err(Error::FrozenExecutableBindingByteLimit { limit, actual, .. })
            if limit == MAX_TOTAL_FROZEN_EXECUTABLE_BINDING_BYTES
                && actual == MAX_TOTAL_FROZEN_EXECUTABLE_BINDING_BYTES + 1
    ));

    assert!(require_frozen_executable_layout_count(MAX_FROZEN_EXECUTABLE_LAYOUTS).is_ok());
    assert!(matches!(
        require_frozen_executable_layout_count(MAX_FROZEN_EXECUTABLE_LAYOUTS + 1),
        Err(Error::FrozenExecutableLayoutLimit { limit, actual })
            if limit == MAX_FROZEN_EXECUTABLE_LAYOUTS
                && actual == MAX_FROZEN_EXECUTABLE_LAYOUTS + 1
    ));
    let mut layout_bytes = MAX_TOTAL_FROZEN_EXECUTABLE_LAYOUT_BYTES - 1;
    account_frozen_layout_bytes(&package, &binding.path, 1, &mut layout_bytes).unwrap();
    assert_eq!(layout_bytes, MAX_TOTAL_FROZEN_EXECUTABLE_LAYOUT_BYTES);
    assert!(matches!(
        account_frozen_layout_bytes(&package, &binding.path, 1, &mut layout_bytes),
        Err(Error::FrozenExecutableLayoutByteLimit { limit, actual, .. })
            if limit == MAX_TOTAL_FROZEN_EXECUTABLE_LAYOUT_BYTES
                && actual == MAX_TOTAL_FROZEN_EXECUTABLE_LAYOUT_BYTES + 1
    ));

    assert!(require_frozen_executable_directory_count(MAX_FROZEN_EXECUTABLE_DIRECTORY_PATHS).is_ok());
    assert!(matches!(
        require_frozen_executable_directory_count(MAX_FROZEN_EXECUTABLE_DIRECTORY_PATHS + 1),
        Err(Error::FrozenExecutableDirectoryLimit { limit, actual })
            if limit == MAX_FROZEN_EXECUTABLE_DIRECTORY_PATHS
                && actual == MAX_FROZEN_EXECUTABLE_DIRECTORY_PATHS + 1
    ));
    let mut directory_bytes = MAX_TOTAL_FROZEN_EXECUTABLE_DIRECTORY_BYTES - 1;
    account_frozen_executable_directory_bytes(1, &mut directory_bytes).unwrap();
    assert_eq!(directory_bytes, MAX_TOTAL_FROZEN_EXECUTABLE_DIRECTORY_BYTES);
    assert!(matches!(
        account_frozen_executable_directory_bytes(1, &mut directory_bytes),
        Err(Error::FrozenExecutableDirectoryByteLimit { limit, actual })
            if limit == MAX_TOTAL_FROZEN_EXECUTABLE_DIRECTORY_BYTES
                && actual == MAX_TOTAL_FROZEN_EXECUTABLE_DIRECTORY_BYTES + 1
    ));

    let mut total = MAX_TOTAL_FROZEN_EXECUTABLE_BYTES - MAX_FROZEN_EXECUTABLE_BYTES;
    account_frozen_executable_bytes(&binding, MAX_FROZEN_EXECUTABLE_BYTES, &mut total).unwrap();
    assert_eq!(total, MAX_TOTAL_FROZEN_EXECUTABLE_BYTES);

    let mut empty = 0;
    assert!(matches!(
        account_frozen_executable_bytes(&binding, MAX_FROZEN_EXECUTABLE_BYTES + 1, &mut empty),
        Err(Error::FrozenExecutableByteLimit { limit, actual, .. })
            if limit == MAX_FROZEN_EXECUTABLE_BYTES
                && actual == MAX_FROZEN_EXECUTABLE_BYTES + 1
    ));

    let mut full = MAX_TOTAL_FROZEN_EXECUTABLE_BYTES;
    assert!(matches!(
        account_frozen_executable_bytes(&binding, 1, &mut full),
        Err(Error::FrozenExecutableTotalByteLimit { limit, actual })
            if limit == MAX_TOTAL_FROZEN_EXECUTABLE_BYTES
                && actual == MAX_TOTAL_FROZEN_EXECUTABLE_BYTES + 1
    ));
    assert_eq!(full, MAX_TOTAL_FROZEN_EXECUTABLE_BYTES);

    assert!(require_frozen_executable_deadline(Instant::now() + Duration::from_secs(1)).is_ok());
    assert!(matches!(
        require_frozen_executable_deadline(Instant::now() - Duration::from_secs(1)),
        Err(Error::FrozenExecutableVerificationTimeout { .. })
    ));
    assert!(require_frozen_materialization_deadline(Instant::now() + Duration::from_secs(1)).is_ok());
    assert!(matches!(
        require_frozen_materialization_deadline(Instant::now() - Duration::from_secs(1)),
        Err(Error::FrozenMaterializationTimeout { .. })
    ));
}

#[test]
fn frozen_binding_paths_preflight_raw_bounds_before_provider_lookup_or_copy() {
    let package = package::Id::from("inside-provider");
    let outside = package::Id::from("outside-provider");

    let accepted = FrozenExecutableBinding {
        package: package.clone(),
        path: PathBuf::from(format!(
            "/{}",
            std::iter::once("usr")
                .chain(std::iter::repeat_n("a", MAX_FROZEN_LAYOUT_PATH_COMPONENTS - 1))
                .join("/")
        )),
    };
    assert_eq!(
        accepted.path.components().count(),
        MAX_FROZEN_LAYOUT_PATH_COMPONENTS + 1
    );
    require_frozen_executable_path(&accepted).unwrap();

    let too_deep = FrozenExecutableBinding {
        package: package.clone(),
        path: PathBuf::from(format!("{}/a", accepted.path.display())),
    };
    assert!(matches!(
        require_frozen_executable_path(&too_deep),
        Err(Error::FrozenExecutablePathDepthLimit { limit, actual })
            if limit == MAX_FROZEN_LAYOUT_PATH_COMPONENTS
                && actual == MAX_FROZEN_LAYOUT_PATH_COMPONENTS + 1
    ));

    let invalid_utf8 = FrozenExecutableBinding {
        package: package.clone(),
        path: PathBuf::from(OsString::from_vec(b"/usr/bin/\xff".to_vec())),
    };
    assert!(matches!(
        require_frozen_executable_path(&invalid_utf8),
        Err(Error::FrozenExecutablePathEncoding { bytes }) if bytes == b"/usr/bin/\xff".len()
    ));

    let mut oversized_non_utf8 = vec![b'a'; MAX_FROZEN_EXECUTABLE_PATH_BYTES + 1];
    oversized_non_utf8[0] = 0xff;
    let oversized = FrozenExecutableBinding {
        package: outside,
        path: PathBuf::from(OsString::from_vec(oversized_non_utf8)),
    };
    assert!(matches!(
        require_frozen_executable_path(&oversized),
        Err(Error::FrozenExecutablePathByteLimit { limit, actual })
            if limit == MAX_FROZEN_EXECUTABLE_PATH_BYTES
                && actual == MAX_FROZEN_EXECUTABLE_PATH_BYTES + 1
    ));

    let temporary = tempfile::tempdir().unwrap();
    let installation_root = temporary.path().join("installation");
    let frozen_root = temporary.path().join("frozen-root");
    fs::create_dir(&installation_root).unwrap();
    fs::create_dir(&frozen_root).unwrap();
    let client = Client::frozen(
        "frozen-raw-binding-preflight-test",
        frozen_test_installation(&installation_root),
        repository::Map::default(),
        &frozen_root,
    )
    .unwrap();
    assert!(matches!(
        client.require_frozen_executables(std::slice::from_ref(&package), &[oversized]),
        Err(Error::FrozenExecutablePathByteLimit { .. })
    ));
}

#[test]
fn empty_frozen_binding_set_returns_a_live_root_guard_and_detects_substitution() {
    let temporary = tempfile::tempdir().unwrap();
    let installation_root = temporary.path().join("installation");
    let frozen_root = temporary.path().join("frozen-root");
    fs::create_dir(&installation_root).unwrap();
    fs::create_dir(&frozen_root).unwrap();
    let client = Client::frozen(
        "empty-frozen-root-guard-test",
        frozen_test_installation(&installation_root),
        repository::Map::default(),
        &frozen_root,
    )
    .unwrap();
    let package = package::Id::from("guard-provider");
    let guard = client
        .require_frozen_executables(std::slice::from_ref(&package), &[])
        .unwrap();
    drop(client);

    assert_eq!(guard.root_path(), frozen_root);
    assert!(guard.revalidated_anchor().unwrap().as_raw_fd() >= 0);

    let moved = temporary.path().join("moved-frozen-root");
    fs::rename(&frozen_root, &moved).unwrap();
    fs::create_dir(&frozen_root).unwrap();
    assert!(matches!(
        guard.revalidate(),
        Err(Error::FrozenExecutableRootReplaced(path)) if path == frozen_root
    ));
}

#[test]
fn frozen_shebang_limits_accept_n_and_reject_n_plus_one() {
    let prefix = "/usr/bin/";
    let accepted_path = format!(
        "{prefix}{}",
        "a".repeat(MAX_FROZEN_SHEBANG_INTERPRETER_BYTES - prefix.len())
    );
    assert_eq!(accepted_path.len(), MAX_FROZEN_SHEBANG_INTERPRETER_BYTES);
    let accepted = format!("#!{accepted_path}\n");
    assert_eq!(accepted.len(), MAX_FROZEN_SHEBANG_LINE_BYTES);
    assert_eq!(
        parse_frozen_shebang(accepted.as_bytes()).unwrap(),
        Some(FrozenShebangInterpreter {
            path: PathBuf::from(accepted_path),
            root_alias: None,
        })
    );

    let rejected_path = format!(
        "{prefix}{}",
        "a".repeat(MAX_FROZEN_SHEBANG_INTERPRETER_BYTES + 1 - prefix.len())
    );
    let rejected = format!("#!{rejected_path}\n");
    assert_eq!(rejected.len(), MAX_FROZEN_SHEBANG_LINE_BYTES + 1);
    assert_eq!(
        parse_frozen_shebang(rejected.as_bytes()),
        Err(FrozenShebangParseError::LineTooLong)
    );

    let padded_path = format!(
        "{prefix}{}",
        "a".repeat(MAX_FROZEN_SHEBANG_INTERPRETER_BYTES - prefix.len() - 2)
    );
    let padded = format!("#! {padded_path} \n");
    assert_eq!(padded.len(), MAX_FROZEN_SHEBANG_LINE_BYTES);
    assert_eq!(
        parse_frozen_shebang(padded.as_bytes()).unwrap(),
        Some(FrozenShebangInterpreter {
            path: PathBuf::from(padded_path.clone()),
            root_alias: None,
        })
    );
    let padded_rejected = format!("#!  {padded_path} \n");
    assert_eq!(padded_rejected.len(), MAX_FROZEN_SHEBANG_LINE_BYTES + 1);
    assert_eq!(
        parse_frozen_shebang(padded_rejected.as_bytes()),
        Err(FrozenShebangParseError::LineTooLong)
    );

    let binding = FrozenExecutableBinding {
        package: package::Id::from("script-provider"),
        path: PathBuf::from("/usr/bin/script"),
    };
    require_frozen_shebang_interpreter_count(&binding, MAX_FROZEN_SHEBANG_INTERPRETERS).unwrap();
    assert!(matches!(
        require_frozen_shebang_interpreter_count(&binding, MAX_FROZEN_SHEBANG_INTERPRETERS + 1),
        Err(Error::FrozenShebangInterpreterLimit { package, path, limit })
            if package == binding.package
                && path == binding.path
                && limit == MAX_FROZEN_SHEBANG_INTERPRETERS
    ));
    require_frozen_executable_interpreter_count(&binding, MAX_FROZEN_EXECUTABLE_INTERPRETERS).unwrap();
    assert!(matches!(
        require_frozen_executable_interpreter_count(&binding, MAX_FROZEN_EXECUTABLE_INTERPRETERS + 1),
        Err(Error::FrozenExecutableInterpreterLimit { package, path, limit })
            if package == binding.package
                && path == binding.path
                && limit == MAX_FROZEN_EXECUTABLE_INTERPRETERS
    ));

    let mut pinned = MAX_FROZEN_EXECUTABLE_PINNED_FILES - 1;
    reserve_frozen_pinned_files(&binding, &mut pinned, 1).unwrap();
    assert_eq!(pinned, MAX_FROZEN_EXECUTABLE_PINNED_FILES);
    assert!(matches!(
        reserve_frozen_pinned_files(&binding, &mut pinned, 1),
        Err(Error::FrozenExecutablePinnedFileLimit { package, path, limit, actual })
            if package == binding.package
                && path == binding.path
                && limit == MAX_FROZEN_EXECUTABLE_PINNED_FILES
                && actual == MAX_FROZEN_EXECUTABLE_PINNED_FILES + 1
    ));
    assert_eq!(pinned, MAX_FROZEN_EXECUTABLE_PINNED_FILES);
}

#[test]
fn frozen_shebang_depth_matches_the_linux_execve_boundary() {
    fn install_chain(root: &Path, depth: usize) -> PathBuf {
        let paths = (0..depth)
            .map(|index| root.join(format!("script-{index}")))
            .collect::<Vec<_>>();
        for (index, path) in paths.iter().enumerate() {
            let interpreter = paths
                .get(index + 1)
                .cloned()
                .unwrap_or_else(|| PathBuf::from("/bin/true"));
            fs::write(path, format!("#!{}\n", interpreter.display())).unwrap();
            fs::set_permissions(path, Permissions::from_mode(0o755)).unwrap();
        }
        paths.into_iter().next().unwrap()
    }

    assert_eq!(MAX_FROZEN_SHEBANG_INTERPRETERS, 5);
    // Put scripts beside the running test binary rather than in /tmp;
    // hardened CI commonly mounts /tmp noexec, while this filesystem is
    // proven executable by the current process.
    let executable_directory = std::env::current_exe().unwrap();
    let executable_directory = executable_directory.parent().unwrap();
    let accepted = tempfile::Builder::new()
        .prefix("forge-shebang-accepted-")
        .tempdir_in(executable_directory)
        .unwrap();
    let accepted_entry = install_chain(accepted.path(), MAX_FROZEN_SHEBANG_INTERPRETERS);
    assert!(Command::new(accepted_entry).status().unwrap().success());

    let rejected = tempfile::Builder::new()
        .prefix("forge-shebang-rejected-")
        .tempdir_in(executable_directory)
        .unwrap();
    let rejected_entry = install_chain(rejected.path(), MAX_FROZEN_SHEBANG_INTERPRETERS + 1);
    let error = Command::new(rejected_entry).status().unwrap_err();
    assert_eq!(error.raw_os_error(), Some(nix::libc::ELOOP));
}

#[test]
fn frozen_shebang_parser_accepts_only_one_absolute_frozen_path() {
    assert_eq!(
        parse_frozen_shebang(b"#!/bin/sh\necho ok\n").unwrap(),
        Some(FrozenShebangInterpreter {
            path: PathBuf::from("/usr/bin/sh"),
            root_alias: Some(ExpectedFrozenRootAlias {
                path: PathBuf::from("/bin"),
                target: "usr/bin".to_owned(),
            }),
        })
    );
    assert_eq!(parse_frozen_shebang(b"\x7fELFnot-a-script").unwrap(), None);
    assert_eq!(
        parse_frozen_shebang(b"#! /usr/bin/perl\n").unwrap(),
        Some(FrozenShebangInterpreter {
            path: PathBuf::from("/usr/bin/perl"),
            root_alias: None,
        })
    );
    assert_eq!(
        parse_frozen_shebang(b"#! \t/usr/bin/perl \t\n").unwrap(),
        Some(FrozenShebangInterpreter {
            path: PathBuf::from("/usr/bin/perl"),
            root_alias: None,
        })
    );
    assert_eq!(
        parse_frozen_shebang(b"#!   \t\n"),
        Err(FrozenShebangParseError::EmptyInterpreter)
    );
    assert_eq!(
        parse_frozen_shebang(b"#!/usr/bin/env\n"),
        Err(FrozenShebangParseError::EnvironmentLookup)
    );
    assert_eq!(
        parse_frozen_shebang(b"#! /bin/env \t\n"),
        Err(FrozenShebangParseError::EnvironmentLookup)
    );
    assert_eq!(
        parse_frozen_shebang(b"#!/usr/bin/env bash\n"),
        Err(FrozenShebangParseError::InterpreterOptions)
    );
    assert_eq!(
        parse_frozen_shebang(b"#!/usr/bin/env -S bash -e\n"),
        Err(FrozenShebangParseError::InterpreterOptions)
    );
    assert_eq!(
        parse_frozen_shebang(b"#!/usr/bin/bash -e\n"),
        Err(FrozenShebangParseError::InterpreterOptions)
    );
    assert_eq!(
        parse_frozen_shebang(b"#!/usr/bin/bash\r\n"),
        Err(FrozenShebangParseError::UnsupportedWhitespace)
    );
    assert_eq!(
        parse_frozen_shebang(b"#!relative/interpreter\n"),
        Err(FrozenShebangParseError::Relative)
    );
    assert_eq!(
        parse_frozen_shebang(b"#!/usr/bin/ba\0sh\n"),
        Err(FrozenShebangParseError::Nul)
    );
    assert_eq!(
        parse_frozen_shebang(b"#!/usr/bin/bash"),
        Err(FrozenShebangParseError::Unterminated)
    );
    assert_eq!(
        parse_frozen_shebang(b"#!/usr/bin/../bin/bash\n"),
        Err(FrozenShebangParseError::NonNormalized)
    );
}
