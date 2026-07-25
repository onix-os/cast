const PYTHON_MODULE_ARCHIVE_URL: &str = "https://fixtures.invalid/sources/cast-python-module-fixture-1.0.0.tar";
const PYTHON_MODULE_ARCHIVE_SHA256: &str = "c21e8157b3119a2453aa76de45672b7c8f63a761c973d4fdbd20893958b402f7";
const PYTHON_MODULE_SETUP_SCRIPT: &str = r#"test -s pyproject.toml
test -s src/cast_python_module_fixture/__init__.py
test -s src/cast_python_module_fixture/codec.py
test -s src/cast_python_module_fixture/__main__.py
test -s tests/test_codec.py
test ! -e setup.py
test ! -e setup.cfg
test ! -e tox.ini"#;
const PYTHON_MODULE_BUILD_SCRIPT: &str = r#"export HOME="${CAST_BUILD_ROOT}/home"
export PYTHONHASHSEED=0
export PYTHONDONTWRITEBYTECODE=1
umask 022
python3 -m build --wheel --no-isolation --outdir dist
shopt -s nullglob
wheels=(dist/*.whl)
test "${#wheels[@]}" -eq 1
test "${wheels[0]}" = dist/cast_python_module_fixture-1.0.0-py3-none-any.whl
test -s "${wheels[0]}""#;
const PYTHON_MODULE_CHECK_SCRIPT: &str = r#"export HOME="${CAST_BUILD_ROOT}/home"
export PYTHONHASHSEED=0
export PYTHONDONTWRITEBYTECODE=1
umask 022
python3 -m pytest -q
PYTHONPATH=src python3 -m cast_python_module_fixture --self-test"#;
const PYTHON_MODULE_INSTALL_SCRIPT: &str = r#"export HOME="${CAST_BUILD_ROOT}/home"
export PYTHONHASHSEED=0
export PYTHONDONTWRITEBYTECODE=1
umask 022
python3 -m installer --destdir "${CAST_INSTALL_ROOT}" --prefix "${CAST_PREFIX}" --no-compile-bytecode --validate-record all dist/cast_python_module_fixture-1.0.0-py3-none-any.whl
staged_purelib=$(python3 -c 'import sysconfig, sys; print(sysconfig.get_path("purelib", vars={"base": sys.argv[1], "platbase": sys.argv[1]}))' "${CAST_INSTALL_ROOT}${CAST_PREFIX}")
test -s "${staged_purelib}/cast_python_module_fixture/codec.py"
test -s "${staged_purelib}/cast_python_module_fixture-1.0.0.dist-info/METADATA"
PYTHONPATH="${staged_purelib}" "${CAST_INSTALL_ROOT}${CAST_BINDIR}/cast-python-module-fixture" --self-test"#;

const PYTHON_MODULE_ASSETS: [(&str, &str, usize); 7] = [
    (
        "LICENSE",
        "55a79c52601a388f20391fdd5fbd3a26b34a9e1436655320b3096d2809fb1682",
        1_077,
    ),
    (
        "README.md",
        "e2b86ebaa40bf9c9cb48ef204d7ac150f0df4c01c4e22b6d7e415b488b52ad61",
        277,
    ),
    (
        "pyproject.toml",
        "10e153e93b863f476f8b18b3a6a0b2463ecd08b16b8a2d760b74d27cb3a00ef4",
        677,
    ),
    (
        "src/cast_python_module_fixture/__init__.py",
        "dcb113712f1f498110682996e43c0cf32bbca8cd184c23f798e349a094e49b21",
        128,
    ),
    (
        "src/cast_python_module_fixture/__main__.py",
        "39500b071c2a48deb14de92faf82e053f144c76ce7e0eb2d188bfd0624921296",
        517,
    ),
    (
        "src/cast_python_module_fixture/codec.py",
        "8d53b9c36d19f586cdba2ef519536d614ba29f758fbfdc8197b7deeec77ac9fd",
        746,
    ),
    (
        "tests/test_codec.py",
        "604740f65a2effb2b094ebe026fec734d4741e1ef5ed9a977c49fa3dcc4afc28",
        620,
    ),
];

type PythonModuleSnapshot = BTreeMap<String, (Vec<u8>, u32)>;

fn assert_python_module_shell(step: &StepSpec, expected_script: &str) -> Result<(), String> {
    let StepSpec::Shell {
        interpreter,
        declared_programs,
        script,
    } = step
    else {
        return Err("expected one typed Bash shell step".to_owned());
    };
    if interpreter.path != "/usr/bin/bash"
        || interpreter.requirement != DependencySpec::Binary("bash".to_owned())
        || declared_programs.len() != 1
        || declared_programs[0].path != "/usr/bin/python3"
        || declared_programs[0].requirement != DependencySpec::Binary("python3".to_owned())
        || script != expected_script
    {
        return Err("typed Python shell step, declared program, or exact script drifted".to_owned());
    }
    Ok(())
}

fn validate_python_module_recipe(package: &PackageSpec, lock: &SourceLock) -> Result<(), String> {
    lock.validate_against(&package.sources)
        .map_err(|error| format!("python-module lock mismatch: {error}"))?;
    if package.meta.pname != "cast-python-module-fixture"
        || package.meta.version != "1.0.0"
        || package.meta.release != 1
        || package.meta.homepage != "https://fixtures.invalid/cast-python-module-fixture"
        || package.meta.license != ["MIT"]
    {
        return Err("package identity drifted".to_owned());
    }
    if dependency_names(&package.builder.required_tools) != ["binary(bash)", "binary(python3)"] {
        return Err("builder tools must remain exactly Bash and Python".to_owned());
    }
    if dependency_names(&package.native_build_inputs) != ["python(build)", "python(installer)", "python(setuptools)"]
        || !package.build_inputs.is_empty()
        || dependency_names(&package.check_inputs) != ["python(pytest)"]
    {
        return Err("PEP 517 build, install, and check dependency roles drifted".to_owned());
    }
    let [setup] = package.builder.phases.setup.steps.as_slice() else {
        return Err("setup must remain one explicit shell step".to_owned());
    };
    let StepSpec::Shell {
        interpreter,
        declared_programs,
        script,
    } = setup
    else {
        return Err("setup must remain one explicit shell step".to_owned());
    };
    if interpreter.path != "/usr/bin/bash"
        || interpreter.requirement != DependencySpec::Binary("bash".to_owned())
        || !declared_programs.is_empty()
        || script != PYTHON_MODULE_SETUP_SCRIPT
    {
        return Err("setup source-boundary check drifted".to_owned());
    }
    let [build] = package.builder.phases.build.steps.as_slice() else {
        return Err("build must remain one typed shell step".to_owned());
    };
    assert_python_module_shell(build, PYTHON_MODULE_BUILD_SCRIPT)?;
    let [check] = package.builder.phases.check.steps.as_slice() else {
        return Err("check must remain one typed shell step".to_owned());
    };
    assert_python_module_shell(check, PYTHON_MODULE_CHECK_SCRIPT)?;
    let [install] = package.builder.phases.install.steps.as_slice() else {
        return Err("install must remain one typed shell step".to_owned());
    };
    assert_python_module_shell(install, PYTHON_MODULE_INSTALL_SCRIPT)?;
    if !package.builder.phases.workload.steps.is_empty()
        || package.hooks != stone_recipe::package::HooksSpec::default()
        || package.options.networking
        || package.architectures != ["x86_64"]
    {
        return Err("fixture gained hidden work, networking, hooks, or architectures".to_owned());
    }
    let [output] = package.outputs.as_slice() else {
        return Err("fixture must emit exactly one explicit output".to_owned());
    };
    if output.name != "out"
        || !output.include_in_manifest
        || output.summary.as_deref() != Some("Pinned offline PEP 517 Python module fixture")
        || output.description.as_deref()
            != Some(
                "A reproducible pure-Python wheel built, tested, installed, and packaged from one exact self-authored source archive.",
            )
        || dependency_names(&output.runtime_inputs) != ["binary(python3)", "python(typing-extensions)"]
    {
        return Err("Python output metadata or runtime boundary drifted".to_owned());
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
            ("exe", "/usr/bin/cast-python-module-fixture"),
            ("any", "/usr/lib/python*/site-packages/cast_python_module_fixture"),
            (
                "any",
                "/usr/lib/python*/site-packages/cast_python_module_fixture-1.0.0.dist-info",
            ),
        ]
    {
        return Err("Python output routing drifted".to_owned());
    }
    let [
        UpstreamSpec::Archive {
            url,
            hash,
            rename,
            strip_dirs,
            unpack,
            unpack_dir,
        },
    ] = package.sources.as_slice()
    else {
        return Err("fixture must retain exactly one archive source".to_owned());
    };
    if url != PYTHON_MODULE_ARCHIVE_URL
        || hash != PYTHON_MODULE_ARCHIVE_SHA256
        || rename.as_deref() != Some("cast-python-module-fixture.tar")
        || *strip_dirs != Some(1)
        || !*unpack
        || unpack_dir.as_deref() != Some("cast-python-module-fixture")
    {
        return Err("Python archive identity or extraction policy drifted".to_owned());
    }
    let [SourceResolution::Archive(source)] = lock.sources.as_slice() else {
        return Err("source lock must retain one archive entry".to_owned());
    };
    if source.order != 0 || source.url != PYTHON_MODULE_ARCHIVE_URL || source.sha256 != PYTHON_MODULE_ARCHIVE_SHA256 {
        return Err("Python source lock identity drifted".to_owned());
    }
    Ok(())
}

