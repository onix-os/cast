const PGO_WORKLOAD_ARCHIVE_URL: &str =
    "https://fixtures.invalid/sources/cast-pgo-workload-fixture-1.0.0.tar";
const PGO_WORKLOAD_ARCHIVE_SHA256: &str =
    "84b8b4011b5cff63cb53cb1180cc4550d3e762233c121dfa8490d55fe4035f34";
const PGO_WORKLOAD_BUILD_SCRIPT: &str = r#"set -eu
case "${PGO_STAGE}" in
    ONE)
        test ! -e build/stage1-only.marker
        : > build/stage1-only.marker
        ;;
    USE)
        test ! -e build/stage1-only.marker
        ;;
    *)
        exit 1
        ;;
esac
"${CC}" ${CFLAGS} -std=c11 -Wall -Wextra -Werror main.c ${LDFLAGS} -o build/cast-pgo-workload-fixture"#;
const PGO_WORKLOAD_INSTALL_SCRIPT: &str =
    r#"install -Dm755 build/cast-pgo-workload-fixture "${CAST_INSTALL_ROOT}${CAST_BINDIR}/cast-pgo-workload-fixture""#;

fn assert_pgo_workload_fixture_contract(package: &PackageSpec, source_tree: &Path) {
    let package_root = execution_fixture_package_directory("pgo-workload");
    let lock = decode_source_lock(
        SOURCE_LOCK_FILE_NAME,
        &fs::read(package_root.join(SOURCE_LOCK_FILE_NAME)).unwrap(),
    )
    .unwrap();
    validate_pgo_workload_recipe(package, &lock).unwrap();
    validate_pgo_workload_source(&fs::read_to_string(source_tree.join("main.c")).unwrap()).unwrap();
}

