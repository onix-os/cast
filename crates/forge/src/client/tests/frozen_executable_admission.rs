#[test]
fn frozen_executable_format_rejects_shell_fallback_and_unknown_binfmt_inputs() {
    let binding = FrozenExecutableBinding {
        package: package::Id::from("format-provider"),
        path: PathBuf::from("/usr/bin/tool"),
    };
    for bytes in [
        b"echo this must never reach a shell\n".as_slice(),
        b"MZunknown-binfmt-input".as_slice(),
        b"".as_slice(),
    ] {
        assert!(matches!(
            inspect_test_executable(bytes, &binding),
            Err(Error::InvalidFrozenExecutableFormat { package, path, .. })
                if package == binding.package && path == binding.path
        ));
    }
    assert!(matches!(
        inspect_test_executable(b"#!/usr/bin/sh", &binding),
        Err(Error::InvalidFrozenShebang { package, path, .. })
            if package == binding.package && path == binding.path
    ));
}

#[test]
fn frozen_elf_admission_is_structural_and_binds_pt_interp() {
    let binding = FrozenExecutableBinding {
        package: package::Id::from("elf-provider"),
        path: PathBuf::from("/usr/bin/elf-tool"),
    };
    assert_eq!(inspect_test_executable(&test_elf(None, 1), &binding).unwrap(), None);
    assert_eq!(
        inspect_test_executable(&test_elf(Some("/lib64/ld-frozen.so"), 2), &binding).unwrap(),
        Some(FrozenExecutableInterpreter::Elf(FrozenShebangInterpreter {
            path: PathBuf::from("/usr/lib/ld-frozen.so"),
            root_alias: Some(ExpectedFrozenRootAlias {
                path: PathBuf::from("/lib64"),
                target: "usr/lib".to_owned(),
            }),
        }))
    );

    let mut truncated = test_elf(None, 1);
    truncated.truncate(16);
    assert!(matches!(
        inspect_test_executable(&truncated, &binding),
        Err(Error::InvalidFrozenExecutableFormat { .. })
    ));

    let mut wrong_machine = test_elf(None, 1);
    test_elf_write_u16(&mut wrong_machine, 18, 0, cfg!(target_endian = "little"));
    assert!(matches!(
        inspect_test_executable(&wrong_machine, &binding),
        Err(Error::InvalidFrozenExecutableFormat { .. })
    ));

    let mut unterminated_interp = test_elf(Some("/lib64/ld-frozen.so"), 2);
    *unterminated_interp.last_mut().unwrap() = b'x';
    assert!(matches!(
        inspect_test_executable(&unterminated_interp, &binding),
        Err(Error::InvalidFrozenExecutableFormat { .. })
    ));
}