fn read_python_module_assets(source_tree: &Path) -> PythonModuleSnapshot {
    let root_metadata = fs::symlink_metadata(source_tree).unwrap();
    assert!(
        root_metadata.file_type().is_dir(),
        "python-module: source root is unsafe"
    );
    assert_eq!(
        root_metadata.mode() & 0o7777,
        0o755,
        "python-module: source root mode drifted"
    );
    let expected_directories = BTreeSet::from([
        "src".to_owned(),
        "src/cast_python_module_fixture".to_owned(),
        "tests".to_owned(),
    ]);
    let mut directories = BTreeSet::new();
    let mut assets = BTreeMap::new();
    let mut pending = vec![source_tree.to_owned()];
    while let Some(directory) = pending.pop() {
        for entry in fs::read_dir(&directory).unwrap() {
            let entry = entry.unwrap();
            let path = entry.path();
            let metadata = fs::symlink_metadata(&path).unwrap();
            let relative = path.strip_prefix(source_tree).unwrap().to_string_lossy().into_owned();
            if metadata.file_type().is_dir() {
                assert_eq!(
                    metadata.mode() & 0o7777,
                    0o755,
                    "python-module: directory mode drifted: {relative}"
                );
                assert!(directories.insert(relative));
                pending.push(path);
            } else {
                assert!(
                    metadata.file_type().is_file(),
                    "python-module: unsafe source entry: {relative}"
                );
                assert_eq!(metadata.nlink(), 1, "python-module: multiply-linked source: {relative}");
                assert!(
                    assets
                        .insert(relative, (fs::read(path).unwrap(), metadata.mode() & 0o7777))
                        .is_none()
                );
            }
        }
    }
    assert_eq!(
        directories, expected_directories,
        "python-module: source directory shape drifted"
    );
    assets
}

