const MULTIPLE_SOURCES_ARCHIVE_URL: &str =
    "https://fixtures.invalid/sources/cast-multiple-sources-fixture-1.0.0.tar.xz";
const MULTIPLE_SOURCES_ARCHIVE_SHA256: &str =
    "35c3c296ce08dd0ae1ebf27c782acf77f0f577f080b5ebe467c6c646f9ec7324";
const MULTIPLE_SOURCES_GIT_URL: &str =
    "https://fixtures.invalid/sources/cast-multiple-sources-protocol.git";
const MULTIPLE_SOURCES_GIT_COMMIT: &str = "4f124a6f438b061a836e332d67e803a69a7bf2d3";
const MULTIPLE_SOURCES_GIT_MATERIALIZATION_SHA256: &str =
    "4ee9bc28310196671f067634c0cbce03f21eca3a1a7b18be2fd2f808bc0c0e2c";
const MULTIPLE_SOURCES_GIT_BUNDLE: &str = "cast-multiple-sources-protocol-1.0.0.bundle";
const MULTIPLE_SOURCES_GIT_BUNDLE_SHA256: &str =
    "26531c6b1aa55b55c592f8f513f4e584c0c696a1888ef0f9948cfd265d57c5ed";
const MULTIPLE_SOURCES_RAW_URL: &str =
    "https://fixtures.invalid/sources/cast-multiple-sources-schema-1.0.0.h";
const MULTIPLE_SOURCES_RAW_SHA256: &str =
    "6c4caab665188429a1f0eda479c4aa11d1f61240f09ac0d201cf80d20fa5e330";
const MULTIPLE_SOURCES_RAW_ARTIFACT: &str = "cast-multiple-sources-schema-1.0.0.h";
const MULTIPLE_SOURCES_IDENTITY: &str =
    "cast multiple sources fixture: archive-main+git-protocol-v2+raw-schema-v3";
const MULTIPLE_SOURCES_RAW_COPY_SCRIPT: &str =
    r#"test ! -e src/protocol-schema.h && cp --preserve=mode,timestamps -- "${CAST_SOURCE_DIR}/protocol-schema.h" src/protocol-schema.h"#;

