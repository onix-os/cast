const SYSTEM_INTEGRATION_ARCHIVE_URL: &str =
    "https://fixtures.invalid/sources/cast-system-integration-assets-fixture-1.0.0.tar";
const SYSTEM_INTEGRATION_ARCHIVE_SHA256: &str =
    "27d04653529db216023599d2f6122f503acb59e0ebe26c5bce351dc970d58113";
const SYSTEM_INTEGRATION_INSTALL_SCRIPT: &str = r#"install -Dm755 integration/cast-system-integration-fixture "${CAST_INSTALL_ROOT}/usr/libexec/cast-system-integration-fixture"
install -Dm644 integration/cast-system-integration-fixture.service "${CAST_INSTALL_ROOT}${CAST_LIBDIR}/systemd/system/cast-system-integration-fixture.service"
install -Dm644 integration/cast-system-integration-fixture.sysusers "${CAST_INSTALL_ROOT}${CAST_LIBDIR}/sysusers.d/cast-system-integration-fixture.conf"
install -Dm644 integration/cast-system-integration-fixture.tmpfiles "${CAST_INSTALL_ROOT}${CAST_LIBDIR}/tmpfiles.d/cast-system-integration-fixture.conf"
install -Dm644 integration/70-cast-system-integration-fixture.rules "${CAST_INSTALL_ROOT}${CAST_LIBDIR}/udev/rules.d/70-cast-system-integration-fixture.rules"
install -Dm644 integration/io.cast.SystemIntegrationFixture.rules "${CAST_INSTALL_ROOT}${CAST_DATADIR}/polkit-1/rules.d/io.cast.SystemIntegrationFixture.rules"
install -Dm644 integration/io.cast.SystemIntegrationFixture.policy "${CAST_INSTALL_ROOT}${CAST_DATADIR}/polkit-1/actions/io.cast.SystemIntegrationFixture.policy"
install -Dm644 LICENSE "${CAST_INSTALL_ROOT}${CAST_DATADIR}/licenses/cast-system-integration-assets-fixture/LICENSE""#;
const SYSTEM_INTEGRATION_STAGED_HELPER_CHECK: &str =
    r#""${CAST_INSTALL_ROOT}/usr/libexec/cast-system-integration-fixture" --self-test"#;
const SYSTEM_INTEGRATION_VALIDATION_SCRIPT: &str = r#"SYSTEMD_UNIT_PATH=/usr/lib/systemd/system systemd-analyze --root="${CAST_INSTALL_ROOT}" --recursive-errors=no --man=no --generators=no verify cast-system-integration-fixture.service
systemd-sysusers --dry-run --root="${CAST_INSTALL_ROOT}" "${CAST_INSTALL_ROOT}/usr/lib/sysusers.d/cast-system-integration-fixture.conf"
systemd-tmpfiles --create --dry-run --root="${CAST_INSTALL_ROOT}" --graceful -E "${CAST_INSTALL_ROOT}/usr/lib/tmpfiles.d/cast-system-integration-fixture.conf"
udevadm verify --root="${CAST_INSTALL_ROOT}" --resolve-names=never --no-summary --no-style /usr/lib/udev/rules.d/70-cast-system-integration-fixture.rules
xmllint --nonet --noout "${CAST_INSTALL_ROOT}/usr/share/polkit-1/actions/io.cast.SystemIntegrationFixture.policy""#;

const SYSTEM_INTEGRATION_ASSETS: [(&str, &str, u32); 8] = [
    (
        "LICENSE",
        "4c2460f51cf7112f4500b769feceb974d1146f285f6dee5d3ce9778f912f3582",
        0o644,
    ),
    (
        "integration/70-cast-system-integration-fixture.rules",
        "fde7e6a751ea0793f518acdef914abcc01dfec040c96bd859ebb3a2f7347bc4e",
        0o644,
    ),
    (
        "integration/cast-system-integration-fixture",
        "4ff2f79cd3d97f87571122f0e46dd287a82923fd2987e9215b137f362627a8e1",
        0o755,
    ),
    (
        "integration/cast-system-integration-fixture.service",
        "69638f5ac2a2cb6a2071b643e992a61de960d1f5e3eb28742d23492a1b8db06a",
        0o644,
    ),
    (
        "integration/cast-system-integration-fixture.sysusers",
        "ba438963581c56f1d906f916845c2047e7bfb33822da740d319140f413721bf3",
        0o644,
    ),
    (
        "integration/cast-system-integration-fixture.tmpfiles",
        "715655fbb103d64b4deb021bec16628f0258ac471e2af564897f8c4004be2b48",
        0o644,
    ),
    (
        "integration/io.cast.SystemIntegrationFixture.policy",
        "c6a8472709a16a464c99a4b43f5412c28c60c39479827d0210e26306eb7f2890",
        0o644,
    ),
    (
        "integration/io.cast.SystemIntegrationFixture.rules",
        "53654bedc77c8f8eb21d15a2569c60d0361f2db315c4f3f8a83fc864c258dc93",
        0o644,
    ),
];