fn validate_python_module_assets(snapshot: &PythonModuleSnapshot) -> Result<(), String> {
    let expected_names = PYTHON_MODULE_ASSETS
        .iter()
        .map(|(relative, _, _)| relative.to_string())
        .collect::<BTreeSet<_>>();
    if snapshot.keys().cloned().collect::<BTreeSet<_>>() != expected_names {
        return Err("authored Python source inventory drifted".to_owned());
    }
    for (relative, sha256, size) in PYTHON_MODULE_ASSETS {
        let Some((bytes, mode)) = snapshot.get(relative) else {
            return Err(format!("missing authored Python source {relative}"));
        };
        if *mode != 0o644 || bytes.len() != size || hex::encode(Sha256::digest(bytes)) != sha256 {
            return Err(format!("authored Python source bytes or mode drifted: {relative}"));
        }
    }
    let text = |relative: &str| std::str::from_utf8(&snapshot[relative].0).unwrap();
    let pyproject = text("pyproject.toml");
    for marker in [
        "build-backend = \"setuptools.build_meta\"",
        "requires = [\"setuptools>=82\", \"wheel>=0.47\"]",
        "dependencies = [\"typing-extensions>=4.15\"]",
        "cast-python-module-fixture = \"cast_python_module_fixture.__main__:main\"",
        "package-dir = { \"\" = \"src\" }",
    ] {
        if pyproject.matches(marker).count() != 1 {
            return Err(format!("pyproject PEP 517 marker drifted: {marker}"));
        }
    }
    if text("src/cast_python_module_fixture/codec.py")
        .matches("from typing_extensions import TypedDict")
        .count()
        != 1
        || text("src/cast_python_module_fixture/codec.py")
            .matches("separators=(\",\", \":\"), sort_keys=True")
            .count()
            != 1
        || text("src/cast_python_module_fixture/__main__.py")
            .matches("cast python module fixture: offline PEP 517 wheel")
            .count()
            != 1
        || text("tests/test_codec.py")
            .matches("b'{\"code\":17,\"message\":\"declarative userspace\"}'")
            .count()
            != 1
    {
        return Err("Python runtime, identity, or test contract drifted".to_owned());
    }
    Ok(())
}

