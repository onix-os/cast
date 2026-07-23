const DESKTOP_INTEGRATION_ARCHIVE_URL: &str =
    "https://fixtures.invalid/sources/cast-desktop-integration-fixture-1.0.0.tar";
const DESKTOP_INTEGRATION_ARCHIVE_SHA256: &str =
    "0f39867b15a8ae8f5386fdc768fd83e2874ac41f6e5c8c8711b5ce9a67887169";
const DESKTOP_INTEGRATION_INSTALL_SCRIPT: &str = r#"install -Dm755 integration/cast-desktop-integration-fixture "${CAST_INSTALL_ROOT}/usr/libexec/cast-desktop-integration-fixture"
install -Dm644 integration/io.cast.desktop-integration-fixture.desktop "${CAST_INSTALL_ROOT}${CAST_DATADIR}/applications/io.cast.desktop-integration-fixture.desktop"
install -Dm644 integration/io.cast.desktop-integration-fixture.metainfo.xml "${CAST_INSTALL_ROOT}${CAST_DATADIR}/metainfo/io.cast.desktop-integration-fixture.metainfo.xml"
install -Dm644 integration/io.cast.desktop-integration-fixture.gschema.xml "${CAST_INSTALL_ROOT}${CAST_DATADIR}/glib-2.0/schemas/io.cast.desktop-integration-fixture.gschema.xml"
install -Dm644 integration/application-x-cast-desktop-integration-fixture.xml "${CAST_INSTALL_ROOT}${CAST_DATADIR}/mime/packages/application-x-cast-desktop-integration-fixture.xml"
install -Dm644 integration/io.cast.desktop-integration-fixture.svg "${CAST_INSTALL_ROOT}${CAST_DATADIR}/icons/hicolor/scalable/apps/io.cast.desktop-integration-fixture.svg"
install -Dm644 COPYING "${CAST_INSTALL_ROOT}${CAST_DATADIR}/licenses/cast-desktop-integration-fixture/COPYING""#;
const DESKTOP_INTEGRATION_CHECK_SCRIPT: &str = r#""${CAST_INSTALL_ROOT}/usr/libexec/cast-desktop-integration-fixture" --self-test
desktop-file-validate "${CAST_INSTALL_ROOT}/usr/share/applications/io.cast.desktop-integration-fixture.desktop"
glib-compile-schemas --strict --dry-run "${CAST_INSTALL_ROOT}/usr/share/glib-2.0/schemas"
appstreamcli validate --no-net --strict --pedantic "${CAST_INSTALL_ROOT}/usr/share/metainfo/io.cast.desktop-integration-fixture.metainfo.xml"
xmllint --nonet --noout "${CAST_INSTALL_ROOT}/usr/share/mime/packages/application-x-cast-desktop-integration-fixture.xml" "${CAST_INSTALL_ROOT}/usr/share/icons/hicolor/scalable/apps/io.cast.desktop-integration-fixture.svg"
install -Dm644 "${CAST_INSTALL_ROOT}/usr/share/mime/packages/application-x-cast-desktop-integration-fixture.xml" build/mime-validation/packages/application-x-cast-desktop-integration-fixture.xml
XDG_DATA_HOME="$PWD/build" XDG_DATA_DIRS="$PWD/build" update-mime-database build/mime-validation"#;

const DESKTOP_INTEGRATION_ASSETS: [(&str, &str, u32); 7] = [
    (
        "COPYING",
        "8abb14a2ec733cd92b7037fea6470cb891909634abf523ce4c5e4be9cbcdabb8",
        0o644,
    ),
    (
        "integration/application-x-cast-desktop-integration-fixture.xml",
        "ce4c8f53ada51f08328e6510c61a09c9c35e163f934a4be777b0d8e82ac8a789",
        0o644,
    ),
    (
        "integration/cast-desktop-integration-fixture",
        "6bb29de925bc8e24523478927b6e133bcc2ca0aac38c8345174d4da3089a9ca5",
        0o755,
    ),
    (
        "integration/io.cast.desktop-integration-fixture.desktop",
        "60c1bd67a2a822d4b52ed23ef71f9f0c6bb2b4de226cc0ce6d7ebbeaa9478c1e",
        0o644,
    ),
    (
        "integration/io.cast.desktop-integration-fixture.gschema.xml",
        "d752632200d2730abdc806dea1ea2c34878324bb7b668113102771ee3f8a48bf",
        0o644,
    ),
    (
        "integration/io.cast.desktop-integration-fixture.metainfo.xml",
        "b2ba101c78a308004445f048089958ff593f9e011ed04007d027182c9e6df2e3",
        0o644,
    ),
    (
        "integration/io.cast.desktop-integration-fixture.svg",
        "8f66003e5ab0c501c8850bb7401d7ffe54ea3442d13168c6b0435074a34eade5",
        0o644,
    ),
];