fn validate_multiple_sources_contract(package: &PackageSpec, lock: &SourceLock) -> Result<(), String> {
    let sources = package.sources.as_slice();
    lock.validate_against(sources)
        .map_err(|error| format!("multiple-sources lock does not match declaration: {error}"))?;
    if package.native_build_inputs != [DependencySpec::Binary("cp".to_owned())] {
        return Err("multiple-sources must declare exactly native binary(cp)".to_owned());
    }
    let [StepSpec::Shell {
        interpreter,
        declared_programs,
        script,
    }] = package.hooks.pre_setup.as_slice()
    else {
        return Err("multiple-sources must declare exactly one typed pre-setup shell copy".to_owned());
    };
    let [copy] = declared_programs.as_slice() else {
        return Err("multiple-sources pre-setup must declare exactly one copy program".to_owned());
    };
    if interpreter.path != "/usr/bin/bash"
        || interpreter.requirement != DependencySpec::Binary("bash".to_owned())
        || copy.path != "/usr/bin/cp"
        || copy.requirement != DependencySpec::Binary("cp".to_owned())
        || script != MULTIPLE_SOURCES_RAW_COPY_SCRIPT
    {
        return Err("multiple-sources raw-source copy program or path contract drifted".to_owned());
    }
    if [
        &package.hooks.post_setup,
        &package.hooks.pre_build,
        &package.hooks.post_build,
        &package.hooks.pre_check,
        &package.hooks.post_check,
        &package.hooks.pre_install,
        &package.hooks.post_install,
        &package.hooks.pre_workload,
        &package.hooks.post_workload,
    ]
    .into_iter()
    .any(|hooks| !hooks.is_empty())
    {
        return Err("multiple-sources must not hide additional build hooks".to_owned());
    }
    let [archive, git, raw] = sources else {
        return Err("multiple-sources must declare exactly three ordered sources".to_owned());
    };
    let UpstreamSpec::Archive {
        url,
        hash,
        rename,
        strip_dirs,
        unpack,
        unpack_dir,
    } = archive
    else {
        return Err("multiple-sources source 0 must remain archive-kind".to_owned());
    };
    if url != MULTIPLE_SOURCES_ARCHIVE_URL
        || hash != MULTIPLE_SOURCES_ARCHIVE_SHA256
        || rename.as_deref() != Some("application.tar.xz")
        || *strip_dirs != Some(1)
        || !*unpack
        || unpack_dir.as_deref() != Some("application")
    {
        return Err("multiple-sources archive rename/unpack/destination contract drifted".to_owned());
    }
    let UpstreamSpec::Git {
        url,
        git_ref,
        clone_dir,
    } = git
    else {
        return Err("multiple-sources source 1 must remain Git-kind".to_owned());
    };
    if url != MULTIPLE_SOURCES_GIT_URL
        || git_ref != MULTIPLE_SOURCES_GIT_COMMIT
        || clone_dir.as_deref() != Some("vendor-protocol")
    {
        return Err("multiple-sources exact Git ref/materialization directory contract drifted".to_owned());
    }
    let UpstreamSpec::Archive {
        url,
        hash,
        rename,
        strip_dirs,
        unpack,
        unpack_dir,
    } = raw
    else {
        return Err("multiple-sources source 2 must remain archive-kind raw data".to_owned());
    };
    if url != MULTIPLE_SOURCES_RAW_URL
        || hash != MULTIPLE_SOURCES_RAW_SHA256
        || rename.as_deref() != Some("protocol-schema.h")
        || strip_dirs.is_some()
        || *unpack
        || unpack_dir.is_some()
    {
        return Err("multiple-sources raw source must remain renamed and non-unpacked".to_owned());
    }

    let [SourceResolution::Archive(archive), SourceResolution::Git(git), SourceResolution::Archive(raw)] =
        lock.sources.as_slice()
    else {
        return Err("multiple-sources lock kinds or cardinality drifted".to_owned());
    };
    if archive.order != 0
        || archive.url != MULTIPLE_SOURCES_ARCHIVE_URL
        || archive.sha256 != MULTIPLE_SOURCES_ARCHIVE_SHA256
        || git.order != 1
        || git.url != MULTIPLE_SOURCES_GIT_URL
        || git.requested_ref != MULTIPLE_SOURCES_GIT_COMMIT
        || git.commit != MULTIPLE_SOURCES_GIT_COMMIT
        || git.materialization_sha256 != MULTIPLE_SOURCES_GIT_MATERIALIZATION_SHA256
        || raw.order != 2
        || raw.url != MULTIPLE_SOURCES_RAW_URL
        || raw.sha256 != MULTIPLE_SOURCES_RAW_SHA256
    {
        return Err("multiple-sources canonical lock identity or order drifted".to_owned());
    }
    Ok(())
}

fn multiple_sources_contract_fixture() -> (PackageSpec, SourceLock) {
    let package = execution_fixture_package_directory("multiple-sources");
    let recipe = crate::Recipe::load_authored(package.join("stone.glu")).unwrap();
    let lock = decode_source_lock(
        SOURCE_LOCK_FILE_NAME,
        &fs::read(package.join(SOURCE_LOCK_FILE_NAME)).unwrap(),
    )
    .unwrap();
    (recipe.declaration, lock)
}

fn assert_multiple_sources_package_rejected(label: &str, package: PackageSpec, lock: &SourceLock) {
    assert!(
        validate_multiple_sources_contract(&package, lock).is_err(),
        "multiple-sources package mutation must fail closed: {label}"
    );
}

fn assert_multiple_sources_lock_rejected(label: &str, package: &PackageSpec, lock: SourceLock) {
    assert!(
        validate_multiple_sources_contract(package, &lock).is_err(),
        "multiple-sources lock mutation must fail closed: {label}"
    );
}

