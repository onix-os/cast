const GETTEXT_ARCHIVE_URL: &str =
    "https://fixtures.invalid/sources/cast-gettext-localization-fixture-1.0.0.tar";
const GETTEXT_ARCHIVE_SHA256: &str =
    "1e6b0b3267767853eb622e4155d3c50ecff677f8f3b305f5e4e1470f91fc1e5d";
const GETTEXT_INSTALL_SCRIPT: &str = r#"install -Dm644 build/locale/fr/LC_MESSAGES/cast-gettext-localization-fixture.mo "${CAST_INSTALL_ROOT}${CAST_DATADIR}/locale/fr/LC_MESSAGES/cast-gettext-localization-fixture.mo"
install -Dm644 build/locale/de/LC_MESSAGES/cast-gettext-localization-fixture.mo "${CAST_INSTALL_ROOT}${CAST_DATADIR}/locale/de/LC_MESSAGES/cast-gettext-localization-fixture.mo"
install -Dm644 COPYING "${CAST_INSTALL_ROOT}${CAST_DATADIR}/licenses/cast-gettext-localization-fixture/COPYING""#;

const GETTEXT_ASSETS: [(&str, &str); 4] = [
    (
        "COPYING",
        "8abb14a2ec733cd92b7037fea6470cb891909634abf523ce4c5e4be9cbcdabb8",
    ),
    (
        "consumer.c",
        "5c2ebfba2d1e3088fd89636be1060a522f22a4524c0994c49bf4c53dd64b388a",
    ),
    (
        "po/de.po",
        "59a83e5734bc83c03e92fc4d225e3ceaa340c9229366b8c5cc0e16bfd90094d2",
    ),
    (
        "po/fr.po",
        "023815def356c9ff5d1f50c386de88eaf03935d44f01660a05bc871c001c79bf",
    ),
];

type GettextSnapshot = BTreeMap<String, Vec<u8>>;

fn assert_run_step(step: &StepSpec, expected_program: &str, expected_args: &[&str]) -> Result<(), String> {
    let StepSpec::Run { program, args } = step else {
        return Err(format!("expected structural run step for {expected_program}"));
    };
    if program.path != format!("/usr/bin/{expected_program}")
        || program.requirement != DependencySpec::Binary(expected_program.to_owned())
        || args.iter().map(String::as_str).collect::<Vec<_>>() != expected_args
    {
        return Err(format!("{expected_program} structural step drifted"));
    }
    Ok(())
}

