const FONT_FAMILY_ARCHIVE_URL: &str =
    "https://fixtures.invalid/sources/cast-font-family-fixture-1.0.0.tar";
const FONT_FAMILY_ARCHIVE_SHA256: &str =
    "8710f0728fbde240fd94ce8bce46c4e4d71336b8470416e8da7c0895dc2d700c";
const FONT_FAMILY_INSTALL_SCRIPT: &str = r#"install -Dm644 fonts/CastAsterFixture-Regular.ttf "${CAST_INSTALL_ROOT}${CAST_DATADIR}/fonts/truetype/cast-aster-fixture/CastAsterFixture-Regular.ttf"
install -Dm644 fonts/CastAsterFixture-Bold.ttf "${CAST_INSTALL_ROOT}${CAST_DATADIR}/fonts/truetype/cast-aster-fixture/CastAsterFixture-Bold.ttf"
install -Dm644 OFL.txt "${CAST_INSTALL_ROOT}${CAST_DATADIR}/licenses/cast-font-family-fixture/OFL.txt""#;
const FONT_FAMILY_CHECK_SCRIPT: &str = r#"font_root="${CAST_INSTALL_ROOT}${CAST_DATADIR}/fonts/truetype/cast-aster-fixture"
regular="$font_root/CastAsterFixture-Regular.ttf"
bold="$font_root/CastAsterFixture-Bold.ttf"
[ "$(fc-scan --format '%{family[0]}|%{style[0]}|%{fontformat}|%{fullname[0]}|%{postscriptname}\n' "$regular")" = 'Cast Aster Fixture|Regular|TrueType|Cast Aster Fixture Regular|CastAsterFixture-Regular' ]
[ "$(fc-scan --format '%{family[0]}|%{style[0]}|%{fontformat}|%{fullname[0]}|%{postscriptname}\n' "$bold")" = 'Cast Aster Fixture|Bold|TrueType|Cast Aster Fixture Bold|CastAsterFixture-Bold' ]
set -- "$font_root/"*.ttf
[ "$#" -eq 2 ]
for cache in "$font_root/"fonts.cache-* "$font_root/fonts.dir" "$font_root/fonts.scale"; do
    [ ! -e "$cache" ]
done"#;

const FONT_FAMILY_ASSETS: [(&str, &str, u32, usize); 5] = [
    (
        "OFL.txt",
        "9c462076a0345befde9c6e61bac93f8135b3432a8d4539f7d8803e3247a58ae7",
        0o644,
        4_344,
    ),
    (
        "PROVENANCE",
        "baa0f0f1f668d29f085072215bbcaf4cdc1d8b45a00309204336764bdfa29017",
        0o644,
        849,
    ),
    (
        "fonts/CastAsterFixture-Bold.ttf",
        "d911446419017339d2efc88a700b908bc0322239cf90de606339fd63c936b017",
        0o644,
        1_276,
    ),
    (
        "fonts/CastAsterFixture-Regular.ttf",
        "2e8f53f901eed7937f2ae68651055cb2e8a45e14ae37e53e72dda1813457ce4e",
        0o644,
        1_300,
    ),
    (
        "source/generate_cast_aster_fixture.rs",
        "96df58900db9c37afaa9dd68fa605f02e7ef2eb92cfaa62ef4cb975a0a2596de",
        0o644,
        12_320,
    ),
];

type FontFamilySnapshot = BTreeMap<String, (Vec<u8>, u32)>;

fn font_family_path_shape(path: &stone_recipe::PathSpec) -> (&'static str, &str) {
    match path {
        stone_recipe::PathSpec::Any { path } => ("any", path),
        stone_recipe::PathSpec::Exe { path } => ("exe", path),
        stone_recipe::PathSpec::Symlink { path } => ("symlink", path),
        stone_recipe::PathSpec::Special { path } => ("special", path),
    }
}