#[test]
fn frozen_elf_rejects_malformed_headers_segments_and_interpreters() {
    let binding = FrozenExecutableBinding {
        package: package::Id::from("malformed-elf-provider"),
        path: PathBuf::from("/usr/bin/malformed-elf"),
    };
    let little_endian = cfg!(target_endian = "little");
    let class64 = usize::BITS == 64;
    let header_size = if class64 { 64 } else { 52 };
    let program_header_size = if class64 { 56 } else { 32 };
    let assert_invalid = |bytes: &[u8]| {
        assert!(matches!(
            inspect_test_executable(bytes, &binding),
            Err(Error::InvalidFrozenExecutableFormat { package, path, .. })
                if package == binding.package && path == binding.path
        ));
    };

    let mut relative_interp = test_elf(Some("relative/loader"), 2);
    assert_invalid(&relative_interp);
    let interpreter_offset = header_size + 2 * program_header_size;
    relative_interp[interpreter_offset + 1] = 0;
    assert_invalid(&relative_interp);

    let mut relocatable = test_elf(None, 1);
    test_elf_write_u16(&mut relocatable, 16, 1, little_endian);
    assert_invalid(&relocatable);

    let mut wrong_class = test_elf(None, 1);
    wrong_class[4] = if class64 { 1 } else { 2 };
    assert_invalid(&wrong_class);

    let mut no_program_headers = test_elf(None, 1);
    test_elf_write_u16(&mut no_program_headers, if class64 { 56 } else { 44 }, 0, little_endian);
    assert_invalid(&no_program_headers);

    let mut table_past_eof = test_elf(None, 1);
    let table_offset = table_past_eof.len() as u64 + 1;
    if class64 {
        test_elf_write_u64(&mut table_past_eof, 32, table_offset, little_endian);
    } else {
        test_elf_write_u32(&mut table_past_eof, 28, table_offset as u32, little_endian);
    }
    assert_invalid(&table_past_eof);

    let load = header_size;
    let mut non_executable_load = test_elf(None, 1);
    test_elf_write_u32(
        &mut non_executable_load,
        load + if class64 { 4 } else { 24 },
        4,
        little_endian,
    );
    assert_invalid(&non_executable_load);

    let mut segment_past_eof = test_elf(None, 1);
    let oversized = segment_past_eof.len() as u64 + 1;
    if class64 {
        test_elf_write_u64(&mut segment_past_eof, load + 32, oversized, little_endian);
    } else {
        test_elf_write_u32(&mut segment_past_eof, load + 16, oversized as u32, little_endian);
    }
    assert_invalid(&segment_past_eof);

    let mut memory_smaller_than_file = test_elf(None, 1);
    if class64 {
        test_elf_write_u64(&mut memory_smaller_than_file, load + 40, 1, little_endian);
    } else {
        test_elf_write_u32(&mut memory_smaller_than_file, load + 20, 1, little_endian);
    }
    assert_invalid(&memory_smaller_than_file);

    let mut invalid_alignment = test_elf(None, 1);
    if class64 {
        test_elf_write_u64(&mut invalid_alignment, load + 48, 3, little_endian);
    } else {
        test_elf_write_u32(&mut invalid_alignment, load + 28, 3, little_endian);
    }
    assert_invalid(&invalid_alignment);

    let mut duplicate_interp = test_elf(Some("/lib64/ld-frozen.so"), 3);
    let first_interp = header_size + program_header_size;
    let duplicate = first_interp + program_header_size;
    let header = duplicate_interp[first_interp..first_interp + program_header_size].to_vec();
    duplicate_interp[duplicate..duplicate + program_header_size].copy_from_slice(&header);
    assert_invalid(&duplicate_interp);

    let mut one_byte_interp = test_elf(Some("/lib64/ld-frozen.so"), 2);
    let interp_header = header_size + program_header_size;
    if class64 {
        test_elf_write_u64(&mut one_byte_interp, interp_header + 32, 1, little_endian);
    } else {
        test_elf_write_u32(&mut one_byte_interp, interp_header + 16, 1, little_endian);
    }
    assert_invalid(&one_byte_interp);
}

#[test]
fn frozen_elf_admission_parses_the_host_binary_and_confines_its_interp() {
    let binding = FrozenExecutableBinding {
        package: package::Id::from("host-elf-provider"),
        path: PathBuf::from("/usr/bin/host-elf"),
    };
    let mut file = fs::File::open(std::env::current_exe().unwrap()).unwrap();
    let length = file.metadata().unwrap().len();
    let mut probe = vec![0; MAX_FROZEN_SHEBANG_LINE_BYTES + 1];
    let read = file.read(&mut probe).unwrap();
    probe.truncate(read);
    match inspect_frozen_executable_format(
        &file,
        length,
        &probe,
        Instant::now() + Duration::from_secs(10),
        &binding,
    ) {
        Ok(None | Some(FrozenExecutableInterpreter::Elf(_))) => {}
        // Nix-linked test binaries deliberately name a store interpreter,
        // which a /usr-only frozen root must reject after successfully
        // parsing the real ELF and its PT_INTERP segment.
        Err(Error::InvalidFrozenExecutableFormat {
            reason: "ELF PT_INTERP path is not absolute and normalized",
            ..
        }) => {}
        result => panic!("unexpected host ELF admission result: {result:?}"),
    }
}

#[test]
fn frozen_elf_program_header_limit_accepts_n_and_rejects_n_plus_one() {
    let binding = FrozenExecutableBinding {
        package: package::Id::from("elf-limit-provider"),
        path: PathBuf::from("/usr/bin/elf-limit"),
    };
    inspect_test_executable(&test_elf(None, MAX_FROZEN_ELF_PROGRAM_HEADERS), &binding).unwrap();
    assert!(matches!(
        inspect_test_executable(&test_elf(None, MAX_FROZEN_ELF_PROGRAM_HEADERS + 1), &binding),
        Err(Error::FrozenElfProgramHeaderLimit { package, path, limit, actual })
            if package == binding.package
                && path == binding.path
                && limit == MAX_FROZEN_ELF_PROGRAM_HEADERS
                && actual == MAX_FROZEN_ELF_PROGRAM_HEADERS + 1
    ));
}