fn validate_gettext_recipe(package: &PackageSpec, lock: &SourceLock) -> Result<(), String> {
    lock.validate_against(&package.sources)
        .map_err(|error| format!("gettext-localization lock mismatch: {error}"))?;
    if dependency_names(&package.builder.required_tools)
        != [
            "binary(mkdir)",
            "binary(msgfmt)",
            "binary(cc)",
            "binary(bash)",
            "binary(install)",
        ]
    {
        return Err("builder tools must remain exactly mkdir, msgfmt, cc, Bash, and install".to_owned());
    }
    if !package.native_build_inputs.is_empty()
        || !package.build_inputs.is_empty()
        || !package.check_inputs.is_empty()
    {
        return Err("gettext packages must be selected only by typed binary requirements".to_owned());
    }
    let [fr_directory, de_directory] = package.builder.phases.setup.steps.as_slice() else {
        return Err("setup must create exactly two locale directories".to_owned());
    };
    assert_run_step(fr_directory, "mkdir", &["-p", "build/locale/fr/LC_MESSAGES"])?;
    assert_run_step(de_directory, "mkdir", &["-p", "build/locale/de/LC_MESSAGES"])?;

    let [fr_catalog, de_catalog, consumer] = package.builder.phases.build.steps.as_slice() else {
        return Err("build must compile exactly two catalogs and one native consumer".to_owned());
    };
    assert_run_step(
        fr_catalog,
        "msgfmt",
        &[
            "--check-format",
            "--check-header",
            "-o",
            "build/locale/fr/LC_MESSAGES/cast-gettext-localization-fixture.mo",
            "po/fr.po",
        ],
    )?;
    assert_run_step(
        de_catalog,
        "msgfmt",
        &[
            "--check-format",
            "--check-header",
            "-o",
            "build/locale/de/LC_MESSAGES/cast-gettext-localization-fixture.mo",
            "po/de.po",
        ],
    )?;
    assert_run_step(
        consumer,
        "cc",
        &[
            "-std=c11",
            "-O2",
            "-g",
            "-Wall",
            "-Wextra",
            "-Werror",
            "-fstack-protector-strong",
            "-D_FORTIFY_SOURCE=3",
            "-fPIE",
            "consumer.c",
            "-Wl,-pie",
            "-Wl,--build-id=sha1",
            "-Wl,-z,relro,-z,now",
            "-Wl,-z,noexecstack",
            "-Wl,-z,separate-code",
            "-Wl,--as-needed",
            "-o",
            "build/gettext-consumer",
        ],
    )?;

    let [fr_check, de_check] = package.builder.phases.check.steps.as_slice() else {
        return Err("check must execute exactly two locale-specific consumers".to_owned());
    };
    for (step, expected_args) in [
        (fr_check, ["fr_FR.utf8", "build/locale", "Bonjour de Cast"]),
        (de_check, ["de_DE.utf8", "build/locale", "Hallo von Cast"]),
    ] {
        let StepSpec::RunBuilt { program, args } = step else {
            return Err("locale checks must execute the build-only consumer".to_owned());
        };
        if program.path != "build/gettext-consumer"
            || args.iter().map(String::as_str).collect::<Vec<_>>() != expected_args
        {
            return Err("locale name, catalog root, or translated result drifted".to_owned());
        }
    }

    let [StepSpec::Shell {
        interpreter,
        declared_programs,
        script,
    }] = package.builder.phases.install.steps.as_slice()
    else {
        return Err("install must remain one typed Bash step".to_owned());
    };
    if interpreter.path != "/usr/bin/bash"
        || interpreter.requirement != DependencySpec::Binary("bash".to_owned())
        || !matches!(declared_programs.as_slice(), [program]
            if program.path == "/usr/bin/install"
                && program.requirement == DependencySpec::Binary("install".to_owned()))
        || script != GETTEXT_INSTALL_SCRIPT
    {
        return Err("install programs or exact catalog paths drifted".to_owned());
    }
    if !package.builder.phases.workload.steps.is_empty()
        || package.hooks != stone_recipe::package::HooksSpec::default()
    {
        return Err("fixture must not hide extra work in workload or hooks".to_owned());
    }

    let [output] = package.outputs.as_slice() else {
        return Err("fixture must emit exactly one explicit output".to_owned());
    };
    if output.name != "out"
        || !output.include_in_manifest
        || output.summary.as_deref() != Some("Compiled French and German gettext catalogs")
        || output.description.as_deref()
            != Some(
                "Two validated message catalogs whose translations are exercised by a build-only libc gettext consumer.",
            )
        || !output.runtime_inputs.is_empty()
    {
        return Err("catalog output metadata or build-only dependency boundary drifted".to_owned());
    }
    let paths = output
        .paths
        .iter()
        .map(|path| match path {
            stone_recipe::PathSpec::Any { path } => ("any", path.as_str()),
            stone_recipe::PathSpec::Exe { path } => ("exe", path.as_str()),
            stone_recipe::PathSpec::Symlink { path } => ("symlink", path.as_str()),
            stone_recipe::PathSpec::Special { path } => ("special", path.as_str()),
        })
        .collect::<Vec<_>>();
    if paths
        != [
            (
                "any",
                "/usr/share/locale/fr/LC_MESSAGES/cast-gettext-localization-fixture.mo",
            ),
            (
                "any",
                "/usr/share/locale/de/LC_MESSAGES/cast-gettext-localization-fixture.mo",
            ),
            (
                "any",
                "/usr/share/licenses/cast-gettext-localization-fixture/COPYING",
            ),
        ]
    {
        return Err("catalog or license output routing drifted".to_owned());
    }

    let [UpstreamSpec::Archive {
        url,
        hash,
        rename,
        strip_dirs,
        unpack,
        unpack_dir,
    }] = package.sources.as_slice()
    else {
        return Err("fixture must retain exactly one archive source".to_owned());
    };
    if url != GETTEXT_ARCHIVE_URL
        || hash != GETTEXT_ARCHIVE_SHA256
        || rename.as_deref() != Some("cast-gettext-localization-fixture.tar")
        || *strip_dirs != Some(1)
        || !*unpack
        || unpack_dir.as_deref() != Some("cast-gettext-localization-fixture")
    {
        return Err("archive identity or extraction policy drifted".to_owned());
    }
    let [SourceResolution::Archive(source)] = lock.sources.as_slice() else {
        return Err("source lock must retain one archive entry".to_owned());
    };
    if source.order != 0 || source.url != GETTEXT_ARCHIVE_URL || source.sha256 != GETTEXT_ARCHIVE_SHA256 {
        return Err("source lock identity drifted".to_owned());
    }
    Ok(())
}

