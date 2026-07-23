const GO_MODULE_ARCHIVE_URL: &str =
    "https://fixtures.invalid/sources/cast-go-module-fixture-1.0.0.tar.zst";
const GO_MODULE_ARCHIVE_SHA256: &str =
    "f4c4eb74304956e3f3e650d2004c78a39c4d4c009e447b9c28b3819370dbc78f";
const GO_MODULE_SETUP_SCRIPT: &str = r#"test -s go.mod
test -s go.sum
test -s vendor/modules.txt
test -s vendor/fixtures.invalid/cast/go-message/message.go
test ! -e go.work
test ! -e vendor/fixtures.invalid/cast/go-message/go.mod
replace_pattern='^[[:space:]]*replace([[:space:](]|$)'
while IFS= read -r line; do
    [[ ! $line =~ $replace_pattern ]] || exit 1
done < go.mod"#;
const GO_MODULE_BUILD_SCRIPT: &str = r#"export HOME="${CAST_BUILD_ROOT}/home"
export XDG_CONFIG_HOME="${CAST_BUILD_ROOT}/xdg-config"
export GOROOT=/usr/lib/golang
export GOCACHE="${CAST_BUILD_ROOT}/go-cache"
export GOMODCACHE="${CAST_BUILD_ROOT}/go-mod-cache"
export GOENV=off
export GOWORK=off
export GOTOOLCHAIN=local
export GOPROXY=off
export GOSUMDB=off
export GONOSUMDB='*'
export GONOPROXY=none
export GOFLAGS=
export GO111MODULE=on
export CGO_ENABLED=0
export GOOS=linux
export GOARCH=amd64
export GOAMD64=v1
go telemetry off
go build -mod=vendor -trimpath -buildvcs=false -ldflags='-buildid= -s -w' -o cast-go-module-fixture ./cmd/cast-go-module-fixture"#;
const GO_MODULE_CHECK_SCRIPT: &str = r#"export HOME="${CAST_BUILD_ROOT}/home"
export XDG_CONFIG_HOME="${CAST_BUILD_ROOT}/xdg-config"
export GOROOT=/usr/lib/golang
export GOCACHE="${CAST_BUILD_ROOT}/go-cache"
export GOMODCACHE="${CAST_BUILD_ROOT}/go-mod-cache"
export GOENV=off
export GOWORK=off
export GOTOOLCHAIN=local
export GOPROXY=off
export GOSUMDB=off
export GONOSUMDB='*'
export GONOPROXY=none
export GOFLAGS=
export GO111MODULE=on
export CGO_ENABLED=0
export GOOS=linux
export GOARCH=amd64
export GOAMD64=v1
go telemetry off
go test -mod=vendor -trimpath -count=1 ./..."#;
const GO_MODULE_INSTALL_SCRIPT: &str = r#"install -Dm755 cast-go-module-fixture "${CAST_INSTALL_ROOT}${CAST_BINDIR}/cast-go-module-fixture"
install -Dm644 LICENSE "${CAST_INSTALL_ROOT}${CAST_DATADIR}/licenses/cast-go-module-fixture/LICENSE""#;

const GO_MODULE_ASSETS: [(&str, &str, usize); 10] = [
    (
        "LICENSE",
        "cb7f1a8220b7fbc5207dffd81e50c6cb62d3d42ca15612ac45bc354bbdf46dd9",
        1_082,
    ),
    (
        "README.md",
        "6471e3d255d33f06906911205db19b3935ec3cfac96b7a1d90d8b43774b7be81",
        545,
    ),
    (
        "cmd/cast-go-module-fixture/main.go",
        "a1d9a6f47e9cfa3524d1b5b5094eb5de496ac422490ccae445c7e2e9604b10f7",
        540,
    ),
    (
        "cmd/cast-go-module-fixture/main_test.go",
        "f57c931498fd6fe3cc24c5ed0bfabde64ec703eb4b21b09c6884cf4403542240",
        272,
    ),
    (
        "go.mod",
        "1cc19152b9e8b97194c2b9e90ffc79111a809d5a298e4e6143ebb0e8fae76597",
        105,
    ),
    (
        "go.sum",
        "5fd8f20f87d76d1aea5c07f9126afbb993cdf8b0ba23efb48fe4a007e1fb8b15",
        183,
    ),
    (
        "internal/application/application.go",
        "97c9fed62a973f86a7b0dd9795dae1a72a89fd0987bb80c293bf0157b7171c64",
        152,
    ),
    (
        "internal/application/application_test.go",
        "0ba13cab346121ca628ea973099dde2f4aa5eabcfb6be1202a0f7d8b31b51bc9",
        273,
    ),
    (
        "vendor/fixtures.invalid/cast/go-message/message.go",
        "9592bfb15254b2395927c9f8fce46fb003daaaeb8c54375197febd66ecf71d4a",
        160,
    ),
    (
        "vendor/modules.txt",
        "1883a56f58081ddfb041d55c13b496d76d581abd62e14f01ee501eb0cb1e6c75",
        96,
    ),
];