#[test]
fn frozen_interpreter_layout_requires_one_provider_and_confined_symlinks() {
    let link_provider = package::Id::from("link-provider");
    let regular_provider = package::Id::from("regular-provider");
    let link = PathBuf::from("/usr/bin/interpreter");
    let regular = PathBuf::from("/usr/bin/interpreter-real");
    let mut layouts = BTreeMap::from([
        (
            link_provider.clone(),
            BTreeMap::from([(
                link.clone(),
                FrozenExecutableLayout::Symlink {
                    target: "interpreter-real".to_owned(),
                    mode: nix::libc::S_IFLNK | 0o777,
                },
            )]),
        ),
        (
            regular_provider.clone(),
            BTreeMap::from([(
                regular.clone(),
                FrozenExecutableLayout::Regular {
                    digest: 7,
                    mode: nix::libc::S_IFREG | 0o755,
                },
            )]),
        ),
    ]);
    let mut providers = BTreeMap::from([
        (link.clone(), BTreeSet::from([link_provider.clone()])),
        (regular.clone(), BTreeSet::from([regular_provider.clone()])),
    ]);
    let redirects = BTreeMap::new();
    let deadline = Instant::now() + Duration::from_secs(1);

    let (binding, expected) =
        resolve_frozen_interpreter_layout(&link, &layouts, &providers, &redirects, deadline).unwrap();
    assert_eq!(binding.package, regular_provider);
    assert_eq!(binding.path, link);
    assert_eq!(expected.resolved_path, regular);
    assert_eq!(expected.symlinks.len(), 1);
    assert_eq!(expected.symlinks[0].package, link_provider);

    let missing = PathBuf::from("/usr/bin/missing");
    assert!(matches!(
        resolve_frozen_interpreter_layout(&missing, &layouts, &providers, &redirects, deadline),
        Err(Error::MissingFrozenInterpreterProvider { path }) if path == missing
    ));

    let ambiguous = PathBuf::from("/usr/bin/ambiguous");
    for provider in [&link_provider, &regular_provider] {
        layouts.get_mut(provider).unwrap().insert(
            ambiguous.clone(),
            FrozenExecutableLayout::Regular {
                digest: 8,
                mode: nix::libc::S_IFREG | 0o755,
            },
        );
    }
    providers.insert(
        ambiguous.clone(),
        BTreeSet::from([link_provider.clone(), regular_provider.clone()]),
    );
    assert!(matches!(
        resolve_frozen_interpreter_layout(&ambiguous, &layouts, &providers, &redirects, deadline),
        Err(Error::AmbiguousFrozenInterpreterProvider { path, providers })
            if path == ambiguous && providers.len() == 2
    ));

    layouts.get_mut(&link_provider).unwrap().insert(
        link.clone(),
        FrozenExecutableLayout::Symlink {
            target: "../../../etc/passwd".to_owned(),
            mode: nix::libc::S_IFLNK | 0o777,
        },
    );
    assert!(matches!(
        resolve_frozen_interpreter_layout(&link, &layouts, &providers, &redirects, deadline),
        Err(Error::InvalidFrozenExecutableSymlinkTarget { package, path, .. })
            if package == link_provider && path == link
    ));

    let cycle_a = PathBuf::from("/usr/bin/cycle-a");
    let cycle_b = PathBuf::from("/usr/bin/cycle-b");
    layouts.insert(
        link_provider.clone(),
        BTreeMap::from([
            (
                cycle_a.clone(),
                FrozenExecutableLayout::Symlink {
                    target: "cycle-b".to_owned(),
                    mode: nix::libc::S_IFLNK | 0o777,
                },
            ),
            (
                cycle_b.clone(),
                FrozenExecutableLayout::Symlink {
                    target: "cycle-a".to_owned(),
                    mode: nix::libc::S_IFLNK | 0o777,
                },
            ),
        ]),
    );
    providers.insert(cycle_a.clone(), BTreeSet::from([link_provider.clone()]));
    providers.insert(cycle_b, BTreeSet::from([link_provider]));
    assert!(matches!(
        resolve_frozen_interpreter_layout(&cycle_a, &layouts, &providers, &redirects, deadline),
        Err(Error::FrozenInterpreterSymlinkCycle { path }) if path == cycle_a
    ));
}