#[test]
fn multiple_sources_declaration_and_lock_mutations_fail_closed() {
    let (package, lock) = multiple_sources_contract_fixture();
    validate_multiple_sources_contract(&package, &lock).unwrap();

    let mut candidate = package.clone();
    candidate.sources.pop();
    assert_multiple_sources_package_rejected("source cardinality", candidate, &lock);

    let mut candidate = package.clone();
    candidate.sources.swap(0, 1);
    assert_multiple_sources_package_rejected("source order", candidate, &lock);

    let mut candidate = package.clone();
    candidate.sources[1] = candidate.sources[2].clone();
    assert_multiple_sources_package_rejected("source kind", candidate, &lock);

    for (label, value) in [
        ("archive rename", Some("wrong.tar.xz".to_owned())),
        ("archive rename absent", None),
    ] {
        let mut candidate = package.clone();
        let UpstreamSpec::Archive { rename, .. } = &mut candidate.sources[0] else {
            unreachable!()
        };
        *rename = value;
        assert_multiple_sources_package_rejected(label, candidate, &lock);
    }
    let mut candidate = package.clone();
    let UpstreamSpec::Archive { strip_dirs, .. } = &mut candidate.sources[0] else {
        unreachable!()
    };
    *strip_dirs = Some(2);
    assert_multiple_sources_package_rejected("archive strip", candidate, &lock);

    let mut candidate = package.clone();
    let UpstreamSpec::Archive { unpack, .. } = &mut candidate.sources[0] else {
        unreachable!()
    };
    *unpack = false;
    assert_multiple_sources_package_rejected("archive unpack", candidate, &lock);

    let mut candidate = package.clone();
    let UpstreamSpec::Archive { unpack_dir, .. } = &mut candidate.sources[0] else {
        unreachable!()
    };
    *unpack_dir = Some("wrong".to_owned());
    assert_multiple_sources_package_rejected("archive destination", candidate, &lock);

    let mut candidate = package.clone();
    let UpstreamSpec::Git { git_ref, .. } = &mut candidate.sources[1] else {
        unreachable!()
    };
    *git_ref = "0123456789abcdef0123456789abcdef01234567".to_owned();
    assert_multiple_sources_package_rejected("Git ref", candidate, &lock);

    let mut candidate = package.clone();
    let UpstreamSpec::Git { clone_dir, .. } = &mut candidate.sources[1] else {
        unreachable!()
    };
    *clone_dir = Some("wrong-vendor".to_owned());
    assert_multiple_sources_package_rejected("Git directory", candidate, &lock);

    let mut candidate = package.clone();
    let UpstreamSpec::Archive { rename, .. } = &mut candidate.sources[2] else {
        unreachable!()
    };
    *rename = Some("wrong-schema.h".to_owned());
    assert_multiple_sources_package_rejected("raw rename", candidate, &lock);

    let mut candidate = package.clone();
    let UpstreamSpec::Archive { unpack, .. } = &mut candidate.sources[2] else {
        unreachable!()
    };
    *unpack = true;
    assert_multiple_sources_package_rejected("raw unpack", candidate, &lock);

    let mut candidate = package.clone();
    candidate.native_build_inputs.clear();
    assert_multiple_sources_package_rejected("missing native cp", candidate, &lock);

    let mut candidate = package.clone();
    candidate.native_build_inputs = vec![DependencySpec::Binary("install".to_owned())];
    assert_multiple_sources_package_rejected("wrong native cp", candidate, &lock);

    let mut candidate = package.clone();
    candidate.hooks.pre_setup.clear();
    assert_multiple_sources_package_rejected("missing raw copy hook", candidate, &lock);

    let mut candidate = package.clone();
    let StepSpec::Shell { interpreter, .. } = &mut candidate.hooks.pre_setup[0] else {
        unreachable!()
    };
    interpreter.path = "/bin/sh".to_owned();
    assert_multiple_sources_package_rejected("copy interpreter path", candidate, &lock);

    let mut candidate = package.clone();
    let StepSpec::Shell { declared_programs, .. } = &mut candidate.hooks.pre_setup[0] else {
        unreachable!()
    };
    declared_programs[0].path = "/usr/bin/install".to_owned();
    assert_multiple_sources_package_rejected("copy program path", candidate, &lock);

    for (label, script) in [
        ("copy source path", MULTIPLE_SOURCES_RAW_COPY_SCRIPT.replace("${CAST_SOURCE_DIR}", "${HOME}")),
        ("copy destination", MULTIPLE_SOURCES_RAW_COPY_SCRIPT.replace("src/protocol-schema.h", "schema.h")),
        ("copy metadata preservation", MULTIPLE_SOURCES_RAW_COPY_SCRIPT.replace("--preserve=mode,timestamps ", "")),
        ("copy absence check", MULTIPLE_SOURCES_RAW_COPY_SCRIPT.replace("test ! -e src/protocol-schema.h && ", "")),
    ] {
        let mut candidate = package.clone();
        let StepSpec::Shell { script: authored, .. } = &mut candidate.hooks.pre_setup[0] else {
            unreachable!()
        };
        *authored = script;
        assert_multiple_sources_package_rejected(label, candidate, &lock);
    }

    let mut candidate = lock.clone();
    candidate.sources.pop();
    assert_multiple_sources_lock_rejected("source cardinality", &package, candidate);

    let mut candidate = lock.clone();
    candidate.sources.swap(0, 1);
    assert_multiple_sources_lock_rejected("source order", &package, candidate);

    let mut candidate = lock.clone();
    candidate.sources[1] = candidate.sources[2].clone();
    assert_multiple_sources_lock_rejected("source kind", &package, candidate);

    let mut candidate = lock.clone();
    let SourceResolution::Git(git) = &mut candidate.sources[1] else {
        unreachable!()
    };
    git.requested_ref = "0123456789abcdef0123456789abcdef01234567".to_owned();
    assert_multiple_sources_lock_rejected("requested Git ref", &package, candidate);

    let mut candidate = lock.clone();
    let SourceResolution::Git(git) = &mut candidate.sources[1] else {
        unreachable!()
    };
    git.commit = "0123456789abcdef0123456789abcdef01234567".to_owned();
    assert_multiple_sources_lock_rejected("resolved Git commit", &package, candidate);

    let mut candidate = lock.clone();
    let SourceResolution::Git(git) = &mut candidate.sources[1] else {
        unreachable!()
    };
    git.materialization_sha256 = "0".repeat(64);
    assert_multiple_sources_lock_rejected("Git materialization identity", &package, candidate);
}