type GoModuleSnapshot = BTreeMap<String, (Vec<u8>, u32)>;

fn assert_go_module_shell(
    step: &StepSpec,
    declared: &[&str],
    expected_script: &str,
) -> Result<(), String> {
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
        || declared_programs
            .iter()
            .map(|program| program.requirement.dependency().unwrap().to_name())
            .collect::<Vec<_>>()
            != declared
        || script != expected_script
    {
        return Err("typed Bash step, declared programs, or exact script drifted".to_owned());
    }
    Ok(())
}

fn validate_go_module_recipe(package: &PackageSpec, lock: &SourceLock) -> Result<(), String> {
    lock.validate_against(&package.sources)
        .map_err(|error| format!("go-module lock mismatch: {error}"))?;
    if package.meta.pname != "cast-go-module-fixture"
        || package.meta.version != "1.0.0"
        || package.meta.release != 1
        || package.meta.homepage != "https://fixtures.invalid/cast-go-module-fixture"
        || package.meta.license != ["MIT"]
    {
        return Err("package identity drifted".to_owned());
    }
    if dependency_names(&package.builder.required_tools)
        != ["binary(bash)", "binary(go)", "binary(install)"]
    {
        return Err("builder tools must remain exactly Bash, Go, and install".to_owned());
    }
    if !package.native_build_inputs.is_empty()
        || !package.build_inputs.is_empty()
        || !package.check_inputs.is_empty()
    {
        return Err("Go capabilities must come only from exact typed programs".to_owned());
    }
    let [setup] = package.builder.phases.setup.steps.as_slice() else {
        return Err("setup must remain one typed shell step".to_owned());
    };
    assert_go_module_shell(setup, &[], GO_MODULE_SETUP_SCRIPT)?;
    let [build] = package.builder.phases.build.steps.as_slice() else {
        return Err("build must remain one typed shell step".to_owned());
    };
    assert_go_module_shell(build, &["binary(go)"], GO_MODULE_BUILD_SCRIPT)?;
    let [test, self_test] = package.builder.phases.check.steps.as_slice() else {
        return Err("check must run Go tests and the exact built self-test".to_owned());
    };
    assert_go_module_shell(test, &["binary(go)"], GO_MODULE_CHECK_SCRIPT)?;
    let StepSpec::RunBuilt { program, args } = self_test else {
        return Err("check must execute the built fixture structurally".to_owned());
    };
    if program.path != "cast-go-module-fixture" || args.as_slice() != ["--self-test"] {
        return Err("built self-test identity drifted".to_owned());
    }
    let [install] = package.builder.phases.install.steps.as_slice() else {
        return Err("install must remain one typed shell step".to_owned());
    };
    assert_go_module_shell(install, &["binary(install)"], GO_MODULE_INSTALL_SCRIPT)?;
    if !package.builder.phases.workload.steps.is_empty()
        || package.hooks != stone_recipe::package::HooksSpec::default()
        || package.options.networking
    {
        return Err("fixture gained hidden work or execution networking".to_owned());
    }
    if package.architectures != ["x86_64"] {
        return Err("fixture architecture drifted".to_owned());
    }
    let [output] = package.outputs.as_slice() else {
        return Err("fixture must emit exactly one explicit output".to_owned());
    };
    if output.name != "out"
        || !output.include_in_manifest
        || output.summary.as_deref() != Some("Pinned offline Go module fixture")
        || output.description.as_deref()
            != Some("A reproducible static Go executable built exclusively from an exact self-authored vendor tree.")
        || !output.runtime_inputs.is_empty()
    {
        return Err("Go output metadata or runtime boundary drifted".to_owned());
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
            ("exe", "/usr/bin/cast-go-module-fixture"),
            ("any", "/usr/share/licenses/cast-go-module-fixture/LICENSE"),
        ]
    {
        return Err("Go output routing drifted".to_owned());
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
    if url != GO_MODULE_ARCHIVE_URL
        || hash != GO_MODULE_ARCHIVE_SHA256
        || rename.as_deref() != Some("cast-go-module-fixture.tar.zst")
        || *strip_dirs != Some(1)
        || !*unpack
        || unpack_dir.as_deref() != Some("cast-go-module-fixture")
    {
        return Err("Go archive identity or extraction policy drifted".to_owned());
    }
    let [SourceResolution::Archive(source)] = lock.sources.as_slice() else {
        return Err("source lock must retain one archive entry".to_owned());
    };
    if source.order != 0 || source.url != GO_MODULE_ARCHIVE_URL || source.sha256 != GO_MODULE_ARCHIVE_SHA256 {
        return Err("Go source lock identity drifted".to_owned());
    }
    Ok(())
}

fn read_go_module_assets(source_tree: &Path) -> GoModuleSnapshot {
    let root_metadata = fs::symlink_metadata(source_tree).unwrap();
    assert!(root_metadata.file_type().is_dir(), "go-module: source root is unsafe");
    assert_eq!(root_metadata.mode() & 0o7777, 0o755, "go-module: source root mode drifted");
    let expected_directories = BTreeSet::from([
        "cmd".to_owned(),
        "cmd/cast-go-module-fixture".to_owned(),
        "internal".to_owned(),
        "internal/application".to_owned(),
        "vendor".to_owned(),
        "vendor/fixtures.invalid".to_owned(),
        "vendor/fixtures.invalid/cast".to_owned(),
        "vendor/fixtures.invalid/cast/go-message".to_owned(),
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
                assert_eq!(metadata.mode() & 0o7777, 0o755, "go-module: directory mode drifted: {relative}");
                assert!(directories.insert(relative));
                pending.push(path);
            } else {
                assert!(metadata.file_type().is_file(), "go-module: unsafe source entry: {relative}");
                assert_eq!(metadata.nlink(), 1, "go-module: multiply-linked source: {relative}");
                assert!(
                    assets
                        .insert(relative, (fs::read(path).unwrap(), metadata.mode() & 0o7777))
                        .is_none()
                );
            }
        }
    }
    assert_eq!(directories, expected_directories, "go-module: source directory shape drifted");
    assets
}