#[test]
fn frozen_executables_explicitly_reject_materialized_directory_redirects() {
    let package = package::Id::from("redirect-provider");
    let source = PathBuf::from("/usr/lib/redirect");
    let target = PathBuf::from("/usr/lib/real");
    let logical_tool = source.join("tool");
    let prepared = vec![
        PreparedFrozenExecutableLayout {
            package: package.clone(),
            path: source.clone(),
            entry: FrozenExecutableLayout::Symlink {
                target: target.to_string_lossy().into_owned(),
                mode: nix::libc::S_IFLNK | 0o777,
            },
            is_directory: false,
        },
        PreparedFrozenExecutableLayout {
            package: package.clone(),
            path: target.clone(),
            entry: FrozenExecutableLayout::Other,
            is_directory: true,
        },
        PreparedFrozenExecutableLayout {
            package: package.clone(),
            path: logical_tool.clone(),
            entry: FrozenExecutableLayout::Regular {
                digest: 7,
                mode: nix::libc::S_IFREG | 0o755,
            },
            is_directory: false,
        },
    ];
    let redirects = frozen_executable_directory_redirects(&prepared, Instant::now() + Duration::from_secs(1)).unwrap();
    assert_eq!(redirects.get(&source), Some(&target));

    let layouts = BTreeMap::from([(
        logical_tool.clone(),
        FrozenExecutableLayout::Regular {
            digest: 7,
            mode: nix::libc::S_IFREG | 0o755,
        },
    )]);
    let binding = FrozenExecutableBinding {
        package: package.clone(),
        path: logical_tool.clone(),
    };
    let provider_layouts = BTreeMap::from([(package.clone(), layouts)]);
    let path_providers = BTreeMap::from([(logical_tool.clone(), BTreeSet::from([package]))]);
    assert!(matches!(
        resolve_frozen_executable_layout(
            &binding,
            &provider_layouts,
            &path_providers,
            &redirects,
            Instant::now() + Duration::from_secs(1),
        ),
        Err(Error::FrozenExecutableDirectoryRedirect {
            path,
            redirect_source,
            target: actual_target,
        }) if path == logical_tool
            && redirect_source.as_path() == source
            && actual_target.as_path() == target
    ));
}

#[test]
fn frozen_executable_symlink_targets_are_resolved_lexically_beneath_usr() {
    let link = Path::new("/usr/bin/tool");
    assert_eq!(
        resolve_frozen_symlink_target(link, "tool-1"),
        Some(PathBuf::from("/usr/bin/tool-1"))
    );
    assert_eq!(
        resolve_frozen_symlink_target(link, "../libexec/tool-1"),
        Some(PathBuf::from("/usr/libexec/tool-1"))
    );
    assert_eq!(
        resolve_frozen_symlink_target(link, "/usr/libexec/tool-1"),
        Some(PathBuf::from("/usr/libexec/tool-1"))
    );
    assert_eq!(resolve_frozen_symlink_target(link, "../../etc/passwd"), None);
    assert_eq!(resolve_frozen_symlink_target(link, "/etc/passwd"), None);
    assert_eq!(resolve_frozen_symlink_target(link, "tool-1/"), None);
    assert_eq!(resolve_frozen_symlink_target(link, "tool//1"), None);
}