#[test]
fn multiple_sources_raw_and_git_fixture_tampering_never_becomes_consumable() {
    let (package, lock) = multiple_sources_contract_fixture();
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/gluon/execution");
    let temporary = crate::private_tempdir();

    let SourceResolution::Archive(raw) = &lock.sources[2] else {
        panic!("multiple-sources raw lock stopped being archive-kind")
    };
    let locked_raw = stone_recipe::derivation::LockedSource::Archive {
        order: raw.order,
        url: raw.url.clone(),
        sha256: raw.sha256.clone(),
        filename: package.sources[2].materialization_name().unwrap(),
    };
    let tampered_raw = temporary.path().join(MULTIPLE_SOURCES_RAW_ARTIFACT);
    fs::write(&tampered_raw, b"tampered raw schema\n").unwrap();
    let raw_cache = temporary.path().join("raw-cache");
    assert!(crate::upstream::import_locked_archive_fixture(&locked_raw, &raw_cache, &tampered_raw).is_err());
    assert!(!execution_source_cache_path(&raw_cache, &raw.url, &raw.sha256).exists());

    let SourceResolution::Git(git) = &lock.sources[1] else {
        panic!("multiple-sources Git lock stopped being Git-kind")
    };
    let locked_git = stone_recipe::derivation::LockedSource::Git {
        order: git.order,
        url: git.url.clone(),
        requested_ref: git.requested_ref.clone(),
        commit: git.commit.clone(),
        materialization_sha256: git.materialization_sha256.clone(),
        directory: package.sources[1].materialization_name().unwrap(),
    };
    let mut bundle = fs::read(root.join("git-bundles").join(MULTIPLE_SOURCES_GIT_BUNDLE)).unwrap();
    bundle[0] ^= 0xff;
    let tampered_bundle = temporary.path().join(MULTIPLE_SOURCES_GIT_BUNDLE);
    fs::write(&tampered_bundle, bundle).unwrap();
    let git_cache = temporary.path().join("git-cache");
    assert!(
        crate::upstream::import_locked_git_fixture(&locked_git, &git_cache, &tampered_bundle, SOURCE_DATE_EPOCH)
            .is_err()
    );
    let shared = temporary.path().join("shared-after-tamper");
    assert!(crate::upstream::sync_locked(&[locked_git], &git_cache, &shared, SOURCE_DATE_EPOCH).is_err());
    assert!(!shared.join("vendor-protocol").exists());
}

