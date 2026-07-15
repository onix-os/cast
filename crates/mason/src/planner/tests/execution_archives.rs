#[test]
fn offline_execution_fixture_archives_are_real_locked_and_complete() {
    let temporary = crate::private_tempdir();
    let cache = temporary.path().join("source-cache");
    let shared = temporary.path().join("shared");
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/gluon/execution");
    let packages = root.join("packages");
    let archives = root.join("archives");
    let source_trees = root.join("source-trees");

    let discovered = [&packages, &source_trees].map(|directory| {
        let mut names = fs::read_dir(directory)
            .unwrap()
            .map(|entry| entry.unwrap())
            .filter(|entry| entry.file_type().unwrap().is_dir())
            .map(|entry| entry.file_name().into_string().unwrap())
            .collect::<Vec<_>>();
        names.sort();
        names
    });
    assert_eq!(discovered[0], EXECUTION_FIXTURES);
    assert_eq!(
        discovered[1],
        [
            "cast-autotools-fixture-1.0.0",
            "cast-autotools-options-fixture-1.0.0",
            "cast-cargo-features-fixture-1.0.0",
            "cast-cargo-fixture-1.0.0",
            "cast-cargo-vendored-fixture-1.0.0",
            "cast-cmake-fixture-1.0.0",
            "cast-custom-fixture-1.0.0",
            "cast-daemon-fixture-1.0.0",
            "cast-factory-override-fixture-1.0.0",
            "cast-hooks-fixture-1.0.0",
            "cast-meson-fixture-1.0.0",
            "cast-split-fixture-1.0.0",
        ]
    );

    let mut admitted_archives = BTreeSet::new();
    let mut archive_format_counts = [0_usize; 4];
    for name in EXECUTION_FIXTURES {
        let recipe_path = packages.join(name).join("stone.glu");
        let recipe = crate::Recipe::load_authored(&recipe_path)
            .unwrap_or_else(|error| panic!("{name}: evaluate execution fixture: {error:#}"));
        if name == "factory-override" {
            let factory = recipe
                .fingerprint
                .imported_modules
                .iter()
                .find(|module| module.logical_name == "factory.glu")
                .expect("factory-override: local Gluon factory is absent from recipe provenance");
            assert_eq!(
                factory.sha256,
                hex::encode(Sha256::digest(
                    fs::read(packages.join(name).join("factory.glu")).unwrap()
                )),
                "factory-override: recipe provenance does not bind the exact imported factory"
            );
            assert_eq!(recipe.declaration.architectures, ["x86_64"]);
            let [StepSpec::CMakeConfigure { flags }] = recipe.declaration.builder.phases.setup.steps.as_slice() else {
                panic!("factory-override: package patch did not select the CMake builder");
            };
            assert_eq!(flags.as_slice(), ["-DCAST_FACTORY_VARIANT=stone-override"]);
        }
        if name == "autotools-options" {
            let [StepSpec::AutotoolsConfigure { flags }] =
                recipe.declaration.builder.phases.setup.steps.as_slice()
            else {
                panic!("autotools-options: expected one structural configure step");
            };
            assert_eq!(flags.as_slice(), ["--enable-stone-message"]);
            assert!(
                recipe.declaration.builder.phases.check.steps.is_empty(),
                "autotools-options: run_tests=false must remove the typed check step"
            );
        }
        if name == "cargo-features" {
            let [StepSpec::CargoBuild { features }] =
                recipe.declaration.builder.phases.build.steps.as_slice()
            else {
                panic!("cargo-features: expected one structural Cargo build step");
            };
            assert_eq!(features.as_slice(), ["fixture-protocol"]);
            let [StepSpec::CargoInstall { binaries }] =
                recipe.declaration.builder.phases.install.steps.as_slice()
            else {
                panic!("cargo-features: expected one structural Cargo install step");
            };
            assert_eq!(
                binaries.as_slice(),
                ["cast-feature-client", "cast-feature-daemon"]
            );
            let [StepSpec::CargoTest { features }] =
                recipe.declaration.builder.phases.check.steps.as_slice()
            else {
                panic!("cargo-features: expected one structural Cargo test step");
            };
            assert_eq!(features.as_slice(), ["fixture-protocol"]);
        }
        let lock_path = recipe_path.with_file_name(SOURCE_LOCK_FILE_NAME);
        let lock_bytes = fs::read(&lock_path).unwrap();
        let lock = decode_source_lock(SOURCE_LOCK_FILE_NAME, &lock_bytes)
            .unwrap_or_else(|error| panic!("{name}: decode source lock: {error:#}"));
        lock.validate_against(&recipe.declaration.sources)
            .unwrap_or_else(|error| panic!("{name}: validate source lock: {error:#}"));
        assert_eq!(
            lock_bytes,
            encode_source_lock(&lock).into_bytes(),
            "{name}: checked-in source lock is not canonical"
        );

        let [SourceResolution::Archive(source)] = lock.sources.as_slice() else {
            panic!("{name}: execution fixture must have exactly one archive source");
        };
        let url = Url::parse(&source.url).unwrap();
        assert_eq!(
            url.scheme(),
            "https",
            "{name}: production source policy must remain HTTPS"
        );
        assert_eq!(url.host_str(), Some("fixtures.invalid"));
        let filename = url.path_segments().unwrap().next_back().unwrap();
        let archive_path = archives.join(filename);
        let metadata = fs::symlink_metadata(&archive_path).unwrap();
        assert!(metadata.file_type().is_file(), "{name}: archive must be a regular file");
        let bytes = fs::read(&archive_path).unwrap();
        assert_eq!(metadata.len(), u64::try_from(bytes.len()).unwrap());
        assert!(
            (1..=1024 * 1024).contains(&metadata.len()),
            "{name}: encoded fixture archive must remain small and non-empty"
        );

        let mut decoder: Box<dyn Read + '_> = match name {
            "cargo-vendored" => {
                assert_eq!(filename, "cast-cargo-vendored-fixture-1.0.0.tar.gz");
                assert!(bytes.starts_with(&[0x1f, 0x8b, 0x08]), "{name}: missing gzip magic");
                archive_format_counts[1] += 1;
                Box::new(flate2::read::GzDecoder::new(bytes.as_slice()))
            }
            "hooks-patch" => {
                assert_eq!(filename, "cast-hooks-fixture-1.0.0.tar.xz");
                assert!(
                    bytes.starts_with(&[0xfd, b'7', b'z', b'X', b'Z', 0]),
                    "{name}: missing XZ magic"
                );
                archive_format_counts[2] += 1;
                Box::new(xz2::read::XzDecoder::new(bytes.as_slice()))
            }
            "daemon-generated" => {
                assert_eq!(filename, "cast-daemon-fixture-1.0.0.tar.zst");
                assert!(
                    bytes.starts_with(&[0x28, 0xb5, 0x2f, 0xfd]),
                    "{name}: missing Zstandard magic"
                );
                archive_format_counts[3] += 1;
                Box::new(zstd::stream::read::Decoder::new(bytes.as_slice()).unwrap())
            }
            _ => {
                assert!(filename.ends_with(".tar"), "{name}: expected a plain .tar fixture");
                archive_format_counts[0] += 1;
                Box::new(std::io::Cursor::new(bytes.as_slice()))
            }
        };
        let mut tar_bytes = Vec::new();
        decoder
            .by_ref()
            .take(1024 * 1024 + 1)
            .read_to_end(&mut tar_bytes)
            .unwrap_or_else(|error| panic!("{name}: decode execution fixture archive: {error}"));
        assert!(
            (10_240..=1024 * 1024).contains(&tar_bytes.len()) && tar_bytes.len() % 512 == 0,
            "{name}: decoded fixture must remain one small block-aligned tar stream"
        );
        assert_eq!(
            &tar_bytes[257..263],
            b"ustar\0",
            "{name}: decoded fixture is not a USTAR archive"
        );
        assert_eq!(hex::encode(Sha256::digest(&bytes)), source.sha256);
        assert!(
            admitted_archives.insert(filename.to_owned()),
            "duplicate execution archive"
        );

        let materialization_name = recipe.declaration.sources[0].materialization_name().unwrap();
        let locked = stone_recipe::derivation::LockedSource::Archive {
            order: 0,
            url: source.url.clone(),
            sha256: source.sha256.clone(),
            filename: materialization_name.clone(),
        };
        crate::upstream::import_locked_archive_fixture(&locked, &cache, &archive_path)
            .unwrap_or_else(|error| panic!("{name}: import locked fixture into source cache: {error:#}"));
        let share = shared.join(name);
        crate::upstream::sync_locked(std::slice::from_ref(&locked), &cache, &share, SOURCE_DATE_EPOCH)
            .unwrap_or_else(|error| panic!("{name}: share imported fixture through frozen source path: {error:#}"));
        let shared_archive = share.join(&materialization_name);
        assert_eq!(fs::read(&shared_archive).unwrap(), bytes);
        let shared_metadata = fs::metadata(&shared_archive).unwrap();
        let fixture_metadata = fs::metadata(&archive_path).unwrap();
        assert_ne!(
            (shared_metadata.dev(), shared_metadata.ino()),
            (fixture_metadata.dev(), fixture_metadata.ino()),
            "{name}: build-visible source must not alias the tracked fixture"
        );

        // Exercise the same structural two-pass extractor and atomic
        // publication path used by a real build. In particular, the three
        // compressed fixtures must not be accepted on filename or magic alone.
        let build = temporary.path().join("extracted").join(name);
        fs::create_dir_all(&build).unwrap();
        let mut archive_session = crate::archive::ArchiveSessionBudget::production();
        crate::archive::extract_locked_tar(
            &share,
            &materialization_name,
            &source.sha256,
            &build,
            "source",
            1,
            SOURCE_DATE_EPOCH,
            &mut archive_session,
        )
        .unwrap_or_else(|error| panic!("{name}: structurally extract and publish locked fixture: {error:#}"));
        let published = build.join("source");
        assert!(published.is_dir(), "{name}: extractor did not publish its destination");
        assert!(
            fs::read_dir(&published).unwrap().next().is_some(),
            "{name}: extractor published an empty source tree"
        );
    }

    let present_archives = fs::read_dir(archives)
        .unwrap()
        .map(|entry| entry.unwrap())
        .filter(|entry| entry.file_type().unwrap().is_file())
        .map(|entry| entry.file_name().into_string().unwrap())
        .collect::<BTreeSet<_>>();
    assert_eq!(
        present_archives, admitted_archives,
        "orphaned execution fixture archive"
    );
    assert_eq!(
        archive_format_counts,
        [9, 1, 1, 1],
        "execution fixtures must cover nine plain tar streams plus one each of gzip, XZ, and Zstandard"
    );
}