#[test]
fn frozen_executable_symlink_handoff_requires_one_closure_provider() {
    let entry_provider = package::Id::from("entry-provider");
    let target_provider = package::Id::from("target-provider");
    let duplicate_provider = package::Id::from("duplicate-provider");
    let entry = PathBuf::from("/usr/bin/tool");
    let target = PathBuf::from("/usr/bin/tool-real");
    let binding = FrozenExecutableBinding {
        package: entry_provider.clone(),
        path: entry.clone(),
    };
    let provider_layouts = BTreeMap::from([
        (
            entry_provider.clone(),
            BTreeMap::from([(
                entry.clone(),
                FrozenExecutableLayout::Symlink {
                    target: "tool-real".to_owned(),
                    mode: nix::libc::S_IFLNK | 0o777,
                },
            )]),
        ),
        (
            target_provider.clone(),
            BTreeMap::from([(
                target.clone(),
                FrozenExecutableLayout::Regular {
                    digest: 7,
                    mode: nix::libc::S_IFREG | 0o755,
                },
            )]),
        ),
    ]);
    let mut path_providers = BTreeMap::from([
        (entry.clone(), BTreeSet::from([entry_provider.clone()])),
        (target.clone(), BTreeSet::from([target_provider.clone()])),
    ]);
    let redirects = BTreeMap::new();
    let deadline = Instant::now() + Duration::from_secs(1);

    let expected =
        resolve_frozen_executable_layout(&binding, &provider_layouts, &path_providers, &redirects, deadline).unwrap();
    assert_eq!(expected.resolved_path, target);
    assert_eq!(expected.symlinks.len(), 1);
    assert_eq!(expected.symlinks[0].package, entry_provider);

    path_providers.remove(&target);
    assert!(matches!(
        resolve_frozen_executable_layout(
            &binding,
            &provider_layouts,
            &path_providers,
            &redirects,
            deadline,
        ),
        Err(Error::MissingFrozenExecutableSymlinkTarget { package, binding: path, target: missing })
            if package == binding.package && path == binding.path && missing == target
    ));

    path_providers.insert(
        target.clone(),
        BTreeSet::from([target_provider.clone(), duplicate_provider.clone()]),
    );
    assert!(matches!(
        resolve_frozen_executable_layout(
            &binding,
            &provider_layouts,
            &path_providers,
            &redirects,
            deadline,
        ),
        Err(Error::AmbiguousFrozenExecutableSymlinkTarget {
            package,
            binding: path,
            target: ambiguous,
            providers,
        }) if package == binding.package
            && path == binding.path
            && ambiguous == target
            && providers == vec![duplicate_provider, target_provider]
    ));
}

#[test]
fn frozen_executable_symlink_chain_accepts_n_and_rejects_n_plus_one() {
    let binding = FrozenExecutableBinding {
        package: package::Id::from("symlink-chain-provider"),
        path: PathBuf::from("/usr/bin/link-0"),
    };
    let mut layouts = BTreeMap::new();
    for index in 0..MAX_FROZEN_EXECUTABLE_SYMLINKS {
        layouts.insert(
            PathBuf::from(format!("/usr/bin/link-{index}")),
            FrozenExecutableLayout::Symlink {
                target: format!("link-{}", index + 1),
                mode: nix::libc::S_IFLNK | 0o777,
            },
        );
    }
    let final_path = PathBuf::from(format!("/usr/bin/link-{MAX_FROZEN_EXECUTABLE_SYMLINKS}"));
    layouts.insert(
        final_path.clone(),
        FrozenExecutableLayout::Regular {
            digest: 7,
            mode: nix::libc::S_IFREG | 0o755,
        },
    );
    let redirects = BTreeMap::new();
    let deadline = Instant::now() + Duration::from_secs(1);
    let provider_layouts = BTreeMap::from([(binding.package.clone(), layouts.clone())]);
    let path_providers = layouts
        .keys()
        .cloned()
        .map(|path| (path, BTreeSet::from([binding.package.clone()])))
        .collect();
    let expected =
        resolve_frozen_executable_layout(&binding, &provider_layouts, &path_providers, &redirects, deadline).unwrap();
    assert_eq!(expected.symlinks.len(), MAX_FROZEN_EXECUTABLE_SYMLINKS);
    assert_eq!(expected.resolved_path, final_path);

    layouts.insert(
        final_path,
        FrozenExecutableLayout::Symlink {
            target: format!("link-{}", MAX_FROZEN_EXECUTABLE_SYMLINKS + 1),
            mode: nix::libc::S_IFLNK | 0o777,
        },
    );
    layouts.insert(
        PathBuf::from(format!("/usr/bin/link-{}", MAX_FROZEN_EXECUTABLE_SYMLINKS + 1)),
        FrozenExecutableLayout::Regular {
            digest: 7,
            mode: nix::libc::S_IFREG | 0o755,
        },
    );
    let provider_layouts = BTreeMap::from([(binding.package.clone(), layouts.clone())]);
    let path_providers = layouts
        .keys()
        .cloned()
        .map(|path| (path, BTreeSet::from([binding.package.clone()])))
        .collect();
    assert!(matches!(
        resolve_frozen_executable_layout(
            &binding,
            &provider_layouts,
            &path_providers,
            &redirects,
            deadline,
        ),
        Err(Error::FrozenExecutableSymlinkLimit { package, path, limit })
            if package == binding.package
                && path == binding.path
                && limit == MAX_FROZEN_EXECUTABLE_SYMLINKS
    ));
}