fn validate_pgo_workload_recipe(package: &PackageSpec, lock: &SourceLock) -> Result<(), String> {
    lock.validate_against(&package.sources)
        .map_err(|error| format!("pgo-workload lock mismatch: {error}"))?;
    if package.meta.pname != "cast-pgo-workload-fixture"
        || package.meta.version != "1.0.0"
        || package.meta.release != 1
        || package.meta.homepage != "https://fixtures.invalid/cast-pgo-workload-fixture"
        || package.meta.license != ["MPL-2.0"]
    {
        return Err("package identity drifted".to_owned());
    }
    if dependency_names(&package.builder.required_tools)
        != ["binary(mkdir)", "binary(clang)", "binary(dash)", "binary(install)"]
    {
        return Err("custom builder tools drifted".to_owned());
    }
    if package.options.toolchain != stone_recipe::ToolchainSpec::Llvm
        || package.options.cspgo
        || package.options.samplepgo
        || package.options.debug
        || package.options.networking
        || package.mold
    {
        return Err("two-stage offline LLVM PGO options drifted".to_owned());
    }
    if !package.native_build_inputs.is_empty()
        || !package.build_inputs.is_empty()
        || !package.check_inputs.is_empty()
        || package.hooks != stone_recipe::package::HooksSpec::default()
        || package.architectures != ["x86_64"]
    {
        return Err("fixture gained an ambient input, hook, or target".to_owned());
    }

    let [StepSpec::Run { program, args }] = package.builder.phases.setup.steps.as_slice() else {
        return Err("setup must create the build directory structurally".to_owned());
    };
    if program.path != "/usr/bin/mkdir" || args.as_slice() != ["-p", "build"] {
        return Err("setup command drifted".to_owned());
    }
    let [StepSpec::Shell {
        interpreter,
        declared_programs,
        script,
    }] = package.builder.phases.build.steps.as_slice()
    else {
        return Err("build must remain one declared compiler shell step".to_owned());
    };
    if interpreter.path != "/usr/bin/dash"
        || declared_programs.iter().map(|program| program.path.as_str()).collect::<Vec<_>>() != ["/usr/bin/clang"]
        || script != PGO_WORKLOAD_BUILD_SCRIPT
    {
        return Err("fresh-work-tree compiler contract drifted".to_owned());
    }
    let [StepSpec::RunBuilt { program, args }] = package.builder.phases.workload.steps.as_slice() else {
        return Err("training must execute the instrumented build structurally".to_owned());
    };
    if program.path != "build/cast-pgo-workload-fixture"
        || args.as_slice() != ["--train", "profile-guided-build-2026"]
    {
        return Err("training workload identity drifted".to_owned());
    }
    let [StepSpec::Shell {
        interpreter,
        declared_programs,
        script,
    }] = package.builder.phases.install.steps.as_slice()
    else {
        return Err("install must remain one declared install step".to_owned());
    };
    if interpreter.path != "/usr/bin/dash"
        || declared_programs.iter().map(|program| program.path.as_str()).collect::<Vec<_>>() != ["/usr/bin/install"]
        || script != PGO_WORKLOAD_INSTALL_SCRIPT
    {
        return Err("profile-use install contract drifted".to_owned());
    }
    let [StepSpec::RunBuilt { program, args }] = package.builder.phases.check.steps.as_slice() else {
        return Err("final check must execute the profile-use build structurally".to_owned());
    };
    if program.path != "build/cast-pgo-workload-fixture" || args.as_slice() != ["--self-test"] {
        return Err("profile-use self-test identity drifted".to_owned());
    }

    let [output] = package.outputs.as_slice() else {
        return Err("fixture must emit exactly one explicit output".to_owned());
    };
    if output.name != "out"
        || !output.include_in_manifest
        || output.summary.as_deref() != Some("Offline LLVM profile-guided optimization fixture")
        || output.description.as_deref()
            != Some("A two-job custom build trained structurally before its isolated profile-use rebuild.")
        || !output.runtime_inputs.is_empty()
        || !matches!(
            output.paths.as_slice(),
            [stone_recipe::PathSpec::Exe { path }] if path == "/usr/bin/cast-pgo-workload-fixture"
        )
    {
        return Err("single-output package boundary drifted".to_owned());
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
    if url != PGO_WORKLOAD_ARCHIVE_URL
        || hash != PGO_WORKLOAD_ARCHIVE_SHA256
        || rename.as_deref() != Some("cast-pgo-workload-fixture.tar")
        || *strip_dirs != Some(1)
        || !*unpack
        || unpack_dir.as_deref() != Some("cast-pgo-workload-fixture")
    {
        return Err("archive identity or extraction policy drifted".to_owned());
    }
    let [SourceResolution::Archive(source)] = lock.sources.as_slice() else {
        return Err("source lock must retain one archive entry".to_owned());
    };
    if source.order != 0 || source.url != PGO_WORKLOAD_ARCHIVE_URL || source.sha256 != PGO_WORKLOAD_ARCHIVE_SHA256 {
        return Err("source lock identity drifted".to_owned());
    }
    Ok(())
}

fn validate_pgo_workload_source(source: &str) -> Result<(), String> {
    for fragment in [
        "cast PGO workload fixture: profile-use binary executed",
        "cast PGO workload fixture: instrumented training completed",
        "for (uint32_t round = 0; round < 16384U; ++round)",
        "if ((round & 7U) == 0U)",
        "strcmp(argv[1], \"--train\") == 0",
        "strcmp(argv[1], \"--self-test\") == 0",
        "score_text(\"abc\") != 6U",
        "score_text(\"profile\") != 81U",
    ] {
        if source.matches(fragment).count() != 1 {
            return Err(format!("C workload must contain exactly one {fragment:?}"));
        }
    }
    Ok(())
}

fn assert_pgo_workload_archive_matches_tracked_sources(source_tree: &Path, published: &Path) {
    assert_eq!(
        fs::read(published.join("main.c")).unwrap(),
        fs::read(source_tree.join("main.c")).unwrap(),
        "pgo-workload: locked archive contains stale main.c bytes"
    );
}

#[test]
fn pgo_workload_declaration_and_training_source_fail_closed() {
    let package_root = execution_fixture_package_directory("pgo-workload");
    let recipe = crate::Recipe::load_authored(package_root.join("stone.glu")).unwrap();
    let lock = decode_source_lock(
        SOURCE_LOCK_FILE_NAME,
        &fs::read(package_root.join(SOURCE_LOCK_FILE_NAME)).unwrap(),
    )
    .unwrap();
    let source = fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/gluon/execution/source-trees/cast-pgo-workload-fixture-1.0.0/main.c"),
    )
    .unwrap();
    validate_pgo_workload_recipe(&recipe.declaration, &lock).unwrap();
    validate_pgo_workload_source(&source).unwrap();

    let mut no_training = recipe.declaration.clone();
    no_training.builder.phases.workload.steps.clear();
    assert!(validate_pgo_workload_recipe(&no_training, &lock).is_err());
    let mut accidental_stage_two = recipe.declaration.clone();
    accidental_stage_two.options.cspgo = true;
    assert!(validate_pgo_workload_recipe(&accidental_stage_two, &lock).is_err());
    let missing_training = source.replacen(
        "cast PGO workload fixture: instrumented training completed",
        "training omitted",
        1,
    );
    assert!(validate_pgo_workload_source(&missing_training).is_err());
}
