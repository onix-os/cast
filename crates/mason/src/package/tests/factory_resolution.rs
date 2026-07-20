#[test]
fn package_factory_defaults_resolve_directly() {
    let recipe =
        Recipe::load(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/examples/gluon/stone.glu")).unwrap();
    let install = tempfile::tempdir().unwrap();
    let mut collector = Collector::new(install.path());

    let packages = resolve_packages(&recipe, &mut collector).unwrap();

    // Golden split policy is now returned as typed values by mk_package.
    assert_eq!(
        packages.keys().map(String::as_str).collect::<Vec<_>>(),
        [
            "hello",
            "hello-32bit",
            "hello-32bit-dbginfo",
            "hello-32bit-devel",
            "hello-dbginfo",
            "hello-demos",
            "hello-devel",
            "hello-docs",
            "hello-libs",
        ]
    );
    let rules = collector.rules();
    assert_eq!(
        rules.last().map(|rule| (rule.package(), rule.pattern())),
        Some(("hello-demos", "/usr/lib/qt*/examples"))
    );
    assert_ne!(
        rules.last().map(|rule| rule.package()),
        packages.keys().last().map(String::as_str),
        "collector precedence must retain composition order rather than package-map order"
    );

    let root = &packages["hello"];
    assert_eq!(root.summary.as_deref(), Some("Minimal Gluon recipe example"));
    assert!(root.include_in_manifest);
    assert_eq!(
        rules
            .iter()
            .filter(|rule| rule.package() == "hello")
            .map(|rule| (rule.kind(), rule.pattern()))
            .collect::<Vec<_>>(),
        [(PathRuleKind::Any, "*")]
    );
    assert!(!packages["hello-dbginfo"].include_in_manifest);
    assert!(!packages["hello-32bit-dbginfo"].include_in_manifest);

    let devel = &packages["hello-devel"];
    assert_eq!(devel.summary.as_deref(), Some("Development files for hello"));
    assert_eq!(
        devel.description.as_deref(),
        Some("Install this package if you intend to build software against\nthe hello package.")
    );
    assert_eq!(
        devel.runtime_inputs.iter().map(Dependency::to_name).collect::<Vec<_>>(),
        ["hello"]
    );
    assert_eq!(
        rules
            .iter()
            .filter(|rule| rule.package() == "hello-devel")
            .map(|rule| rule.pattern())
            .collect::<Vec<_>>(),
        [
            "/usr/include",
            "/usr/lib/*.a",
            "/usr/lib/cmake",
            "/usr/lib/lib*.so",
            "/usr/lib/pkgconfig",
            "/usr/share/aclocal",
            "/usr/share/cmake",
            "/usr/share/man/man2",
            "/usr/share/man/man3",
            "/usr/share/man/man9",
            "/usr/share/pkgconfig",
            "/usr/share/gir-1.0/*.gir",
            "/usr/share/vala/vapi/*.deps",
            "/usr/share/vala/vapi/*.vapi",
            "/usr/lib/*.prl",
            "/usr/lib/metatypes",
            "/usr/lib/qt*/metatypes/qt*.json",
            "/usr/lib/qt*/mkspecs",
            "/usr/lib/qt*/modules/*.json",
            "/usr/lib/qt*/sbom",
            "/usr/lib/qt*/plugins/designer/*.so",
            "/usr/share/doc/qt5/*.qch",
            "/usr/share/doc/qt5/*.tags",
            "/usr/share/doc/qt6/*.qch",
            "/usr/share/doc/qt6/*.tags",
        ]
    );
}

#[test]
fn resolved_outputs_do_not_inherit_root_metadata() {
    let mut recipe =
        Recipe::load(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/examples/gluon/stone.glu")).unwrap();
    let root = recipe
        .declaration
        .outputs
        .iter_mut()
        .find(|output| output.name == "out")
        .unwrap();
    root.summary = Some("Root summary only".to_owned());
    root.description = Some("Root description only".to_owned());
    let split = recipe
        .declaration
        .outputs
        .iter_mut()
        .find(|output| output.name == "libs")
        .unwrap();
    split.summary = None;
    split.description = None;

    let install = tempfile::tempdir().unwrap();
    let mut collector = Collector::new(install.path());
    let packages = resolve_packages(&recipe, &mut collector).unwrap();

    assert_eq!(packages["hello"].summary.as_deref(), Some("Root summary only"));
    assert_eq!(packages["hello"].description.as_deref(), Some("Root description only"));
    assert_eq!(packages["hello-libs"].summary, None);
    assert_eq!(packages["hello-libs"].description, None);
}

#[test]
fn frozen_packager_uses_only_plan_outputs_rules_analysis_and_identity() {
    let recipe =
        Recipe::load(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/examples/gluon/stone.glu")).unwrap();
    let runtime = crate::private_tempdir();
    let output = tempfile::tempdir().unwrap();
    let mut plan = test_derivation_plan();
    plan.package.name = "frozen".to_owned();
    plan.package.homepage = "https://frozen.invalid".to_owned();
    plan.package.architecture = "x86".to_owned();
    plan.build_lock.target_platform.architecture = "x86".to_owned();
    plan.package.licenses = vec!["MIT".to_owned()];
    plan.analysis = AnalysisPlan {
        handlers: vec![
            AnalyzerKind::IgnoreBlocked,
            AnalyzerKind::Binary,
            AnalyzerKind::Elf,
            AnalyzerKind::PkgConfig,
            AnalyzerKind::Python,
            AnalyzerKind::CMake,
            AnalyzerKind::CompressMan,
            AnalyzerKind::IncludeAny,
        ],
        tools: AnalysisToolsPlan {
            pkg_config: Some(frozen_analyzer_tool("pkg-config")),
            python: Some(frozen_analyzer_tool("python3")),
            objcopy: Some(frozen_analyzer_tool("objcopy")),
            strip: None,
        },
        debug: true,
        strip: false,
        compress_man: false,
        remove_libtool: false,
    };
    plan.manifest_build_inputs = vec![RelationPlan {
        kind: RelationKind::Binary,
        name: "frozen-build-input".to_owned(),
    }];
    plan.outputs = vec![OutputPlan {
        name: "out".to_owned(),
        package_name: "frozen".to_owned(),
        include_in_manifest: true,
        summary: Some("Frozen output".to_owned()),
        description: Some("Only plan data".to_owned()),
        provides_exclude: vec!["excluded-provider".to_owned()],
        runtime_exclude: vec!["excluded-runtime".to_owned()],
        runtime_inputs: Vec::new(),
        conflicts: vec![RelationPlan {
            kind: RelationKind::PkgConfig,
            name: "conflict".to_owned(),
        }],
    }];
    plan.collection_rules = vec![
        CollectionRulePlan {
            output: "out".to_owned(),
            kind: PathRuleKind::Any,
            pattern: "*".to_owned(),
        },
        CollectionRulePlan {
            output: "out".to_owned(),
            kind: PathRuleKind::Executable,
            pattern: "/usr/bin/*".to_owned(),
        },
    ];
    plan.validate().unwrap();
    let expected_id = plan.derivation_id();
    let mut paths = Paths::new(&recipe, plan.layout.clone(), runtime.path(), output.path()).unwrap();
    paths.bind_to_plan(&plan).unwrap();

    let packager = FrozenPackager::from_plan(&paths, &plan).unwrap();
    assert_eq!(packager.install_root, Path::new(&plan.layout.install_dir));
    assert_eq!(packager.artifact_root, Path::new(&plan.layout.artifacts_dir));
    assert_eq!(packager.identity.name, "frozen");
    assert_eq!(packager.identity.homepage, "https://frozen.invalid");
    assert_eq!(packager.architecture, crate::Architecture::X86);
    assert_eq!(packager.analysis, plan.analysis);
    assert_eq!(packager.recipe_fingerprint, plan.provenance.recipe.sha256);
    assert_eq!(
        packager
            .manifest_build_inputs
            .iter()
            .map(Dependency::to_name)
            .collect::<Vec<_>>(),
        ["binary(frozen-build-input)"]
    );
    assert_eq!(packager.derivation_id, expected_id);
    assert_eq!(
        packager
            .collector
            .rules()
            .iter()
            .map(|rule| (rule.package(), rule.kind(), rule.pattern()))
            .collect::<Vec<_>>(),
        [
            ("frozen", PathRuleKind::Any, "*"),
            ("frozen", PathRuleKind::Executable, "/usr/bin/*"),
        ]
    );
    let output = &packager.packages["frozen"];
    assert_eq!(output.summary.as_deref(), Some("Frozen output"));
    assert_eq!(
        output.conflicts.iter().map(Provider::to_name).collect::<Vec<_>>(),
        ["pkgconfig(conflict)"]
    );
}

#[test]
fn frozen_packager_rejects_runtime_and_plan_layout_mismatch() {
    let recipe =
        Recipe::load(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/examples/gluon/stone.glu")).unwrap();
    let runtime = crate::private_tempdir();
    let output = tempfile::tempdir().unwrap();
    let mut plan = test_derivation_plan();
    let paths = Paths::new(&recipe, plan.layout.clone(), runtime.path(), output.path()).unwrap();
    plan.layout.hostname = "different-builder".to_owned();
    plan.validate().unwrap();

    assert!(matches!(
        FrozenPackager::from_plan(&paths, &plan),
        Err(Error::FrozenLayoutMismatch)
    ));
}