fn read_gettext_assets(source_tree: &Path) -> GettextSnapshot {
    for directory in [source_tree, &source_tree.join("po")] {
        let metadata = fs::symlink_metadata(directory).unwrap();
        assert!(metadata.file_type().is_dir(), "gettext-localization: unsafe source directory");
    }
    let root_names = fs::read_dir(source_tree)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect::<BTreeSet<_>>();
    assert_eq!(
        root_names,
        BTreeSet::from(["COPYING".to_owned(), "consumer.c".to_owned(), "po".to_owned()])
    );
    let po_names = fs::read_dir(source_tree.join("po"))
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect::<BTreeSet<_>>();
    assert_eq!(po_names, BTreeSet::from(["de.po".to_owned(), "fr.po".to_owned()]));
    GETTEXT_ASSETS
        .iter()
        .map(|(relative, _)| {
            let path = source_tree.join(relative);
            let metadata = fs::symlink_metadata(&path).unwrap();
            assert!(metadata.file_type().is_file(), "gettext-localization: unsafe authored {relative}");
            assert_eq!(metadata.nlink(), 1, "gettext-localization: multiply-linked authored {relative}");
            assert_eq!(metadata.mode() & 0o7777, 0o644, "gettext-localization: authored mode drifted");
            (relative.to_string(), fs::read(path).unwrap())
        })
        .collect()
}

fn validate_gettext_assets(snapshot: &GettextSnapshot) -> Result<(), String> {
    if snapshot.len() != GETTEXT_ASSETS.len() {
        return Err("authored localization asset cardinality drifted".to_owned());
    }
    for (relative, sha256) in GETTEXT_ASSETS {
        let Some(bytes) = snapshot.get(relative) else {
            return Err(format!("missing authored localization asset {relative}"));
        };
        if hex::encode(Sha256::digest(bytes)) != sha256 {
            return Err(format!("authored localization bytes drifted: {relative}"));
        }
    }
    let text = |relative: &str| std::str::from_utf8(&snapshot[relative]).unwrap();
    let consumer = text("consumer.c");
    for fragment in [
        "#include <libintl.h>",
        "setlocale(LC_ALL, argv[1])",
        "bindtextdomain(DOMAIN, argv[2])",
        "translated = gettext(SOURCE_MESSAGE);",
        "untranslated gettext fallback rejected",
    ] {
        if consumer.matches(fragment).count() != 1 {
            return Err(format!("libc gettext consumer contract drifted: {fragment:?}"));
        }
    }
    for (relative, language, translated) in [
        ("po/fr.po", "Language: fr\\n", "msgstr \"Bonjour de Cast\""),
        ("po/de.po", "Language: de\\n", "msgstr \"Hallo von Cast\""),
    ] {
        let catalog = text(relative);
        if catalog.matches(language).count() != 1
            || catalog.matches("msgid \"Hello from Cast\"").count() != 1
            || catalog.matches(translated).count() != 1
        {
            return Err(format!("{relative}: translation identity drifted"));
        }
    }
    Ok(())
}