type DesktopIntegrationSnapshot = BTreeMap<String, (Vec<u8>, u32)>;

fn desktop_integration_path_shape(path: &stone_recipe::PathSpec) -> (&'static str, &str) {
    match path {
        stone_recipe::PathSpec::Any { path } => ("any", path),
        stone_recipe::PathSpec::Exe { path } => ("exe", path),
        stone_recipe::PathSpec::Symlink { path } => ("symlink", path),
        stone_recipe::PathSpec::Special { path } => ("special", path),
    }
}

fn validate_desktop_integration_recipe(package: &PackageSpec, lock: &SourceLock) -> Result<(), String> {
    lock.validate_against(&package.sources)
        .map_err(|error| format!("desktop-integration lock mismatch: {error}"))?;
    if dependency_names(&package.builder.required_tools) != ["binary(dash)", "binary(install)"] {
        return Err("builder tools must remain exactly dash and install".to_owned());
    }
    if !package.native_build_inputs.is_empty() || !package.build_inputs.is_empty() {
        return Err("desktop validators must remain check-only inputs".to_owned());
    }
    if dependency_names(&package.check_inputs)
        != [
            "binary(desktop-file-validate)",
            "binary(glib-compile-schemas)",
            "binary(appstreamcli)",
            "binary(update-mime-database)",
            "binary(xmllint)",
        ]
    {
        return Err("check capabilities drifted".to_owned());
    }
    if !package.builder.phases.setup.steps.is_empty()
        || !package.builder.phases.build.steps.is_empty()
        || !package.builder.phases.workload.steps.is_empty()
    {
        return Err("install-only fixture gained setup, build, or workload commands".to_owned());
    }
    let [StepSpec::Shell {
        interpreter,
        declared_programs,
        script,
    }] = package.builder.phases.install.steps.as_slice()
    else {
        return Err("install phase must remain one typed shell step".to_owned());
    };
    if interpreter.path != "/usr/bin/dash"
        || interpreter.requirement != DependencySpec::Binary("dash".to_owned())
        || !matches!(declared_programs.as_slice(), [program]
            if program.path == "/usr/bin/install"
                && program.requirement == DependencySpec::Binary("install".to_owned()))
        || script != DESKTOP_INTEGRATION_INSTALL_SCRIPT
    {
        return Err("install program or exact staged paths drifted".to_owned());
    }
    let [StepSpec::Shell {
        interpreter: check_interpreter,
        declared_programs: validators,
        script: check_script,
    }] = package.builder.phases.check.steps.as_slice()
    else {
        return Err("check phase must remain one typed staged-root validation shell".to_owned());
    };
    let expected_validators = [
        ("/usr/bin/desktop-file-validate", "desktop-file-validate"),
        ("/usr/bin/glib-compile-schemas", "glib-compile-schemas"),
        ("/usr/bin/appstreamcli", "appstreamcli"),
        ("/usr/bin/update-mime-database", "update-mime-database"),
        ("/usr/bin/xmllint", "xmllint"),
        ("/usr/bin/install", "install"),
    ];
    if check_interpreter.path != "/usr/bin/dash"
        || check_interpreter.requirement != DependencySpec::Binary("dash".to_owned())
        || validators.len() != expected_validators.len()
        || validators.iter().zip(expected_validators).any(|(program, (path, requirement))| {
            program.path != path || program.requirement != DependencySpec::Binary(requirement.to_owned())
        })
        || check_script != DESKTOP_INTEGRATION_CHECK_SCRIPT
    {
        return Err("staged-root validator programs or exact arguments drifted".to_owned());
    }
    if package.hooks != stone_recipe::package::HooksSpec::default() {
        return Err("fixture must not hide desktop validation in hooks".to_owned());
    }
    let [output] = package.outputs.as_slice() else {
        return Err("fixture must emit exactly one explicit output".to_owned());
    };
    if output.name != "out"
        || !output.include_in_manifest
        || output.summary.as_deref() != Some("Declarative desktop integration assets fixture")
        || output.description.as_deref()
            != Some(
                "A staged helper with validated desktop entry, AppStream, GSettings, MIME, and scalable icon declarations.",
            )
    {
        return Err("output metadata drifted".to_owned());
    }
    if dependency_names(&output.runtime_inputs) != ["binary(dash)", "glib2", "shared-mime-info", "hicolor-icon-theme"] {
        return Err("runtime consumers no longer match the emitted desktop assets".to_owned());
    }
    let paths = output.paths.iter().map(desktop_integration_path_shape).collect::<Vec<_>>();
    if paths
        != [
            ("exe", "/usr/libexec/cast-desktop-integration-fixture"),
            ("any", "/usr/share/applications/io.cast.desktop-integration-fixture.desktop"),
            ("any", "/usr/share/metainfo/io.cast.desktop-integration-fixture.metainfo.xml"),
            (
                "any",
                "/usr/share/glib-2.0/schemas/io.cast.desktop-integration-fixture.gschema.xml",
            ),
            (
                "any",
                "/usr/share/mime/packages/application-x-cast-desktop-integration-fixture.xml",
            ),
            (
                "any",
                "/usr/share/icons/hicolor/scalable/apps/io.cast.desktop-integration-fixture.svg",
            ),
            ("any", "/usr/share/licenses/cast-desktop-integration-fixture/COPYING"),
        ]
    {
        return Err("desktop output routing or executable typing drifted".to_owned());
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
    if url != DESKTOP_INTEGRATION_ARCHIVE_URL
        || hash != DESKTOP_INTEGRATION_ARCHIVE_SHA256
        || rename.as_deref() != Some("cast-desktop-integration-fixture.tar")
        || *strip_dirs != Some(1)
        || !*unpack
        || unpack_dir.as_deref() != Some("cast-desktop-integration-fixture")
    {
        return Err("archive identity or extraction policy drifted".to_owned());
    }
    let [SourceResolution::Archive(source)] = lock.sources.as_slice() else {
        return Err("source lock must retain one archive entry".to_owned());
    };
    if source.order != 0
        || source.url != DESKTOP_INTEGRATION_ARCHIVE_URL
        || source.sha256 != DESKTOP_INTEGRATION_ARCHIVE_SHA256
    {
        return Err("source lock identity drifted".to_owned());
    }
    Ok(())
}

fn read_desktop_integration_assets(source_tree: &Path) -> DesktopIntegrationSnapshot {
    let root_metadata = fs::symlink_metadata(source_tree).unwrap();
    assert!(root_metadata.file_type().is_dir(), "desktop-integration: source root is unsafe");
    assert_eq!(root_metadata.mode() & 0o7777, 0o755, "desktop-integration: source root mode drifted");
    let integration = source_tree.join("integration");
    let integration_metadata = fs::symlink_metadata(&integration).unwrap();
    assert!(
        integration_metadata.file_type().is_dir(),
        "desktop-integration: integration root is unsafe"
    );
    assert_eq!(
        integration_metadata.mode() & 0o7777,
        0o755,
        "desktop-integration: integration root mode drifted"
    );
    let root_names = fs::read_dir(source_tree)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect::<BTreeSet<_>>();
    assert_eq!(root_names, BTreeSet::from(["COPYING".to_owned(), "integration".to_owned()]));
    let integration_names = fs::read_dir(&integration)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect::<BTreeSet<_>>();
    assert_eq!(
        integration_names,
        DESKTOP_INTEGRATION_ASSETS[1..]
            .iter()
            .map(|(path, _, _)| Path::new(path).file_name().unwrap().to_string_lossy().into_owned())
            .collect()
    );
    DESKTOP_INTEGRATION_ASSETS
        .iter()
        .map(|(relative, _, _)| {
            let path = source_tree.join(relative);
            let metadata = fs::symlink_metadata(&path).unwrap();
            assert!(metadata.file_type().is_file(), "desktop-integration: unsafe authored {relative}");
            assert_eq!(metadata.nlink(), 1, "desktop-integration: multiply-linked authored {relative}");
            (relative.to_string(), (fs::read(path).unwrap(), metadata.mode() & 0o7777))
        })
        .collect()
}

fn validate_desktop_integration_assets(snapshot: &DesktopIntegrationSnapshot) -> Result<(), String> {
    if snapshot.len() != DESKTOP_INTEGRATION_ASSETS.len() {
        return Err("authored desktop asset cardinality drifted".to_owned());
    }
    for (relative, sha256, mode) in DESKTOP_INTEGRATION_ASSETS {
        let Some((bytes, actual_mode)) = snapshot.get(relative) else {
            return Err(format!("missing authored desktop asset {relative}"));
        };
        if *actual_mode != mode || hex::encode(Sha256::digest(bytes)) != sha256 {
            return Err(format!("authored desktop asset bytes or mode drifted: {relative}"));
        }
    }
    let text = |relative: &str| std::str::from_utf8(&snapshot[relative].0).unwrap();
    let helper = text("integration/cast-desktop-integration-fixture");
    let desktop = text("integration/io.cast.desktop-integration-fixture.desktop");
    let metainfo = text("integration/io.cast.desktop-integration-fixture.metainfo.xml");
    let schema = text("integration/io.cast.desktop-integration-fixture.gschema.xml");
    let mime = text("integration/application-x-cast-desktop-integration-fixture.xml");
    let icon = text("integration/io.cast.desktop-integration-fixture.svg");
    for (value, fragment) in [
        (helper, "#!/usr/bin/dash"),
        (helper, "cast-desktop-integration-fixture: self-test passed"),
        (desktop, "Exec=/usr/libexec/cast-desktop-integration-fixture %f"),
        (desktop, "Icon=io.cast.desktop-integration-fixture"),
        (desktop, "MimeType=application/x-cast-desktop-integration-fixture;"),
        (metainfo, "<id>io.cast.desktop-integration-fixture</id>"),
        (
            metainfo,
            "<launchable type=\"desktop-id\">io.cast.desktop-integration-fixture.desktop</launchable>",
        ),
        (metainfo, "<mediatype>application/x-cast-desktop-integration-fixture</mediatype>"),
        (schema, "id=\"io.cast.desktop-integration-fixture\""),
        (schema, "path=\"/io/cast/desktop-integration-fixture/\""),
        (mime, "type=\"application/x-cast-desktop-integration-fixture\""),
        (mime, "pattern=\"*.castdesk\" weight=\"80\""),
        (icon, "<title>Cast Desktop Integration Fixture</title>"),
    ] {
        if value.matches(fragment).count() != 1 {
            return Err(format!("desktop identity must contain exactly one {fragment:?}"));
        }
    }
    for generated in ["gschemas.compiled", "mime.cache", "mimeinfo.cache", "icon-theme.cache"] {
        if snapshot.keys().any(|path| path.ends_with(generated)) {
            return Err(format!("generated cache leaked into authored source: {generated}"));
        }
    }
    Ok(())
}

fn desktop_integration_contract_fixture() -> (PackageSpec, SourceLock, PathBuf) {
    let package_root = execution_fixture_package_directory("desktop-integration");
    let recipe = crate::Recipe::load_authored(package_root.join("stone.glu")).unwrap();
    let lock = evaluate_source_lock(
        SOURCE_LOCK_FILE_NAME,
        &fs::read(package_root.join(SOURCE_LOCK_FILE_NAME)).unwrap(),
    )
    .unwrap();
    let source_tree = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/gluon/execution/source-trees/cast-desktop-integration-fixture-1.0.0");
    (recipe.declaration, lock, source_tree)
}

fn assert_desktop_integration_fixture_contract(package: &PackageSpec, source_tree: &Path) {
    let lock_root = execution_fixture_package_directory("desktop-integration");
    let lock = evaluate_source_lock(
        SOURCE_LOCK_FILE_NAME,
        &fs::read(lock_root.join(SOURCE_LOCK_FILE_NAME)).unwrap(),
    )
    .unwrap();
    validate_desktop_integration_recipe(package, &lock).unwrap();
    validate_desktop_integration_assets(&read_desktop_integration_assets(source_tree)).unwrap();
}

fn assert_desktop_integration_archive_matches_tracked_sources(source_tree: &Path, published: &Path) {
    let authored = read_desktop_integration_assets(source_tree);
    let extracted = read_desktop_integration_assets(published);
    assert_eq!(extracted, authored, "desktop-integration: locked archive bytes or modes drifted");
}

#[test]
fn desktop_integration_declaration_and_assets_fail_closed() {
    let (package, lock, source_tree) = desktop_integration_contract_fixture();
    validate_desktop_integration_recipe(&package, &lock).unwrap();
    let assets = read_desktop_integration_assets(&source_tree);
    validate_desktop_integration_assets(&assets).unwrap();

    let reject_package = |label: &str, candidate: PackageSpec| {
        assert!(
            validate_desktop_integration_recipe(&candidate, &lock).is_err(),
            "desktop-integration mutation must fail closed: {label}"
        );
    };
    let mut candidate = package.clone();
    candidate.builder.required_tools.pop();
    reject_package("builder tool", candidate);
    let mut candidate = package.clone();
    candidate.check_inputs.swap(0, 1);
    reject_package("check input order", candidate);
    let mut candidate = package.clone();
    candidate.check_inputs.pop();
    reject_package("missing validator", candidate);
    let mut candidate = package.clone();
    candidate.outputs[0].runtime_inputs.pop();
    reject_package("missing runtime consumer", candidate);
    let mut candidate = package.clone();
    candidate.outputs[0].runtime_inputs.swap(1, 2);
    reject_package("runtime consumer order", candidate);
    let mut candidate = package.clone();
    candidate.outputs[0].paths.pop();
    reject_package("missing routed asset", candidate);
    let mut candidate = package.clone();
    let StepSpec::Shell { script, .. } = &mut candidate.builder.phases.install.steps[0] else {
        unreachable!()
    };
    script.push_str("\ntrue");
    reject_package("install script", candidate);
    let mut candidate = package.clone();
    let StepSpec::Shell { declared_programs, .. } = &mut candidate.builder.phases.check.steps[0] else {
        unreachable!()
    };
    declared_programs.swap(0, 1);
    reject_package("validator program order", candidate);
    let mut candidate = package.clone();
    let UpstreamSpec::Archive { hash, .. } = &mut candidate.sources[0] else {
        unreachable!()
    };
    *hash = "0".repeat(64);
    reject_package("archive hash", candidate);

    let mut candidate_lock = lock.clone();
    let SourceResolution::Archive(source) = &mut candidate_lock.sources[0] else {
        unreachable!()
    };
    source.sha256 = "0".repeat(64);
    assert!(validate_desktop_integration_recipe(&package, &candidate_lock).is_err());

    let mut tampered_assets = assets.clone();
    tampered_assets
        .get_mut("integration/io.cast.desktop-integration-fixture.desktop")
        .unwrap()
        .0
        .push(b' ');
    assert!(validate_desktop_integration_assets(&tampered_assets).is_err());
    let mut wrong_mode = assets;
    wrong_mode
        .get_mut("integration/cast-desktop-integration-fixture")
        .unwrap()
        .1 = 0o644;
    assert!(validate_desktop_integration_assets(&wrong_mode).is_err());
}

#[test]
fn desktop_integration_tampered_archive_never_becomes_consumable() {
    let (package, lock, _) = desktop_integration_contract_fixture();
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
    let tampered = temporary.path().join("cast-desktop-integration-fixture-1.0.0.tar");
    fs::write(&tampered, b"tampered desktop integration archive\n").unwrap();
    let cache = temporary.path().join("cache");
    assert!(crate::upstream::import_locked_archive_fixture(&locked, &cache, &tampered).is_err());
    assert!(!execution_source_cache_path(&cache, &source.url, &source.sha256).exists());
}