fn validate_go_module_assets(snapshot: &GoModuleSnapshot) -> Result<(), String> {
    let expected_names = GO_MODULE_ASSETS
        .iter()
        .map(|(relative, _, _)| relative.to_string())
        .collect::<BTreeSet<_>>();
    if snapshot.keys().cloned().collect::<BTreeSet<_>>() != expected_names {
        return Err("authored Go source inventory drifted".to_owned());
    }
    for (relative, sha256, size) in GO_MODULE_ASSETS {
        let Some((bytes, mode)) = snapshot.get(relative) else {
            return Err(format!("missing authored Go source {relative}"));
        };
        if *mode != 0o644 || bytes.len() != size || hex::encode(Sha256::digest(bytes)) != sha256 {
            return Err(format!("authored Go source bytes or mode drifted: {relative}"));
        }
    }
    let text = |relative: &str| std::str::from_utf8(&snapshot[relative].0).unwrap();
    let go_mod = text("go.mod");
    if go_mod != "module fixtures.invalid/cast/go-module-fixture\n\ngo 1.23\n\nrequire fixtures.invalid/cast/go-message v0.1.0\n"
        || go_mod.lines().any(|line| {
            let line = line.trim_start();
            line == "replace" || line.starts_with("replace ") || line.starts_with("replace(")
        })
    {
        return Err("go.mod identity or no-replace boundary drifted".to_owned());
    }
    if text("go.sum")
        != "fixtures.invalid/cast/go-message v0.1.0 h1:cQagr36IEMfm4r58+Pf8h3NEoEszyzND5F0DJ3M1EZU=\nfixtures.invalid/cast/go-message v0.1.0/go.mod h1:Pn9DbO3fOhatyjAKjf9ICdXCxgfRX9Pi6eZvPYu1evI=\n"
        || text("vendor/modules.txt")
            != "# fixtures.invalid/cast/go-message v0.1.0\n## explicit; go 1.23\nfixtures.invalid/cast/go-message\n"
    {
        return Err("real Go checksum or canonical vendor metadata drifted".to_owned());
    }
    let marker = "cast go module fixture: vendored dependency v0.1.0";
    if text("vendor/fixtures.invalid/cast/go-message/message.go")
        .matches(marker)
        .count()
        != 1
        || text("internal/application/application.go")
            .matches("fixtures.invalid/cast/go-message")
            .count()
            != 1
        || text("cmd/cast-go-module-fixture/main.go")
            .matches("declarative userspace")
            .count()
            != 1
    {
        return Err("vendored dependency use or executable identity drifted".to_owned());
    }
    Ok(())
}

