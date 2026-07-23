const EXTERNAL_TEST_VECTORS_ARCHIVE_URL: &str =
    "https://fixtures.invalid/sources/cast-external-test-vectors-fixture-1.0.0.tar";
const EXTERNAL_TEST_VECTORS_ARCHIVE_SHA256: &str =
    "c04932c66a3399d95fda58b459b7f3e903454e5d2963f3529667216fdad50404";
const EXTERNAL_TEST_VECTORS_RAW_URL: &str =
    "https://fixtures.invalid/sources/cast-external-test-vectors-fixture-1.0.0-vectors.json";
const EXTERNAL_TEST_VECTORS_RAW_SHA256: &str =
    "c957dce5105c8add6e581f9aa0ee34002cbff8c9d403fab5132cebf0a8c7685c";
const EXTERNAL_TEST_VECTORS_RAW_ARTIFACT: &str =
    "cast-external-test-vectors-fixture-1.0.0-vectors.json";
const EXTERNAL_TEST_VECTORS_RAW_COPY_SCRIPT: &str = r#"test ! -e "${CAST_BUILDER_DIR}/external-test-vectors.json" && cp --preserve=mode,timestamps -- "${CAST_SOURCE_DIR}/external-test-vectors.json" "${CAST_BUILDER_DIR}/external-test-vectors.json" && test -f "${CAST_BUILDER_DIR}/external-test-vectors.json" && test ! -L "${CAST_BUILDER_DIR}/external-test-vectors.json" && test -s "${CAST_BUILDER_DIR}/external-test-vectors.json""#;
const EXTERNAL_TEST_VECTORS_MARKER: &str =
    "cast external test vectors fixture: 3 independently locked vectors verified";
const EXTERNAL_TEST_VECTORS_RAW_BYTES: &[u8] = b"{\"schema\":1,\"vectors\":[\n{\"plain\":0,\"encoded\":\"00\"},\n{\"plain\":42,\"encoded\":\"2a\"},\n{\"plain\":255,\"encoded\":\"ff\"}\n]}\n";