fn assert_multiple_sources_authored_trees(root: &Path) {
    let archive_tree = root.join("source-trees/cast-multiple-sources-fixture-1.0.0");
    let git_tree = root.join("git-source-trees/cast-multiple-sources-protocol-1.0.0");
    let raw = root.join("source-files/cast-multiple-sources-schema-1.0.0.h");
    let meson = fs::read_to_string(archive_tree.join("meson.build")).unwrap();
    let main = fs::read_to_string(archive_tree.join("src/main.c")).unwrap();
    let git_header = fs::read_to_string(git_tree.join("include/vendor_protocol.h")).unwrap();
    let raw_header = fs::read_to_string(raw).unwrap();
    for fragment in [
        "include_directories('../vendor-protocol/include')",
        "'cast-multiple-sources-fixture-runs'",
    ] {
        assert_eq!(meson.matches(fragment).count(), 1, "multiple-sources Meson fragment drifted");
    }
    for fragment in [
        "#include \"vendor_protocol.h\"",
        "#include \"protocol-schema.h\"",
        MULTIPLE_SOURCES_IDENTITY,
    ] {
        assert_eq!(main.matches(fragment).count(), 1, "multiple-sources C source fragment drifted");
    }
    assert_eq!(git_header.matches("CAST_VENDOR_PROTOCOL_ID \"git-protocol-v2\"").count(), 1);
    assert_eq!(raw_header.matches("CAST_RAW_SCHEMA_ID \"raw-schema-v3\"").count(), 1);
}

fn assert_multiple_sources_materializations(root: &Path, published: &Path, shared: &Path) {
    let archive_tree = root.join("source-trees/cast-multiple-sources-fixture-1.0.0");
    for relative in ["meson.build", "src/main.c"] {
        assert_eq!(
            fs::read(published.join(relative)).unwrap(),
            fs::read(archive_tree.join(relative)).unwrap(),
            "multiple-sources archive contains stale {relative} bytes"
        );
    }
    assert_eq!(
        fs::read(shared.join("vendor-protocol/include/vendor_protocol.h")).unwrap(),
        fs::read(root.join("git-source-trees/cast-multiple-sources-protocol-1.0.0/include/vendor_protocol.h"))
            .unwrap()
    );
    assert_eq!(
        fs::read(shared.join("protocol-schema.h")).unwrap(),
        fs::read(root.join("source-files/cast-multiple-sources-schema-1.0.0.h")).unwrap()
    );
    let names = |directory: &Path| {
        fs::read_dir(directory)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().into_string().unwrap())
            .collect::<BTreeSet<_>>()
    };
    assert_eq!(
        names(shared),
        BTreeSet::from([
            "application.tar.xz".to_owned(),
            "protocol-schema.h".to_owned(),
            "vendor-protocol".to_owned(),
        ]),
        "multiple-sources shared source root contains undeclared materializations"
    );
    assert_eq!(names(published), BTreeSet::from(["meson.build".to_owned(), "src".to_owned()]));
    assert_eq!(names(&published.join("src")), BTreeSet::from(["main.c".to_owned()]));
    let git_root = shared.join("vendor-protocol");
    assert_eq!(names(&git_root), BTreeSet::from(["include".to_owned()]));
    assert_eq!(
        names(&git_root.join("include")),
        BTreeSet::from(["vendor_protocol.h".to_owned()])
    );
    assert!(!git_root.join(".git").exists());
    for (path, mode) in [
        (published.to_owned(), 0o755),
        (published.join("meson.build"), 0o644),
        (published.join("src"), 0o755),
        (published.join("src/main.c"), 0o644),
        (shared.join("application.tar.xz"), 0o644),
        (git_root.clone(), 0o755),
        (git_root.join("include"), 0o755),
        (git_root.join("include/vendor_protocol.h"), 0o644),
        (shared.join("protocol-schema.h"), 0o644),
    ] {
        let metadata = fs::metadata(&path).unwrap();
        assert_eq!(metadata.mtime(), SOURCE_DATE_EPOCH, "materialization mtime drift at {path:?}");
        assert_eq!(metadata.mtime_nsec(), 0, "materialization subsecond mtime drift at {path:?}");
        assert_eq!(metadata.mode() & 0o7777, mode, "materialization mode drift at {path:?}");
    }
}