#[test]
fn frozen_script_interpreters_are_closure_owned_confined_and_race_checked() {
    let temporary = tempfile::tempdir().unwrap();
    let installation_root = temporary.path().join("installation");
    let frozen_root = temporary.path().join("frozen-root");
    fs::create_dir(&installation_root).unwrap();
    fs::create_dir(&frozen_root).unwrap();

    let installation = frozen_test_installation(&installation_root);
    let client = Client::frozen(
        "frozen-shebang-test",
        installation,
        repository::Map::default(),
        &frozen_root,
    )
    .unwrap();
    let script_package = package::Id::from("script-package");
    let interpreter_package = package::Id::from("interpreter-package");
    let loader_package = package::Id::from("loader-package");
    let packages = [
        script_package.clone(),
        interpreter_package.clone(),
        loader_package.clone(),
    ];
    let script_bytes = b"#!/bin/interpreter\nexit 0\n";
    let native_bytes = test_elf(Some("/lib64/ld-frozen.so"), 2);
    let loader_bytes = test_elf(None, 1);
    let script_digest = xxhash_rust::xxh3::xxh3_128(script_bytes);
    let native_digest = xxhash_rust::xxh3::xxh3_128(&native_bytes);
    let loader_digest = xxhash_rust::xxh3::xxh3_128(&loader_bytes);
    let directory = StonePayloadLayoutRecord {
        uid: 0,
        gid: 0,
        mode: nix::libc::S_IFDIR | 0o755,
        tag: 0,
        file: StonePayloadLayoutFile::Directory("bin".into()),
    };
    let lib_directory = StonePayloadLayoutRecord {
        file: StonePayloadLayoutFile::Directory("lib".into()),
        ..directory.clone()
    };
    let script_layout = StonePayloadLayoutRecord {
        uid: 0,
        gid: 0,
        mode: nix::libc::S_IFREG | 0o755,
        tag: 0,
        file: StonePayloadLayoutFile::Regular(script_digest, "bin/script".into()),
    };
    let native_layout = StonePayloadLayoutRecord {
        uid: 0,
        gid: 0,
        mode: nix::libc::S_IFREG | 0o755,
        tag: 0,
        file: StonePayloadLayoutFile::Regular(native_digest, "bin/interpreter".into()),
    };
    let loader_layout = StonePayloadLayoutRecord {
        file: StonePayloadLayoutFile::Regular(loader_digest, "lib/ld-frozen.so".into()),
        ..native_layout.clone()
    };
    client
        .layout_db
        .batch_add([
            (&script_package, &directory),
            // Frozen materialization collapses byte-identical duplicate
            // directory rows; executable verification must do the same.
            (&script_package, &directory),
            (&script_package, &script_layout),
            (&interpreter_package, &directory),
            (&interpreter_package, &native_layout),
            (&loader_package, &lib_directory),
            (&loader_package, &loader_layout),
        ])
        .unwrap();
    for (digest, bytes) in [
        (script_digest, script_bytes.as_slice()),
        (native_digest, native_bytes.as_slice()),
        (loader_digest, loader_bytes.as_slice()),
    ] {
        let path = cache::asset_path(&client.installation, &format!("{digest:02x}"));
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, bytes).unwrap();
    }
    client.discard_frozen_root().unwrap();
    let _materialized = client.blit_frozen_root(&packages, 1_700_000_123).unwrap();

    let binding = FrozenExecutableBinding {
        package: script_package.clone(),
        path: PathBuf::from("/usr/bin/script"),
    };
    let _guard = client
        .require_frozen_executables(&packages, std::slice::from_ref(&binding))
        .unwrap();

    let bin_alias = frozen_root.join("bin");
    fs::remove_file(&bin_alias).unwrap();
    symlink("usr/sbin", &bin_alias).unwrap();
    assert!(matches!(
        client.require_frozen_executables(&packages, std::slice::from_ref(&binding)),
        Err(Error::FrozenInterpreterRootAliasTarget { path, expected, actual })
            if path == Path::new("/bin")
                && expected == "usr/bin"
                && actual == "usr/sbin"
    ));
    fs::remove_file(&bin_alias).unwrap();
    symlink("usr/bin", &bin_alias).unwrap();

    let lib64_alias = frozen_root.join("lib64");
    fs::remove_file(&lib64_alias).unwrap();
    symlink("usr/lib32", &lib64_alias).unwrap();
    assert!(matches!(
        client.require_frozen_executables(&packages, std::slice::from_ref(&binding)),
        Err(Error::FrozenInterpreterRootAliasTarget { path, expected, actual })
            if path == Path::new("/lib64")
                && expected == "usr/lib"
                && actual == "usr/lib32"
    ));
    fs::remove_file(&lib64_alias).unwrap();
    symlink("usr/lib", &lib64_alias).unwrap();

    let interpreted_loader_bytes = b"#!/usr/bin/interpreter\n";
    let interpreted_loader_digest = xxhash_rust::xxh3::xxh3_128(interpreted_loader_bytes);
    let interpreted_loader_layout = StonePayloadLayoutRecord {
        file: StonePayloadLayoutFile::Regular(interpreted_loader_digest, "lib/ld-frozen.so".into()),
        ..loader_layout.clone()
    };
    client
        .layout_db
        .batch_add([
            (&loader_package, &lib_directory),
            (&loader_package, &interpreted_loader_layout),
        ])
        .unwrap();
    let interpreted_loader_asset = cache::asset_path(&client.installation, &format!("{interpreted_loader_digest:02x}"));
    fs::create_dir_all(interpreted_loader_asset.parent().unwrap()).unwrap();
    fs::write(interpreted_loader_asset, interpreted_loader_bytes).unwrap();
    client.discard_frozen_root().unwrap();
    let _materialized = client.blit_frozen_root(&packages, 1_700_000_123).unwrap();
    assert!(matches!(
        client.require_frozen_executables(&packages, std::slice::from_ref(&binding)),
        Err(Error::FrozenElfInterpreterIsInterpreted { package, path })
            if package == loader_package && path == Path::new("/usr/lib/ld-frozen.so")
    ));
    client
        .layout_db
        .batch_add([(&loader_package, &lib_directory), (&loader_package, &loader_layout)])
        .unwrap();
    client.discard_frozen_root().unwrap();
    let _materialized = client.blit_frozen_root(&packages, 1_700_000_123).unwrap();

    // A file which happens to exist in the root is not an interpreter
    // provider when its package is absent from the exact frozen closure.
    assert!(matches!(
        client.require_frozen_executables(std::slice::from_ref(&script_package), std::slice::from_ref(&binding)),
        Err(Error::MissingFrozenInterpreterProvider { path })
            if path == Path::new("/usr/bin/interpreter")
    ));

    let interpreter_path = frozen_root.join("usr/bin/interpreter");
    fs::remove_file(&interpreter_path).unwrap();
    symlink("interpreter-escape", &interpreter_path).unwrap();
    fs::write(frozen_root.join("usr/bin/interpreter-escape"), &native_bytes).unwrap();
    assert!(matches!(
        client.require_frozen_executables(&packages, std::slice::from_ref(&binding)),
        Err(Error::OpenFrozenExecutable { package, path, .. })
            if package == interpreter_package && path == Path::new("/usr/bin/interpreter")
    ));
    fs::remove_file(frozen_root.join("usr/bin/interpreter-escape")).unwrap();
    client.discard_frozen_root().unwrap();
    let _materialized = client.blit_frozen_root(&packages, 1_700_000_123).unwrap();

    let moved_root = temporary.path().join("moved-frozen-root");
    let mut root_raced = false;
    let error = require_frozen_executables(
        &client,
        test_materialized_frozen_root(&frozen_root).unwrap(),
        &packages,
        std::slice::from_ref(&binding),
        |checked, checkpoint| {
            if checked == &binding && checkpoint == FrozenExecutableCheckpoint::AfterOpen && !root_raced {
                fs::rename(&frozen_root, &moved_root).unwrap();
                fs::create_dir(&frozen_root).unwrap();
                root_raced = true;
            }
        },
    )
    .unwrap_err();
    assert!(root_raced);
    assert!(matches!(
        error,
        Error::FrozenExecutableRootReplaced(path) if path == frozen_root
    ));
    fs::remove_dir(&frozen_root).unwrap();
    fs::rename(&moved_root, &frozen_root).unwrap();

    let script_path = frozen_root.join("usr/bin/script");
    let mut raced = false;
    let error = require_frozen_executables(
        &client,
        test_materialized_frozen_root(&frozen_root).unwrap(),
        &packages,
        std::slice::from_ref(&binding),
        |checked, checkpoint| {
            if checked.package == interpreter_package
                && checked.path == Path::new("/usr/bin/interpreter")
                && checkpoint == FrozenExecutableCheckpoint::AfterOpen
                && !raced
            {
                // The script itself was already accepted. Mutating it
                // while its interpreter is inspected must be caught by
                // the retained-graph revalidation before return.
                fs::write(&script_path, b"#!/bin/interpreter\nexit 1\n").unwrap();
                raced = true;
            }
        },
    )
    .unwrap_err();
    assert!(raced);
    assert!(matches!(
        error,
        Error::FrozenExecutablePathReplaced { package, path }
            if package == script_package && path == Path::new("/usr/bin/script")
    ));

    let cycle_bytes = b"#!/usr/bin/script\n";
    let cycle_digest = xxhash_rust::xxh3::xxh3_128(cycle_bytes);
    let cycle_layout = StonePayloadLayoutRecord {
        file: StonePayloadLayoutFile::Regular(cycle_digest, "bin/interpreter".into()),
        ..native_layout
    };
    client
        .layout_db
        .batch_add([
            (&interpreter_package, &directory),
            (&interpreter_package, &cycle_layout),
        ])
        .unwrap();
    let cycle_asset = cache::asset_path(&client.installation, &format!("{cycle_digest:02x}"));
    fs::create_dir_all(cycle_asset.parent().unwrap()).unwrap();
    fs::write(cycle_asset, cycle_bytes).unwrap();
    client.discard_frozen_root().unwrap();
    let _materialized = client.blit_frozen_root(&packages, 1_700_000_123).unwrap();
    assert!(matches!(
        client.require_frozen_executables(&packages, &[binding]),
        Err(Error::FrozenExecutableInterpreterCycle { package, path })
            if package == script_package && path == Path::new("/usr/bin/script")
    ));
}