fn validate_external_test_vectors_contract(
    package: &PackageSpec,
    lock: &SourceLock,
    raw_bytes: &[u8],
) -> Result<(), String> {
    lock.validate_against(&package.sources)
        .map_err(|error| format!("external-test-vectors lock does not match declaration: {error}"))?;
    if raw_bytes != EXTERNAL_TEST_VECTORS_RAW_BYTES {
        return Err("external-test-vectors raw corpus bytes drifted".to_owned());
    }
    if package.meta.pname != "cast-external-test-vectors-fixture"
        || package.architectures != ["x86_64"]
        || package.options.networking
    {
        return Err("external-test-vectors package identity, target, or network policy drifted".to_owned());
    }
    if package.native_build_inputs != [DependencySpec::Binary("cp".to_owned())]
        || !package.build_inputs.is_empty()
        || !package.check_inputs.is_empty()
    {
        return Err("external-test-vectors must declare only native binary(cp)".to_owned());
    }
    if dependency_names(&package.builder.required_tools) != ["binary(sh)", "binary(ninja)"] {
        return Err("external-test-vectors CMake tool declaration drifted".to_owned());
    }
    if package.builder.phases.setup.steps
        != [StepSpec::CMakeConfigure {
            flags: vec![
                "-DBUILD_TESTING=ON".to_owned(),
                "-DCAST_EXTERNAL_VECTOR_FILE=external-test-vectors.json".to_owned(),
            ],
        }]
        || package.builder.phases.build.steps != [StepSpec::CMakeBuild]
        || package.builder.phases.install.steps != [StepSpec::CMakeInstall]
        || package.builder.phases.check.steps != [StepSpec::CMakeTest]
    {
        return Err("external-test-vectors must retain the exact CMake/CTest phases".to_owned());
    }

    let [StepSpec::Shell {
        interpreter,
        declared_programs,
        script,
    }] = package.hooks.pre_check.as_slice()
    else {
        return Err("external-test-vectors must have exactly one pre-check copy hook".to_owned());
    };
    let [copy] = declared_programs.as_slice() else {
        return Err("external-test-vectors pre-check hook must declare exactly one program".to_owned());
    };
    if interpreter.path != "/usr/bin/bash"
        || interpreter.requirement != DependencySpec::Binary("bash".to_owned())
        || copy.path != "/usr/bin/cp"
        || copy.requirement != DependencySpec::Binary("cp".to_owned())
        || script != EXTERNAL_TEST_VECTORS_RAW_COPY_SCRIPT
    {
        return Err("external-test-vectors pre-check capability or script drifted".to_owned());
    }
    if [
        &package.hooks.pre_setup,
        &package.hooks.post_setup,
        &package.hooks.pre_build,
        &package.hooks.post_build,
        &package.hooks.post_check,
        &package.hooks.pre_install,
        &package.hooks.post_install,
        &package.hooks.pre_workload,
        &package.hooks.post_workload,
    ]
    .into_iter()
    .any(|hooks| !hooks.is_empty())
    {
        return Err("external-test-vectors must not hide another hook".to_owned());
    }

    let [primary, vectors] = package.sources.as_slice() else {
        return Err("external-test-vectors must retain two ordered sources".to_owned());
    };
    if !matches!(
        primary,
        UpstreamSpec::Archive {
            url,
            hash,
            rename: Some(rename),
            strip_dirs: Some(1),
            unpack: true,
            unpack_dir: Some(directory),
        } if url == EXTERNAL_TEST_VECTORS_ARCHIVE_URL
            && hash == EXTERNAL_TEST_VECTORS_ARCHIVE_SHA256
            && rename == "cast-external-test-vectors-fixture.tar"
            && directory == "cast-external-test-vectors-fixture"
    ) || !matches!(
        vectors,
        UpstreamSpec::Archive {
            url,
            hash,
            rename: Some(rename),
            strip_dirs: None,
            unpack: false,
            unpack_dir: None,
        } if url == EXTERNAL_TEST_VECTORS_RAW_URL
            && hash == EXTERNAL_TEST_VECTORS_RAW_SHA256
            && rename == "external-test-vectors.json"
    ) {
        return Err("external-test-vectors archive/raw extraction contract drifted".to_owned());
    }
    let [SourceResolution::Archive(primary), SourceResolution::Archive(vectors)] = lock.sources.as_slice() else {
        return Err("external-test-vectors lock kinds or cardinality drifted".to_owned());
    };
    if primary.order != 0
        || primary.url != EXTERNAL_TEST_VECTORS_ARCHIVE_URL
        || primary.sha256 != EXTERNAL_TEST_VECTORS_ARCHIVE_SHA256
        || vectors.order != 1
        || vectors.url != EXTERNAL_TEST_VECTORS_RAW_URL
        || vectors.sha256 != EXTERNAL_TEST_VECTORS_RAW_SHA256
    {
        return Err("external-test-vectors canonical lock identity or order drifted".to_owned());
    }
    let [output, debugging] = package.outputs.as_slice() else {
        return Err("external-test-vectors must publish exactly out and dbginfo".to_owned());
    };
    if output.name != "out"
        || !output.include_in_manifest
        || output.paths
            != [stone_recipe::PathSpec::Exe {
                path: "/usr/bin/cast-external-test-vectors-fixture".to_owned(),
            }]
        || debugging.name != "dbginfo"
        || debugging.include_in_manifest
        || debugging.paths
            != [stone_recipe::PathSpec::Any {
                path: "/usr/lib/debug".to_owned(),
            }]
    {
        return Err("external-test-vectors output routing drifted".to_owned());
    }
    Ok(())
}

fn external_test_vectors_contract_fixture() -> (PackageSpec, SourceLock, Vec<u8>) {
    let package = execution_fixture_package_directory("external-test-vectors");
    let recipe = crate::Recipe::load_authored(package.join("stone.glu")).unwrap();
    let lock = decode_source_lock(
        SOURCE_LOCK_FILE_NAME,
        &fs::read(package.join(SOURCE_LOCK_FILE_NAME)).unwrap(),
    )
    .unwrap();
    let raw = fs::read(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/gluon/execution/source-files")
            .join(EXTERNAL_TEST_VECTORS_RAW_ARTIFACT),
    )
    .unwrap();
    (recipe.declaration, lock, raw)
}