fn gettext_contract_fixture() -> (PackageSpec, SourceLock, PathBuf) {
    let package_root = execution_fixture_package_directory("gettext-localization");
    let recipe = crate::Recipe::load_authored(package_root.join("stone.glu")).unwrap();
    let lock = evaluate_source_lock(
        SOURCE_LOCK_FILE_NAME,
        &fs::read(package_root.join(SOURCE_LOCK_FILE_NAME)).unwrap(),
    )
    .unwrap();
    let source_tree = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/gluon/execution/source-trees/cast-gettext-localization-fixture-1.0.0");
    (recipe.declaration, lock, source_tree)
}

fn assert_gettext_fixture_contract(package: &PackageSpec, source_tree: &Path) {
    let package_root = execution_fixture_package_directory("gettext-localization");
    let lock = evaluate_source_lock(
        SOURCE_LOCK_FILE_NAME,
        &fs::read(package_root.join(SOURCE_LOCK_FILE_NAME)).unwrap(),
    )
    .unwrap();
    validate_gettext_recipe(package, &lock).unwrap();
    validate_gettext_assets(&read_gettext_assets(source_tree)).unwrap();
}

fn assert_gettext_archive_matches_tracked_sources(source_tree: &Path, published: &Path) {
    assert_eq!(
        read_gettext_assets(published),
        read_gettext_assets(source_tree),
        "gettext-localization: locked archive bytes drifted"
    );
}

#[test]
fn gettext_localization_declaration_and_catalogs_fail_closed() {
    let (package, lock, source_tree) = gettext_contract_fixture();
    validate_gettext_recipe(&package, &lock).unwrap();
    let assets = read_gettext_assets(&source_tree);
    validate_gettext_assets(&assets).unwrap();

    let reject = |label: &str, candidate: PackageSpec| {
        assert!(
            validate_gettext_recipe(&candidate, &lock).is_err(),
            "gettext-localization mutation must fail closed: {label}"
        );
    };
    let mut candidate = package.clone();
    candidate.builder.required_tools.pop();
    reject("builder tool", candidate);
    let mut candidate = package.clone();
    candidate.builder.phases.build.steps.swap(0, 1);
    reject("catalog order", candidate);
    let mut candidate = package.clone();
    candidate.builder.phases.check.steps.pop();
    reject("locale check", candidate);
    let mut candidate = package.clone();
    candidate.outputs[0].paths.pop();
    reject("output path", candidate);
    let mut candidate = package.clone();
    let StepSpec::Shell { script, .. } = &mut candidate.builder.phases.install.steps[0] else {
        unreachable!()
    };
    script.push_str("\ntrue");
    reject("install script", candidate);
    let mut candidate = package.clone();
    let UpstreamSpec::Archive { hash, .. } = &mut candidate.sources[0] else {
        unreachable!()
    };
    *hash = "0".repeat(64);
    reject("archive hash", candidate);

    let mut candidate_lock = lock.clone();
    let SourceResolution::Archive(source) = &mut candidate_lock.sources[0] else {
        unreachable!()
    };
    source.sha256 = "0".repeat(64);
    assert!(validate_gettext_recipe(&package, &candidate_lock).is_err());

    let mut tampered = assets;
    tampered.get_mut("po/fr.po").unwrap().push(b' ');
    assert!(validate_gettext_assets(&tampered).is_err());
}

#[test]
fn gettext_localization_tampered_archive_never_becomes_consumable() {
    let (package, lock, _) = gettext_contract_fixture();
    let SourceResolution::Archive(source) = &lock.sources[0] else {
        unreachable!()
    };
    let locked = stone_recipe::derivation::LockedSource::Archive {
        order: source.order,
        url: source.url.clone(),
        sha256: source.sha256.clone(),
        filename: package.sources[0].materialization_name().unwrap(),
    };
    let temporary = crate::private_tempdir();
    let tampered = temporary.path().join("cast-gettext-localization-fixture-1.0.0.tar");
    fs::write(&tampered, b"tampered gettext archive\n").unwrap();
    let cache = temporary.path().join("cache");
    assert!(crate::upstream::import_locked_archive_fixture(&locked, &cache, &tampered).is_err());
    assert!(!execution_source_cache_path(&cache, &source.url, &source.sha256).exists());
}