type SystemIntegrationSnapshot = BTreeMap<String, (Vec<u8>, u32)>;

fn system_integration_path_shape(path: &stone_recipe::PathSpec) -> (&'static str, &str) {
    match path {
        stone_recipe::PathSpec::Any { path } => ("any", path),
        stone_recipe::PathSpec::Exe { path } => ("exe", path),
        stone_recipe::PathSpec::Symlink { path } => ("symlink", path),
        stone_recipe::PathSpec::Special { path } => ("special", path),
    }
}

fn validate_system_integration_recipe(package: &PackageSpec, lock: &SourceLock) -> Result<(), String> {
    lock.validate_against(&package.sources)
        .map_err(|error| format!("system-integration-assets lock mismatch: {error}"))?;
    if dependency_names(&package.builder.required_tools) != ["binary(dash)", "binary(install)"] {
        return Err("builder tools must remain exactly dash and install".to_owned());
    }
    if !package.native_build_inputs.is_empty() || !package.build_inputs.is_empty() {
        return Err("validators must not be disguised as build inputs".to_owned());
    }
    if dependency_names(&package.check_inputs)
        != [
            "binary(systemd-analyze)",
            "binary(systemd-sysusers)",
            "binary(systemd-tmpfiles)",
            "binary(udevadm)",
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
        || script != SYSTEM_INTEGRATION_INSTALL_SCRIPT
    {
        return Err("install program or exact staged paths drifted".to_owned());
    }
    let [
        StepSpec::Shell {
            interpreter: helper_interpreter,
            declared_programs: helper_programs,
            script: helper_script,
        },
        StepSpec::Shell {
            interpreter: validator_interpreter,
            declared_programs: validators,
            script: validation_script,
        },
    ] = package.builder.phases.check.steps.as_slice()
    else {
        return Err("check phase must retain staged helper execution and one typed validator shell".to_owned());
    };
    if helper_interpreter.path != "/usr/bin/dash"
        || helper_interpreter.requirement != DependencySpec::Binary("dash".to_owned())
        || !helper_programs.is_empty()
        || helper_script != SYSTEM_INTEGRATION_STAGED_HELPER_CHECK
    {
        return Err("staged helper execution contract drifted".to_owned());
    }
    let expected_validators = [
        ("/usr/bin/systemd-analyze", "systemd-analyze"),
        ("/usr/bin/systemd-sysusers", "systemd-sysusers"),
        ("/usr/bin/systemd-tmpfiles", "systemd-tmpfiles"),
        ("/usr/bin/udevadm", "udevadm"),
        ("/usr/bin/xmllint", "xmllint"),
    ];
    if validator_interpreter.path != "/usr/bin/dash"
        || validator_interpreter.requirement != DependencySpec::Binary("dash".to_owned())
        || validators.len() != expected_validators.len()
        || validators.iter().zip(expected_validators).any(|(program, (path, requirement))| {
            program.path != path || program.requirement != DependencySpec::Binary(requirement.to_owned())
        })
        || validation_script != SYSTEM_INTEGRATION_VALIDATION_SCRIPT
    {
        return Err("staged-root validator programs or exact arguments drifted".to_owned());
    }
    if package.hooks != stone_recipe::package::HooksSpec::default() {
        return Err("fixture must not hide validation in hooks".to_owned());
    }
    let [output] = package.outputs.as_slice() else {
        return Err("fixture must emit exactly one explicit output".to_owned());
    };
    if output.name != "out"
        || !output.include_in_manifest
        || output.summary.as_deref() != Some("Declarative system integration assets fixture")
        || output.description.as_deref()
            != Some(
                "A staged helper plus systemd, sysusers, tmpfiles, udev, and self-contained polkit declarations with offline syntax checks.",
            )
    {
        return Err("output metadata drifted".to_owned());
    }
    if dependency_names(&output.runtime_inputs)
        != [
            "binary(dash)",
            "systemd",
            "systemd-sysusers",
            "systemd-tmpfiles",
            "systemd-udev",
            "polkit",
        ]
    {
        return Err("runtime consumers no longer match the emitted assets".to_owned());
    }
    let paths = output.paths.iter().map(system_integration_path_shape).collect::<Vec<_>>();
    if paths
        != [
            ("exe", "/usr/libexec/cast-system-integration-fixture"),
            ("any", "/usr/lib/systemd/system/cast-system-integration-fixture.service"),
            ("any", "/usr/lib/sysusers.d/cast-system-integration-fixture.conf"),
            ("any", "/usr/lib/tmpfiles.d/cast-system-integration-fixture.conf"),
            ("any", "/usr/lib/udev/rules.d/70-cast-system-integration-fixture.rules"),
            ("any", "/usr/share/polkit-1/rules.d/io.cast.SystemIntegrationFixture.rules"),
            ("any", "/usr/share/polkit-1/actions/io.cast.SystemIntegrationFixture.policy"),
            ("any", "/usr/share/licenses/cast-system-integration-assets-fixture/LICENSE"),
        ]
    {
        return Err("output routing or executable typing drifted".to_owned());
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
    if url != SYSTEM_INTEGRATION_ARCHIVE_URL
        || hash != SYSTEM_INTEGRATION_ARCHIVE_SHA256
        || rename.as_deref() != Some("cast-system-integration-assets-fixture.tar")
        || *strip_dirs != Some(1)
        || !*unpack
        || unpack_dir.as_deref() != Some("cast-system-integration-assets-fixture")
    {
        return Err("archive identity or extraction policy drifted".to_owned());
    }
    let [SourceResolution::Archive(source)] = lock.sources.as_slice() else {
        return Err("source lock must retain one archive entry".to_owned());
    };
    if source.order != 0 || source.url != SYSTEM_INTEGRATION_ARCHIVE_URL || source.sha256 != SYSTEM_INTEGRATION_ARCHIVE_SHA256 {
        return Err("source lock identity drifted".to_owned());
    }
    Ok(())
}

fn read_system_integration_assets(source_tree: &Path) -> SystemIntegrationSnapshot {
    let root_metadata = fs::symlink_metadata(source_tree).unwrap();
    assert!(root_metadata.file_type().is_dir(), "system-integration-assets: source root is unsafe");
    let integration_metadata = fs::symlink_metadata(source_tree.join("integration")).unwrap();
    assert!(
        integration_metadata.file_type().is_dir(),
        "system-integration-assets: integration root is unsafe"
    );
    let root_names = fs::read_dir(source_tree)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect::<BTreeSet<_>>();
    assert_eq!(root_names, BTreeSet::from(["LICENSE".to_owned(), "integration".to_owned()]));
    let integration_names = fs::read_dir(source_tree.join("integration"))
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect::<BTreeSet<_>>();
    assert_eq!(
        integration_names,
        SYSTEM_INTEGRATION_ASSETS[1..]
            .iter()
            .map(|(path, _, _)| Path::new(path).file_name().unwrap().to_string_lossy().into_owned())
            .collect()
    );
    SYSTEM_INTEGRATION_ASSETS
        .iter()
        .map(|(relative, _, _)| {
            let path = source_tree.join(relative);
            let metadata = fs::symlink_metadata(&path).unwrap();
            assert!(metadata.file_type().is_file(), "system-integration-assets: unsafe authored {relative}");
            assert_eq!(metadata.nlink(), 1, "system-integration-assets: multiply-linked authored {relative}");
            (relative.to_string(), (fs::read(path).unwrap(), metadata.mode() & 0o7777))
        })
        .collect()
}

fn validate_system_integration_assets(snapshot: &SystemIntegrationSnapshot) -> Result<(), String> {
    if snapshot.len() != SYSTEM_INTEGRATION_ASSETS.len() {
        return Err("authored asset cardinality drifted".to_owned());
    }
    for (relative, sha256, mode) in SYSTEM_INTEGRATION_ASSETS {
        let Some((bytes, actual_mode)) = snapshot.get(relative) else {
            return Err(format!("missing authored asset {relative}"));
        };
        if *actual_mode != mode || hex::encode(Sha256::digest(bytes)) != sha256 {
            return Err(format!("authored asset bytes or mode drifted: {relative}"));
        }
    }
    let text = |relative: &str| std::str::from_utf8(&snapshot[relative].0).unwrap();
    let helper = text("integration/cast-system-integration-fixture");
    let service = text("integration/cast-system-integration-fixture.service");
    let sysusers = text("integration/cast-system-integration-fixture.sysusers");
    let tmpfiles = text("integration/cast-system-integration-fixture.tmpfiles");
    let udev = text("integration/70-cast-system-integration-fixture.rules");
    let policy = text("integration/io.cast.SystemIntegrationFixture.policy");
    let rules = text("integration/io.cast.SystemIntegrationFixture.rules");
    for (value, fragment) in [
        (helper, "#!/usr/bin/dash"),
        (helper, "cast system integration assets fixture: helper executed"),
        (service, "ExecStart=/usr/libexec/cast-system-integration-fixture"),
        (service, "User=cast-system-integration"),
        (service, "Group=cast-system-integration"),
        (sysusers, "u cast-system-integration -"),
        (tmpfiles, "d /var/lib/cast-system-integration 0750 cast-system-integration cast-system-integration -"),
        (udev, "GROUP=\"cast-system-integration\""),
        (udev, "ENV{SYSTEMD_WANTS}+=\"cast-system-integration-fixture.service\""),
        (policy, "<action id=\"io.cast.SystemIntegrationFixture.manage\">"),
        (policy, "<allow_active>auth_admin_keep</allow_active>"),
        (rules, "action.id == \"io.cast.SystemIntegrationFixture.manage\""),
        (rules, "subject.isInGroup(\"cast-system-integration\")"),
        (rules, "return polkit.Result.AUTH_ADMIN;"),
    ] {
        if value.matches(fragment).count() != 1 {
            return Err(format!("cross-file identity must contain exactly one {fragment:?}"));
        }
    }
    if rules.contains("polkit.Result.YES") || rules.contains("=>") || rules.contains("const ") || rules.contains("let ") {
        return Err("polkit rule must remain conservative ES5-shaped exact bytes".to_owned());
    }
    Ok(())
}

fn system_integration_contract_fixture() -> (PackageSpec, SourceLock, PathBuf) {
    let package_root = execution_fixture_package_directory("system-integration-assets");
    let recipe = crate::Recipe::load_authored(package_root.join("stone.glu")).unwrap();
    let lock = evaluate_source_lock(
        SOURCE_LOCK_FILE_NAME,
        &fs::read(package_root.join(SOURCE_LOCK_FILE_NAME)).unwrap(),
    )
    .unwrap();
    let source_tree = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/gluon/execution/source-trees/cast-system-integration-assets-fixture-1.0.0");
    (recipe.declaration, lock, source_tree)
}

fn assert_system_integration_assets_fixture_contract(package: &PackageSpec, source_tree: &Path) {
    let lock_root = execution_fixture_package_directory("system-integration-assets");
    let lock = evaluate_source_lock(
        SOURCE_LOCK_FILE_NAME,
        &fs::read(lock_root.join(SOURCE_LOCK_FILE_NAME)).unwrap(),
    )
    .unwrap();
    validate_system_integration_recipe(package, &lock).unwrap();
    validate_system_integration_assets(&read_system_integration_assets(source_tree)).unwrap();
}

fn assert_system_integration_assets_archive_matches_tracked_sources(source_tree: &Path, published: &Path) {
    let authored = read_system_integration_assets(source_tree);
    let extracted = read_system_integration_assets(published);
    assert_eq!(extracted, authored, "system-integration-assets: locked archive bytes or modes drifted");
}

#[test]
fn system_integration_assets_declaration_and_assets_fail_closed() {
    let (package, lock, source_tree) = system_integration_contract_fixture();
    validate_system_integration_recipe(&package, &lock).unwrap();
    let assets = read_system_integration_assets(&source_tree);
    validate_system_integration_assets(&assets).unwrap();

    let reject_package = |label: &str, candidate: PackageSpec| {
        assert!(
            validate_system_integration_recipe(&candidate, &lock).is_err(),
            "system-integration-assets mutation must fail closed: {label}"
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
    candidate.builder.phases.check.steps.swap(0, 1);
    reject_package("validator topology", candidate);
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
    assert!(validate_system_integration_recipe(&package, &candidate_lock).is_err());

    let mut tampered_assets = assets.clone();
    tampered_assets
        .get_mut("integration/io.cast.SystemIntegrationFixture.rules")
        .unwrap()
        .0
        .push(b' ');
    assert!(validate_system_integration_assets(&tampered_assets).is_err());
    let mut wrong_mode = assets;
    wrong_mode
        .get_mut("integration/cast-system-integration-fixture")
        .unwrap()
        .1 = 0o644;
    assert!(validate_system_integration_assets(&wrong_mode).is_err());
}

#[test]
fn system_integration_assets_tampered_archive_never_becomes_consumable() {
    let (package, lock, _) = system_integration_contract_fixture();
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
    let tampered = temporary.path().join("cast-system-integration-assets-fixture-1.0.0.tar");
    fs::write(&tampered, b"tampered system integration archive\n").unwrap();
    let cache = temporary.path().join("cache");
    assert!(crate::upstream::import_locked_archive_fixture(&locked, &cache, &tampered).is_err());
    assert!(!execution_source_cache_path(&cache, &source.url, &source.sha256).exists());
}