fn assert_external_test_vectors_fixture_contract(
    package: &PackageSpec,
    source_tree: &Path,
    raw_path: &Path,
    lock: &SourceLock,
) {
    let raw = fs::read(raw_path).unwrap();
    validate_external_test_vectors_contract(package, lock, &raw).unwrap();
    let cmake = fs::read_to_string(source_tree.join("CMakeLists.txt")).unwrap();
    let source = fs::read_to_string(source_tree.join("frame_codec.c")).unwrap();
    for fragment in [
        "CAST_EXTERNAL_VECTOR_FILE must name the admitted external corpus",
        "if(BUILD_TESTING)",
        "${CMAKE_BINARY_DIR}/${CAST_EXTERNAL_VECTOR_FILE}",
        EXTERNAL_TEST_VECTORS_MARKER,
    ] {
        assert_eq!(cmake.matches(fragment).count(), 1, "external-test-vectors CMake fragment drifted");
    }
    assert_eq!(
        cmake
            .matches("cast-external-test-vectors-fixture-consumes-locked-corpus")
            .count(),
        2,
        "external-test-vectors CTest identity drifted"
    );
    for fragment in [
        "fopen(path, \"rb\")",
        "external vector corpus disagrees with the codec",
        "vectors != required_vector_count",
        EXTERNAL_TEST_VECTORS_MARKER,
    ] {
        assert_eq!(source.matches(fragment).count(), 1, "external-test-vectors C fragment drifted");
    }
}

fn assert_external_test_vectors_archive_matches_tracked_sources(source_tree: &Path, published: &Path) {
    for name in ["CMakeLists.txt", "frame_codec.c"] {
        assert_eq!(
            fs::read(published.join(name)).unwrap(),
            fs::read(source_tree.join(name)).unwrap(),
            "external-test-vectors: locked archive contains stale {name} bytes"
        );
    }
}

#[test]
fn external_test_vectors_declaration_and_corpus_fail_closed() {
    let (package, lock, raw) = external_test_vectors_contract_fixture();
    validate_external_test_vectors_contract(&package, &lock, &raw).unwrap();

    let mut candidate = package.clone();
    candidate.native_build_inputs.clear();
    assert!(validate_external_test_vectors_contract(&candidate, &lock, &raw).is_err());

    let mut candidate = package.clone();
    candidate.hooks.pre_check.clear();
    assert!(validate_external_test_vectors_contract(&candidate, &lock, &raw).is_err());

    let mut candidate = package.clone();
    let StepSpec::Shell { declared_programs, .. } = &mut candidate.hooks.pre_check[0] else {
        unreachable!()
    };
    declared_programs[0].path = "/usr/bin/install".to_owned();
    assert!(validate_external_test_vectors_contract(&candidate, &lock, &raw).is_err());

    let mut candidate = package.clone();
    let UpstreamSpec::Archive { unpack, .. } = &mut candidate.sources[1] else {
        unreachable!()
    };
    *unpack = true;
    assert!(validate_external_test_vectors_contract(&candidate, &lock, &raw).is_err());

    let mut candidate = package.clone();
    candidate.options.networking = true;
    assert!(validate_external_test_vectors_contract(&candidate, &lock, &raw).is_err());

    let mut candidate = package.clone();
    candidate.outputs[1].include_in_manifest = true;
    assert!(validate_external_test_vectors_contract(&candidate, &lock, &raw).is_err());

    let mut candidate = package.clone();
    candidate.outputs[1].paths[0] = stone_recipe::PathSpec::Any {
        path: "/usr/lib/debug/.build-id".to_owned(),
    };
    assert!(validate_external_test_vectors_contract(&candidate, &lock, &raw).is_err());

    let mut candidate = lock.clone();
    let SourceResolution::Archive(raw_lock) = &mut candidate.sources[1] else {
        unreachable!()
    };
    raw_lock.sha256 = "0".repeat(64);
    assert!(validate_external_test_vectors_contract(&package, &candidate, &raw).is_err());

    let mut tampered_raw = raw;
    tampered_raw[0] ^= 1;
    assert!(validate_external_test_vectors_contract(&package, &lock, &tampered_raw).is_err());
}

#[test]
fn external_test_vectors_tampered_sources_never_become_consumable() {
    let (package, lock, _) = external_test_vectors_contract_fixture();
    let temporary = crate::private_tempdir();
    for (index, bytes) in [b"tampered source archive\n".as_slice(), b"tampered vector corpus\n".as_slice()]
        .into_iter()
        .enumerate()
    {
        let SourceResolution::Archive(source) = &lock.sources[index] else {
            unreachable!()
        };
        let locked = stone_recipe::derivation::LockedSource::Archive {
            order: source.order,
            url: source.url.clone(),
            sha256: source.sha256.clone(),
            filename: package.sources[index].materialization_name().unwrap(),
        };
        let candidate = temporary.path().join(format!("tampered-{index}"));
        fs::write(&candidate, bytes).unwrap();
        let cache = temporary.path().join(format!("cache-{index}"));
        assert!(crate::upstream::import_locked_archive_fixture(&locked, &cache, &candidate).is_err());
        assert!(!execution_source_cache_path(&cache, &source.url, &source.sha256).exists());
    }
}