fn validate_font_family_recipe(package: &PackageSpec, lock: &SourceLock) -> Result<(), String> {
    lock.validate_against(&package.sources)
        .map_err(|error| format!("font-family lock mismatch: {error}"))?;
    if dependency_names(&package.builder.required_tools) != ["binary(dash)", "binary(install)"] {
        return Err("builder tools must remain exactly dash and install".to_owned());
    }
    if !package.native_build_inputs.is_empty() || !package.build_inputs.is_empty() {
        return Err("install-only font fixture gained build inputs".to_owned());
    }
    if dependency_names(&package.check_inputs) != ["binary(fc-scan)"] {
        return Err("font metadata check capability drifted".to_owned());
    }
    if !package.builder.phases.setup.steps.is_empty()
        || !package.builder.phases.build.steps.is_empty()
        || !package.builder.phases.workload.steps.is_empty()
    {
        return Err("install-only font fixture gained setup, build, or workload commands".to_owned());
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
        || script != FONT_FAMILY_INSTALL_SCRIPT
    {
        return Err("font installation program or exact staged paths drifted".to_owned());
    }
    let [StepSpec::Shell {
        interpreter: check_interpreter,
        declared_programs,
        script: check_script,
    }] = package.builder.phases.check.steps.as_slice()
    else {
        return Err("check phase must remain one typed shell step".to_owned());
    };
    if check_interpreter.path != "/usr/bin/dash"
        || check_interpreter.requirement != DependencySpec::Binary("dash".to_owned())
        || !matches!(declared_programs.as_slice(), [program]
            if program.path == "/usr/bin/fc-scan"
                && program.requirement == DependencySpec::Binary("fc-scan".to_owned()))
        || check_script != FONT_FAMILY_CHECK_SCRIPT
    {
        return Err("font metadata validation contract drifted".to_owned());
    }
    if package.hooks != stone_recipe::package::HooksSpec::default() {
        return Err("fixture must not hide font generation or validation in hooks".to_owned());
    }
    if package.architectures != ["x86_64"] {
        return Err("fixture target architecture drifted".to_owned());
    }
    let [output] = package.outputs.as_slice() else {
        return Err("fixture must emit exactly one explicit output".to_owned());
    };
    if output.name != "out"
        || !output.include_in_manifest
        || output.summary.as_deref() != Some("Deterministic install-only TrueType fixture")
        || output.description.as_deref()
            != Some(
                "A self-authored Regular and Bold font family with semantic metadata validation and no generated font cache.",
            )
        || !output.runtime_inputs.is_empty()
    {
        return Err("font output metadata or empty runtime relation drifted".to_owned());
    }
    let paths = output.paths.iter().map(font_family_path_shape).collect::<Vec<_>>();
    if paths
        != [
            ("any", "/usr/share/fonts/truetype/cast-aster-fixture"),
            ("any", "/usr/share/licenses/cast-font-family-fixture/OFL.txt"),
        ]
    {
        return Err("font output routing drifted".to_owned());
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
    if url != FONT_FAMILY_ARCHIVE_URL
        || hash != FONT_FAMILY_ARCHIVE_SHA256
        || rename.as_deref() != Some("cast-font-family-fixture.tar")
        || *strip_dirs != Some(1)
        || !*unpack
        || unpack_dir.as_deref() != Some("cast-font-family-fixture")
    {
        return Err("font archive identity or extraction policy drifted".to_owned());
    }
    let [SourceResolution::Archive(source)] = lock.sources.as_slice() else {
        return Err("source lock must retain one archive entry".to_owned());
    };
    if source.order != 0
        || source.url != FONT_FAMILY_ARCHIVE_URL
        || source.sha256 != FONT_FAMILY_ARCHIVE_SHA256
    {
        return Err("font source lock identity drifted".to_owned());
    }
    Ok(())
}

fn read_font_family_assets(source_tree: &Path) -> FontFamilySnapshot {
    let root_metadata = fs::symlink_metadata(source_tree).unwrap();
    assert!(root_metadata.file_type().is_dir(), "font-family: source root is unsafe");
    assert_eq!(root_metadata.mode() & 0o7777, 0o755, "font-family: source root mode drifted");
    for directory in ["fonts", "source"] {
        let metadata = fs::symlink_metadata(source_tree.join(directory)).unwrap();
        assert!(metadata.file_type().is_dir(), "font-family: unsafe {directory} directory");
        assert_eq!(metadata.mode() & 0o7777, 0o755, "font-family: {directory} mode drifted");
    }
    let root_names = fs::read_dir(source_tree)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect::<BTreeSet<_>>();
    assert_eq!(
        root_names,
        BTreeSet::from([
            "OFL.txt".to_owned(),
            "PROVENANCE".to_owned(),
            "fonts".to_owned(),
            "source".to_owned(),
        ])
    );
    let fonts = fs::read_dir(source_tree.join("fonts"))
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect::<BTreeSet<_>>();
    assert_eq!(
        fonts,
        BTreeSet::from([
            "CastAsterFixture-Bold.ttf".to_owned(),
            "CastAsterFixture-Regular.ttf".to_owned(),
        ])
    );
    let source = fs::read_dir(source_tree.join("source"))
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect::<BTreeSet<_>>();
    assert_eq!(source, BTreeSet::from(["generate_cast_aster_fixture.rs".to_owned()]));
    FONT_FAMILY_ASSETS
        .iter()
        .map(|(relative, _, _, _)| {
            let path = source_tree.join(relative);
            let metadata = fs::symlink_metadata(&path).unwrap();
            assert!(metadata.file_type().is_file(), "font-family: unsafe authored {relative}");
            assert_eq!(metadata.nlink(), 1, "font-family: multiply-linked authored {relative}");
            (relative.to_string(), (fs::read(path).unwrap(), metadata.mode() & 0o7777))
        })
        .collect()
}

fn sfnt_checksum(bytes: &[u8]) -> u32 {
    bytes.chunks(4).fold(0_u32, |sum, chunk| {
        let mut word = [0_u8; 4];
        word[..chunk.len()].copy_from_slice(chunk);
        sum.wrapping_add(u32::from_be_bytes(word))
    })
}

fn validate_font_family_assets(snapshot: &FontFamilySnapshot) -> Result<(), String> {
    if snapshot.len() != FONT_FAMILY_ASSETS.len() {
        return Err("authored font source cardinality drifted".to_owned());
    }
    for (relative, sha256, mode, size) in FONT_FAMILY_ASSETS {
        let Some((bytes, actual_mode)) = snapshot.get(relative) else {
            return Err(format!("missing authored font source {relative}"));
        };
        if *actual_mode != mode || bytes.len() != size || hex::encode(Sha256::digest(bytes)) != sha256 {
            return Err(format!("authored font source bytes or mode drifted: {relative}"));
        }
    }
    for relative in [
        "fonts/CastAsterFixture-Regular.ttf",
        "fonts/CastAsterFixture-Bold.ttf",
    ] {
        let bytes = &snapshot[relative].0;
        if !bytes.starts_with(&[0, 1, 0, 0]) || sfnt_checksum(bytes) != 0xb1b0_afba {
            return Err(format!("tracked font is not a checksum-complete TrueType sfnt: {relative}"));
        }
    }
    let provenance = std::str::from_utf8(&snapshot["PROVENANCE"].0).unwrap();
    let generator = std::str::from_utf8(&snapshot["source/generate_cast_aster_fixture.rs"].0).unwrap();
    let license = std::str::from_utf8(&snapshot["OFL.txt"].0).unwrap();
    for (text, fragment) in [
        (provenance, "not copied from or derived from an external font"),
        (provenance, "1700000000"),
        (generator, "const CREATED_1904_SECONDS: u64 = 3_782_844_800;"),
        (generator, "assert_eq!(table_checksum(&font), 0xb1b0_afba);"),
        (license, "SIL OPEN FONT LICENSE Version 1.1"),
    ] {
        if text.matches(fragment).count() != 1 {
            return Err(format!("font provenance must contain exactly one {fragment:?}"));
        }
    }
    Ok(())
}

fn font_family_contract_fixture() -> (PackageSpec, SourceLock, PathBuf) {
    let package_root = execution_fixture_package_directory("font-family");
    let recipe = crate::Recipe::load_authored(package_root.join("stone.glu")).unwrap();
    let lock = evaluate_source_lock(
        SOURCE_LOCK_FILE_NAME,
        &fs::read(package_root.join(SOURCE_LOCK_FILE_NAME)).unwrap(),
    )
    .unwrap();
    let source_tree = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/gluon/execution/source-trees/cast-font-family-fixture-1.0.0");
    (recipe.declaration, lock, source_tree)
}

fn assert_font_family_fixture_contract(package: &PackageSpec, source_tree: &Path) {
    let lock_root = execution_fixture_package_directory("font-family");
    let lock = evaluate_source_lock(
        SOURCE_LOCK_FILE_NAME,
        &fs::read(lock_root.join(SOURCE_LOCK_FILE_NAME)).unwrap(),
    )
    .unwrap();
    validate_font_family_recipe(package, &lock).unwrap();
    validate_font_family_assets(&read_font_family_assets(source_tree)).unwrap();
}

fn assert_font_family_archive_matches_tracked_sources(source_tree: &Path, published: &Path) {
    assert_eq!(
        read_font_family_assets(published),
        read_font_family_assets(source_tree),
        "font-family: locked archive bytes or modes drifted"
    );
}

#[test]
fn font_family_declaration_and_assets_fail_closed() {
    let (package, lock, source_tree) = font_family_contract_fixture();
    validate_font_family_recipe(&package, &lock).unwrap();
    let assets = read_font_family_assets(&source_tree);
    validate_font_family_assets(&assets).unwrap();

    let reject_package = |label: &str, candidate: PackageSpec| {
        assert!(
            validate_font_family_recipe(&candidate, &lock).is_err(),
            "font-family mutation must fail closed: {label}"
        );
    };
    let mut candidate = package.clone();
    candidate.builder.required_tools.pop();
    reject_package("builder tool", candidate);
    let mut candidate = package.clone();
    candidate.check_inputs.pop();
    reject_package("check provider", candidate);
    let mut candidate = package.clone();
    candidate.outputs[0]
        .runtime_inputs
        .push(DependencySpec::Package(stone_recipe::package::PackageRef {
            name: "fontconfig".to_owned(),
        }));
    reject_package("runtime input", candidate);
    let mut candidate = package.clone();
    candidate.outputs[0].paths.pop();
    reject_package("output path", candidate);
    let mut candidate = package.clone();
    let StepSpec::Shell { script, .. } = &mut candidate.builder.phases.install.steps[0] else {
        unreachable!()
    };
    script.push_str("\nfc-cache");
    reject_package("install cache generation", candidate);
    let mut candidate = package.clone();
    let StepSpec::Shell { script, .. } = &mut candidate.builder.phases.check.steps[0] else {
        unreachable!()
    };
    script.push_str("\ntrue");
    reject_package("metadata check", candidate);
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
    assert!(validate_font_family_recipe(&package, &candidate_lock).is_err());

    for relative in [
        "fonts/CastAsterFixture-Regular.ttf",
        "source/generate_cast_aster_fixture.rs",
    ] {
        let mut tampered = assets.clone();
        tampered.get_mut(relative).unwrap().0.push(b' ');
        assert!(validate_font_family_assets(&tampered).is_err(), "accepted tampered {relative}");
    }
    let mut wrong_mode = assets;
    wrong_mode.get_mut("PROVENANCE").unwrap().1 = 0o600;
    assert!(validate_font_family_assets(&wrong_mode).is_err());
}

#[test]
fn font_family_tampered_archive_never_becomes_consumable() {
    let (package, lock, _) = font_family_contract_fixture();
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
    let tampered = temporary.path().join("cast-font-family-fixture-1.0.0.tar");
    fs::write(&tampered, b"tampered font archive\n").unwrap();
    let cache = temporary.path().join("cache");
    assert!(crate::upstream::import_locked_archive_fixture(&locked, &cache, &tampered).is_err());
    assert!(!execution_source_cache_path(&cache, &source.url, &source.sha256).exists());
}