fn go_module_contract_fixture() -> (PackageSpec, SourceLock, PathBuf) {
    let package_root = execution_fixture_package_directory("go-module");
    let recipe = crate::Recipe::load_authored(package_root.join("stone.glu")).unwrap();
    let lock = evaluate_source_lock(
        SOURCE_LOCK_FILE_NAME,
        &fs::read(package_root.join(SOURCE_LOCK_FILE_NAME)).unwrap(),
    )
    .unwrap();
    let source_tree = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/gluon/execution/source-trees/cast-go-module-fixture-1.0.0");
    (recipe.declaration, lock, source_tree)
}

fn assert_go_module_fixture_contract(package: &PackageSpec, source_tree: &Path) {
    let package_root = execution_fixture_package_directory("go-module");
    let lock = evaluate_source_lock(
        SOURCE_LOCK_FILE_NAME,
        &fs::read(package_root.join(SOURCE_LOCK_FILE_NAME)).unwrap(),
    )
    .unwrap();
    validate_go_module_recipe(package, &lock).unwrap();
    validate_go_module_assets(&read_go_module_assets(source_tree)).unwrap();
}

fn assert_go_module_archive_matches_tracked_sources(source_tree: &Path, published: &Path) {
    assert_eq!(
        read_go_module_assets(published),
        read_go_module_assets(source_tree),
        "go-module: locked archive bytes or modes drifted"
    );
}

#[test]
fn go_module_declaration_and_vendor_tree_fail_closed() {
    let (package, lock, source_tree) = go_module_contract_fixture();
    validate_go_module_recipe(&package, &lock).unwrap();
    let assets = read_go_module_assets(&source_tree);
    validate_go_module_assets(&assets).unwrap();

    let reject_package = |label: &str, candidate: PackageSpec| {
        assert!(
            validate_go_module_recipe(&candidate, &lock).is_err(),
            "go-module mutation must fail closed: {label}"
        );
    };
    let mut candidate = package.clone();
    candidate.builder.required_tools.pop();
    reject_package("typed tool", candidate);
    let mut candidate = package.clone();
    candidate.options.networking = true;
    reject_package("networking", candidate);
    let mut candidate = package.clone();
    let StepSpec::Shell { script, .. } = &mut candidate.builder.phases.build.steps[0] else {
        unreachable!()
    };
    script.push_str("\ngo env");
    reject_package("offline build", candidate);
    let mut candidate = package.clone();
    candidate.outputs[0].paths.pop();
    reject_package("output routing", candidate);
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
    assert!(validate_go_module_recipe(&package, &candidate_lock).is_err());

    let mut with_replace = assets.clone();
    with_replace
        .get_mut("go.mod")
        .unwrap()
        .0
        .extend_from_slice(b"replace fixtures.invalid/cast/go-message => ../escape\n");
    assert!(validate_go_module_assets(&with_replace).is_err());
    let mut tampered_vendor = assets;
    tampered_vendor
        .get_mut("vendor/fixtures.invalid/cast/go-message/message.go")
        .unwrap()
        .0
        .push(b' ');
    assert!(validate_go_module_assets(&tampered_vendor).is_err());
}

#[test]
fn go_module_tampered_archive_never_becomes_consumable() {
    let (package, lock, _) = go_module_contract_fixture();
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
    let tampered = temporary.path().join("cast-go-module-fixture-1.0.0.tar.zst");
    fs::write(&tampered, b"tampered Go archive\n").unwrap();
    let cache = temporary.path().join("cache");
    assert!(crate::upstream::import_locked_archive_fixture(&locked, &cache, &tampered).is_err());
    assert!(!execution_source_cache_path(&cache, &source.url, &source.sha256).exists());
}