fn python_module_contract_fixture() -> (PackageSpec, SourceLock, PathBuf) {
    let package_root = execution_fixture_package_directory("python-module");
    let recipe = crate::Recipe::load_authored(package_root.join("stone.glu")).unwrap();
    let lock = evaluate_source_lock(
        SOURCE_LOCK_FILE_NAME,
        &fs::read(package_root.join(SOURCE_LOCK_FILE_NAME)).unwrap(),
    )
    .unwrap();
    let source_tree = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/gluon/execution/source-trees/cast-python-module-fixture-1.0.0");
    (recipe.declaration, lock, source_tree)
}

fn assert_python_module_fixture_contract(package: &PackageSpec, source_tree: &Path) {
    let package_root = execution_fixture_package_directory("python-module");
    let lock = evaluate_source_lock(
        SOURCE_LOCK_FILE_NAME,
        &fs::read(package_root.join(SOURCE_LOCK_FILE_NAME)).unwrap(),
    )
    .unwrap();
    validate_python_module_recipe(package, &lock).unwrap();
    validate_python_module_assets(&read_python_module_assets(source_tree)).unwrap();
}

fn assert_python_module_archive_matches_tracked_sources(source_tree: &Path, published: &Path) {
    assert_eq!(
        read_python_module_assets(published),
        read_python_module_assets(source_tree),
        "python-module: locked archive bytes or modes drifted"
    );
}

#[test]
fn python_module_declaration_and_pep517_tree_fail_closed() {
    let (package, lock, source_tree) = python_module_contract_fixture();
    validate_python_module_recipe(&package, &lock).unwrap();
    let assets = read_python_module_assets(&source_tree);
    validate_python_module_assets(&assets).unwrap();

    let reject_package = |label: &str, candidate: PackageSpec| {
        assert!(
            validate_python_module_recipe(&candidate, &lock).is_err(),
            "python-module mutation must fail closed: {label}"
        );
    };
    let mut candidate = package.clone();
    candidate.native_build_inputs.pop();
    reject_package("PEP 517 provider", candidate);
    let mut candidate = package.clone();
    candidate.options.networking = true;
    reject_package("networking", candidate);
    let mut candidate = package.clone();
    let StepSpec::Shell { script, .. } = &mut candidate.builder.phases.build.steps[0] else {
        unreachable!()
    };
    script.push_str("\npython3 -m pip install build");
    reject_package("offline build", candidate);
    let mut candidate = package.clone();
    candidate.outputs[0].runtime_inputs.pop();
    reject_package("runtime dependency", candidate);
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
    assert!(validate_python_module_recipe(&package, &candidate_lock).is_err());

    let mut legacy = assets.clone();
    legacy.insert(
        "setup.py".to_owned(),
        (b"from setuptools import setup\nsetup()\n".to_vec(), 0o644),
    );
    assert!(validate_python_module_assets(&legacy).is_err());
    let mut tampered = assets;
    tampered
        .get_mut("src/cast_python_module_fixture/codec.py")
        .unwrap()
        .0
        .push(b' ');
    assert!(validate_python_module_assets(&tampered).is_err());
}

#[test]
fn python_module_tampered_archive_never_becomes_consumable() {
    let (package, lock, _) = python_module_contract_fixture();
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
    let tampered = temporary.path().join("cast-python-module-fixture-1.0.0.tar");
    fs::write(&tampered, b"tampered Python archive\n").unwrap();
    let cache = temporary.path().join("cache");
    assert!(crate::upstream::import_locked_archive_fixture(&locked, &cache, &tampered).is_err());
    assert!(!execution_source_cache_path(&cache, &source.url, &source.sha256).exists());
}