#[test]
fn frozen_script_chain_accepts_n_and_rejects_n_plus_one_end_to_end() {
    let temporary = tempfile::tempdir().unwrap();
    let installation_root = temporary.path().join("installation");
    let frozen_root = temporary.path().join("frozen-root");
    fs::create_dir(&installation_root).unwrap();
    fs::create_dir_all(frozen_root.join("usr/bin")).unwrap();

    let installation = frozen_test_installation(&installation_root);
    let client = Client::frozen(
        "frozen-shebang-depth-test",
        installation,
        repository::Map::default(),
        &frozen_root,
    )
    .unwrap();
    let package = package::Id::from("script-chain-package");
    let binding = FrozenExecutableBinding {
        package: package.clone(),
        path: PathBuf::from("/usr/bin/chain-0"),
    };

    let install_chain = |interpreter_count: usize| {
        let mut layouts = vec![StonePayloadLayoutRecord {
            uid: 0,
            gid: 0,
            mode: nix::libc::S_IFDIR | 0o755,
            tag: 0,
            file: StonePayloadLayoutFile::Directory("bin".into()),
        }];
        for index in 0..=interpreter_count {
            let bytes = if index == interpreter_count {
                test_elf(None, 1)
            } else {
                format!("#!/usr/bin/chain-{}\n", index + 1).into_bytes()
            };
            let digest = xxhash_rust::xxh3::xxh3_128(&bytes);
            layouts.push(StonePayloadLayoutRecord {
                uid: 0,
                gid: 0,
                mode: nix::libc::S_IFREG | 0o755,
                tag: 0,
                file: StonePayloadLayoutFile::Regular(digest, format!("bin/chain-{index}").into()),
            });
            let path = frozen_root.join(format!("usr/bin/chain-{index}"));
            fs::write(&path, bytes).unwrap();
            fs::set_permissions(path, Permissions::from_mode(0o755)).unwrap();
        }
        client
            .layout_db
            .batch_add(layouts.iter().map(|layout| (&package, layout)))
            .unwrap();
    };

    install_chain(MAX_FROZEN_SHEBANG_INTERPRETERS);
    let _guard = client
        .require_frozen_executables(std::slice::from_ref(&package), std::slice::from_ref(&binding))
        .unwrap();

    install_chain(MAX_FROZEN_SHEBANG_INTERPRETERS + 1);
    assert!(matches!(
        client.require_frozen_executables(
            std::slice::from_ref(&package),
            std::slice::from_ref(&binding),
        ),
        Err(Error::FrozenShebangInterpreterLimit { package, path, limit })
            if package == binding.package
                && path == binding.path
                && limit == MAX_FROZEN_SHEBANG_INTERPRETERS
    ));
}
