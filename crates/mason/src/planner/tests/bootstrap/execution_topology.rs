const REQUIRED_EXECUTION_FIXTURES: [&str; 26] = [
    "autotools",
    "autotools-options",
    "cargo",
    "cargo-features",
    "cargo-vendored",
    "cmake",
    "custom",
    "daemon-generated",
    "desktop-integration",
    "external-test-vectors",
    "factory-override",
    "font-family",
    "generated-config",
    "generated-shell",
    "gettext-localization",
    "go-module",
    "header-only-library",
    "hooks-patch",
    "meson",
    "multiple-sources",
    "plugin-output",
    "post-install-smoke-test",
    "python-module",
    "split",
    "system-integration-assets",
    "userspace-profile",
];
const EXECUTION_FIXTURE_SELECTOR_ENV: &str = "CAST_EXECUTION_FIXTURE";

mod multiple_sources_topology {
    use super::*;

    include!("execution_topology/multiple_sources.rs");
}

mod desktop_integration_topology {
    use super::*;

    include!("execution_topology/desktop_integration.rs");
}

mod external_test_vectors_topology {
    use super::*;

    include!("execution_topology/external_test_vectors.rs");
}

mod font_family_topology {
    use super::*;

    include!("execution_topology/font_family.rs");
}

mod system_integration_assets_topology {
    use super::*;

    include!("execution_topology/system_integration_assets.rs");
}

mod gettext_localization_topology {
    use super::*;

    include!("execution_topology/gettext_localization.rs");
}

mod go_module_topology {
    use super::*;

    include!("execution_topology/go_module.rs");
}

mod python_module_topology {
    use super::*;

    include!("execution_topology/python_module.rs");
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ExecutionFixtureSelection {
    All,
    One(&'static str),
}

impl ExecutionFixtureSelection {
    fn includes(self, fixture: &str) -> bool {
        match self {
            Self::All => true,
            Self::One(selected) => selected == fixture,
        }
    }

    fn expected_count(self) -> usize {
        match self {
            Self::All => REQUIRED_EXECUTION_FIXTURES.len(),
            Self::One(_) => 1,
        }
    }
}

fn parse_execution_fixture_selection(value: Option<&str>) -> Result<ExecutionFixtureSelection, String> {
    let value = value.unwrap_or("all");
    if value == "all" {
        return Ok(ExecutionFixtureSelection::All);
    }
    if let Some(fixture) = REQUIRED_EXECUTION_FIXTURES
        .iter()
        .copied()
        .find(|fixture| *fixture == value)
    {
        return Ok(ExecutionFixtureSelection::One(fixture));
    }
    Err(format!(
        "{EXECUTION_FIXTURE_SELECTOR_ENV} must be `all` or exactly one of {}; got {value:?}",
        REQUIRED_EXECUTION_FIXTURES.join(", ")
    ))
}

fn execution_fixture_selection_from_env() -> Result<ExecutionFixtureSelection, String> {
    let Some(value) = std::env::var_os(EXECUTION_FIXTURE_SELECTOR_ENV) else {
        return parse_execution_fixture_selection(None);
    };
    let value = value.to_str().ok_or_else(|| {
        format!("{EXECUTION_FIXTURE_SELECTOR_ENV} must contain valid UTF-8 and name exactly one fixture or `all`")
    })?;
    parse_execution_fixture_selection(Some(value))
}

#[derive(Debug, PartialEq, Eq)]
enum FrozenStepShape {
    Run {
        program: String,
        first_argument: Option<String>,
    },
    RunBuilt {
        program: String,
        first_argument: Option<String>,
    },
    Shell {
        interpreter: String,
        declared_programs: Vec<String>,
        script: String,
    },
    ExtractArchive {
        source: u32,
        destination: String,
        strip_components: u32,
    },
}

#[derive(Debug, PartialEq, Eq)]
struct FrozenPhaseShape {
    name: String,
    pre: Vec<FrozenStepShape>,
    steps: Vec<FrozenStepShape>,
    post: Vec<FrozenStepShape>,
}

fn step_shape(step: &stone_recipe::derivation::StepPlan) -> FrozenStepShape {
    match step {
        stone_recipe::derivation::StepPlan::Run { program, args, .. } => FrozenStepShape::Run {
            program: program.path.clone(),
            first_argument: args.first().cloned(),
        },
        stone_recipe::derivation::StepPlan::RunBuilt { program, args, .. } => FrozenStepShape::RunBuilt {
            program: program.clone(),
            first_argument: args.first().cloned(),
        },
        stone_recipe::derivation::StepPlan::Shell {
            interpreter,
            declared_programs,
            script,
            ..
        } => FrozenStepShape::Shell {
            interpreter: interpreter.path.clone(),
            declared_programs: declared_programs.iter().map(|program| program.path.clone()).collect(),
            script: script.clone(),
        },
        stone_recipe::derivation::StepPlan::ExtractArchive {
            source,
            destination,
            strip_components,
        } => FrozenStepShape::ExtractArchive {
            source: *source,
            destination: destination.clone(),
            strip_components: *strip_components,
        },
    }
}

fn extract(destination: &str) -> FrozenStepShape {
    FrozenStepShape::ExtractArchive {
        source: 0,
        destination: destination.to_owned(),
        strip_components: 1,
    }
}

fn phase(name: &str, steps: Vec<FrozenStepShape>) -> FrozenPhaseShape {
    FrozenPhaseShape {
        name: name.to_owned(),
        pre: Vec::new(),
        steps,
        post: Vec::new(),
    }
}

fn phase_with_pre(name: &str, pre: Vec<FrozenStepShape>, steps: Vec<FrozenStepShape>) -> FrozenPhaseShape {
    FrozenPhaseShape {
        name: name.to_owned(),
        pre,
        steps,
        post: Vec::new(),
    }
}

fn phase_with_post(name: &str, steps: Vec<FrozenStepShape>, post: Vec<FrozenStepShape>) -> FrozenPhaseShape {
    FrozenPhaseShape {
        name: name.to_owned(),
        pre: Vec::new(),
        steps,
        post,
    }
}

fn run(program: &str, first_argument: &str) -> FrozenStepShape {
    FrozenStepShape::Run {
        program: format!("/usr/bin/{program}"),
        first_argument: Some(first_argument.to_owned()),
    }
}

fn assert_userspace_profile_relations(plan: &stone_recipe::derivation::DerivationPlan) {
    const EXACT_RUNTIME: [(&str, &str); 5] = [
        (
            "bash",
            "20a6cfc76001152c45a7f77f1ee50bfdb816d0b67408cd6857f023022f37f0d9",
        ),
        (
            "uutils-coreutils",
            "1a3f33a18144f93019f9572be47ce56ec60b79707a8e0678df0acbc98699a9cf",
        ),
        (
            "findutils",
            "154290fa77e4195e01500d586386b40b54dd818d964a8716b2aacb772609db6c",
        ),
        (
            "ca-certificates",
            "d0e58fc88b5d2ce74ca0d9d15effd521964da4a40108881f7a02ce7b02429c62",
        ),
        (
            "xz",
            "77f24568486ea39af22b71ed668653debaa9ddff4188626c61a626abd8b663ed",
        ),
    ];

    assert!(plan.sources.is_empty(), "userspace-profile: frozen sources must be empty");
    let [output] = plan.outputs.as_slice() else {
        panic!("userspace-profile: frozen plan must have exactly one output");
    };
    assert_eq!(output.name, "out");
    assert_eq!(output.package_name, "cast-userspace-profile-fixture");
    let actual = output
        .runtime_inputs
        .iter()
        .map(|relation| match relation {
                stone_recipe::derivation::OutputRelation::Locked {
                    relation,
                    reference,
                } => (relation.canonical_name(), reference.package_id.clone()),
                stone_recipe::derivation::OutputRelation::Planned { output } => {
                    panic!("userspace-profile: runtime relation unexpectedly targets local output {output}")
                }
            })
        .collect::<Vec<_>>();
    let expected = EXACT_RUNTIME
        .into_iter()
        .map(|(name, package_id)| (name.to_owned(), package_id.to_owned()))
        .collect::<Vec<_>>();
    assert_eq!(actual, expected);
}

fn assert_plugin_output_relations(plan: &stone_recipe::derivation::DerivationPlan) {
    let outputs = plan
        .outputs
        .iter()
        .map(|output| (output.name.as_str(), output))
        .collect::<BTreeMap<_, _>>();
    assert_eq!(outputs.keys().copied().collect::<Vec<_>>(), ["dbginfo", "out", "plugins"]);
    assert!(outputs["out"].include_in_manifest);
    assert!(outputs["plugins"].include_in_manifest);
    assert!(!outputs["dbginfo"].include_in_manifest);
    assert!(matches!(
        outputs["out"].runtime_inputs.as_slice(),
        [stone_recipe::derivation::OutputRelation::Planned { output }] if output == "plugins"
    ));
    assert!(outputs["plugins"].runtime_inputs.is_empty());
    assert!(outputs["dbginfo"].runtime_inputs.is_empty());
}

fn assert_generated_shell_relations(plan: &stone_recipe::derivation::DerivationPlan) {
    const BASH_PACKAGE_ID: &str = "20a6cfc76001152c45a7f77f1ee50bfdb816d0b67408cd6857f023022f37f0d9";

    assert!(plan.sources.is_empty(), "generated-shell: frozen sources must be empty");
    let [output] = plan.outputs.as_slice() else {
        panic!("generated-shell: frozen plan must have exactly one output");
    };
    assert_eq!(output.name, "out");
    assert_eq!(output.package_name, "cast-generated-shell-fixture");
    assert!(output.include_in_manifest);
    let [stone_recipe::derivation::OutputRelation::Locked {
        relation,
        reference,
    }] = output.runtime_inputs.as_slice()
    else {
        panic!("generated-shell: runtime must be exactly one locked Bash relation");
    };
    assert_eq!(relation.canonical_name(), "binary(bash)");
    assert_eq!(reference.package_id, BASH_PACKAGE_ID);
}

fn assert_cmake_zlib_relations(plan: &stone_recipe::derivation::DerivationPlan) {
    assert_eq!(
        plan.manifest_build_inputs
            .iter()
            .map(|relation| relation.canonical_name())
            .collect::<Vec<_>>(),
        ["binary(sh)", "binary(ninja)", "cmake(zlib)"],
        "cmake: manifest BuildDepends inputs drifted"
    );

    let requests = plan
        .build_lock
        .requests
        .iter()
        .filter(|request| request.request == "cmake(zlib)")
        .collect::<Vec<_>>();
    let [request] = requests.as_slice() else {
        panic!("cmake: build lock must contain exactly one cmake(zlib) request");
    };
    assert_eq!(request.package_id, ZLIB_DEVEL_PACKAGE_ID);
    assert_eq!(request.output, "out");
    assert_eq!(
        request.origins,
        [stone_recipe::derivation::InputOrigin::Build {
            selection: stone_recipe::derivation::PackageInputSelection::Package,
            index: 0,
        }],
        "cmake: zlib provider origin drifted"
    );

    let zlib_devel = plan
        .build_lock
        .packages
        .iter()
        .find(|package| package.package_id == ZLIB_DEVEL_PACKAGE_ID)
        .expect("cmake: zlib-devel provider package is absent");
    assert_eq!(zlib_devel.name, "zlib-devel");
    assert_eq!(zlib_devel.version, "2.3.3-23-1");
    assert_eq!(zlib_devel.architecture, "x86_64");
    assert_eq!(zlib_devel.repository, "bootstrap");
    assert_eq!(
        zlib_devel.outputs.iter().map(|output| output.name.as_str()).collect::<Vec<_>>(),
        ["out"]
    );
    assert_eq!(
        zlib_devel
            .dependencies
            .iter()
            .map(|dependency| (dependency.package_id.as_str(), dependency.output.as_str()))
            .collect::<Vec<_>>(),
        [(ZLIB_RUNTIME_PACKAGE_ID, "out")],
        "cmake: zlib-devel runtime edge drifted"
    );
    let zlib = plan
        .build_lock
        .packages
        .iter()
        .find(|package| package.package_id == ZLIB_RUNTIME_PACKAGE_ID)
        .expect("cmake: zlib runtime package is absent");
    assert_eq!(zlib.name, "zlib");
    assert_eq!(zlib.version, "2.3.3-23-1");
    assert_eq!(zlib.architecture, "x86_64");
    assert_eq!(zlib.repository, "bootstrap");
    assert_eq!(
        zlib.outputs.iter().map(|output| output.name.as_str()).collect::<Vec<_>>(),
        ["out"]
    );
}

fn assert_ninja_builder_tool_relations(
    name: &str,
    plan: &stone_recipe::derivation::DerivationPlan,
    shell_index: u32,
    ninja_index: u32,
) {
    let request = |requirement: &str| {
        let matches = plan
            .build_lock
            .requests
            .iter()
            .filter(|request| request.request == requirement)
            .collect::<Vec<_>>();
        let [request] = matches.as_slice() else {
            panic!("{name}: build lock must contain exactly one {requirement} request");
        };
        *request
    };
    for (requirement, package_id, package_name, version, index) in [
        (
            "binary(sh)",
            NINJA_SHELL_DASH_PACKAGE_ID,
            "dash",
            "0.5.13.4-19-1",
            shell_index,
        ),
        (
            "binary(ninja)",
            NINJA_PACKAGE_ID,
            "ninja",
            "1.13.2-6-1",
            ninja_index,
        ),
    ] {
        let locked = request(requirement);
        assert_eq!(locked.package_id, package_id, "{name}: {requirement} provider drifted");
        assert_eq!(locked.output, "out");
        assert_eq!(
            locked.origins,
            [stone_recipe::derivation::InputOrigin::BuilderTool {
                selection: stone_recipe::derivation::PackageInputSelection::Package,
                index,
            }],
            "{name}: {requirement} must remain an exact BuilderTool input"
        );
        let provider = plan
            .build_lock
            .packages
            .iter()
            .find(|package| package.package_id == package_id)
            .unwrap_or_else(|| panic!("{name}: {requirement} provider package is absent"));
        assert_eq!(provider.name, package_name);
        assert_eq!(provider.version, version);
        assert_eq!(provider.architecture, "x86_64");
        assert_eq!(provider.repository, "bootstrap");
        assert_eq!(
            provider.outputs.iter().map(|output| output.name.as_str()).collect::<Vec<_>>(),
            ["out"]
        );
    }
}

fn assert_meson_dependency_role_relations(plan: &stone_recipe::derivation::DerivationPlan) {
    assert_eq!(
        plan.manifest_build_inputs
            .iter()
            .map(|relation| relation.canonical_name())
            .collect::<Vec<_>>(),
        [
            "binary(cmake)",
            "binary(sh)",
            "binary(ninja)",
            "binary(pkgconf)",
            "pkgconfig(zlib)",
            "binary(file)",
        ],
        "meson: manifest BuildDepends inputs drifted"
    );

    let request = |name: &str| {
        let matches = plan.build_lock.requests.iter().filter(|request| request.request == name).collect::<Vec<_>>();
        let [request] = matches.as_slice() else {
            panic!("meson: build lock must contain exactly one {name} request");
        };
        *request
    };
    let zlib_request = request("pkgconfig(zlib)");
    assert_eq!(zlib_request.package_id, ZLIB_DEVEL_PACKAGE_ID);
    assert_eq!(zlib_request.output, "out");
    assert_eq!(
        zlib_request.origins,
        [stone_recipe::derivation::InputOrigin::Build {
            selection: stone_recipe::derivation::PackageInputSelection::Package,
            index: 0,
        }],
        "meson: zlib must retain only its target-build origin"
    );

    let file_request = request("binary(file)");
    assert_eq!(file_request.package_id, FILE_PACKAGE_ID);
    assert_eq!(file_request.output, "out");
    assert_eq!(
        file_request.origins,
        [stone_recipe::derivation::InputOrigin::Check {
            selection: stone_recipe::derivation::PackageInputSelection::Package,
            index: 0,
        }],
        "meson: file must retain only its check-input origin"
    );

    let package = |id: &str| {
        plan.build_lock
            .packages
            .iter()
            .find(|package| package.package_id == id)
            .unwrap_or_else(|| panic!("meson: locked package {id} is absent"))
    };
    let zlib_devel = package(ZLIB_DEVEL_PACKAGE_ID);
    assert_eq!((zlib_devel.name.as_str(), zlib_devel.version.as_str()), ("zlib-devel", "2.3.3-23-1"));
    assert_eq!(
        zlib_devel
            .dependencies
            .iter()
            .map(|dependency| (dependency.package_id.as_str(), dependency.output.as_str()))
            .collect::<Vec<_>>(),
        [(ZLIB_RUNTIME_PACKAGE_ID, "out")]
    );

    let file = package(FILE_PACKAGE_ID);
    assert_eq!((file.name.as_str(), file.version.as_str()), ("file", "5.48-12-1"));
    assert_eq!(file.architecture, "x86_64");
    assert_eq!(file.repository, "bootstrap");
    assert_eq!(file.outputs.iter().map(|output| output.name.as_str()).collect::<Vec<_>>(), ["out"]);
    assert_eq!(
        file.dependencies
            .iter()
            .map(|dependency| (dependency.package_id.as_str(), dependency.output.as_str()))
            .collect::<BTreeSet<_>>(),
        BTreeSet::from([
            ("3fec7dd3f1f8d01c674ae2182f427de44864dc205d5014ec6c8dc2e3e0327875", "out"),
            ("72f68a72d866271aa2f3db09dd636aed30faedf8ddc92f1c73b6ba0a24f29da8", "out"),
            ("77f24568486ea39af22b71ed668653debaa9ddff4188626c61a626abd8b663ed", "out"),
            ("8db61cf368b0425c5eec49273dec58bf99894fd10b2987bfeda7214ef3cbb43e", "out"),
            ("ae5d2ec54e5776dfdae0b5b1b54fd00308031b197644380926b7cb7422b13e9e", "out"),
            ("e8b9d5cee1c7500c87de37c6389de695d879e27b37ea402e1cad1efd88bd3c63", "out"),
        ]),
        "meson: file runtime closure drifted"
    );

    let libseccomp = package(LIBSECCOMP_PACKAGE_ID);
    assert_eq!((libseccomp.name.as_str(), libseccomp.version.as_str()), ("libseccomp", "2.6.1-7-1"));
    assert_eq!(
        libseccomp
            .dependencies
            .iter()
            .map(|dependency| (dependency.package_id.as_str(), dependency.output.as_str()))
            .collect::<Vec<_>>(),
        [("ae5d2ec54e5776dfdae0b5b1b54fd00308031b197644380926b7cb7422b13e9e", "out")]
    );
}

fn assert_execution_fixture_topology(name: &str, plan: &stone_recipe::derivation::DerivationPlan) {
    assert_eq!(EXECUTION_FIXTURES, REQUIRED_EXECUTION_FIXTURES);
    assert_eq!(plan.execution.jobs, 1, "{name}: execution preflight jobs drifted");
    assert_eq!(
        plan.execution.filesystems,
        stone_recipe::derivation::FilesystemPolicy::default(),
        "{name}: execution preflight filesystem policy drifted"
    );
    let [job] = plan.jobs.as_slice() else {
        panic!("{name}: execution fixture must freeze exactly one non-PGO job");
    };
    assert_eq!(job.pgo_stage, None, "{name}: unexpected PGO stage");
    assert_eq!(job.pgo_dir, None, "{name}: unexpected PGO directory");

    let prepare = |destination: &str| phase("Prepare", vec![extract(destination)]);
    let run_built = |program: &str, first_argument: &str| FrozenStepShape::RunBuilt {
        program: Path::new(&job.work_dir).join(program).display().to_string(),
        first_argument: Some(first_argument.to_owned()),
    };
    let expected = match name {
        "cmake" | "factory-override" => vec![
            prepare(if name == "cmake" {
                "cast-cmake-fixture"
            } else {
                "cast-factory-override-fixture"
            }),
            phase("Setup", vec![run("cmake", "-G")]),
            phase("Build", vec![run("cmake", "--build")]),
            phase("Install", vec![run("cmake", "--install")]),
            phase("Check", vec![run("ctest", "--test-dir")]),
        ],
        "split" => vec![
            prepare("cast-split-fixture"),
            phase("Setup", vec![run("cmake", "-G")]),
            phase("Build", vec![run("cmake", "--build")]),
            phase("Install", vec![run("cmake", "--install")]),
            phase("Check", vec![run("ctest", "--test-dir")]),
        ],
        "meson" => vec![
            prepare("cast-meson-fixture"),
            phase("Setup", vec![run("meson", "setup")]),
            phase("Build", vec![run("meson", "compile")]),
            phase("Install", vec![run("meson", "install")]),
            phase("Check", vec![run("meson", "test")]),
        ],
        "multiple-sources" => multiple_sources_topology::expected(),
        "desktop-integration" => desktop_integration_topology::expected(),
        "external-test-vectors" => external_test_vectors_topology::expected(),
        "font-family" => font_family_topology::expected(),
        "gettext-localization" => gettext_localization_topology::expected(&job.work_dir),
        "go-module" => go_module_topology::expected(&job.work_dir),
        "python-module" => python_module_topology::expected(),
        "system-integration-assets" => system_integration_assets_topology::expected(),
        "cargo" | "cargo-features" | "cargo-vendored" => vec![
            prepare(match name {
                "cargo" => "cast-cargo-fixture",
                "cargo-features" => "cast-cargo-features-fixture",
                "cargo-vendored" => "cast-cargo-vendored-fixture",
                _ => unreachable!(),
            }),
            phase("Build", vec![run("cargo", "build")]),
            phase("Install", vec![run("install", "-Dm00755")]),
            phase("Check", vec![run("cargo", "test")]),
        ],
        "autotools" => vec![
            prepare("cast-autotools-fixture"),
            phase_with_pre(
                "Setup",
                vec![run("autoreconf", "-fi")],
                vec![run("dash", "./configure")],
            ),
            phase("Build", vec![run("make", "VERBOSE=1")]),
            phase("Install", vec![run("make", "install")]),
            phase("Check", vec![run("make", "check")]),
        ],
        "autotools-options" => vec![
            prepare("cast-autotools-options-fixture"),
            phase("Setup", vec![run("dash", "./configure")]),
            phase("Build", vec![run("make", "VERBOSE=1")]),
            phase("Install", vec![run("make", "install")]),
        ],
        "custom" => vec![
            prepare("cast-custom-fixture"),
            phase("Setup", vec![run("mkdir", "-p")]),
            phase("Build", vec![run("cc", "-O2")]),
            phase(
                "Install",
                vec![FrozenStepShape::Shell {
                    interpreter: "/usr/bin/dash".to_owned(),
                    declared_programs: vec!["/usr/bin/install".to_owned()],
                    script: r#"install -Dm755 build/cast-custom-fixture "${CAST_INSTALL_ROOT}${CAST_BINDIR}/cast-custom-fixture""#
                        .to_owned(),
                }],
            ),
            phase(
                "Check",
                vec![run_built("build/cast-custom-fixture", "--self-test")],
            ),
        ],
        "header-only-library" => vec![
            prepare("cast-header-only-library-fixture"),
            phase(
                "Install",
                vec![FrozenStepShape::Shell {
                    interpreter: "/usr/bin/dash".to_owned(),
                    declared_programs: vec!["/usr/bin/install".to_owned()],
                    script: HEADER_ONLY_INSTALL_SCRIPT.to_owned(),
                }],
            ),
            phase(
                "Check",
                vec![FrozenStepShape::Shell {
                    interpreter: "/usr/bin/dash".to_owned(),
                    declared_programs: vec!["/usr/bin/cc".to_owned()],
                    script: HEADER_ONLY_CHECK_SCRIPT.to_owned(),
                }],
            ),
        ],
        "daemon-generated" => vec![
            prepare("cast-daemon-fixture"),
            phase("Setup", vec![run("cmake", "-G")]),
            phase("Build", vec![run("cmake", "--build")]),
            phase("Install", vec![run("cmake", "--install")]),
            phase("Check", vec![run("ctest", "--test-dir")]),
        ],
        "post-install-smoke-test" => vec![
            prepare("cast-post-install-smoke-test-fixture"),
            phase("Setup", vec![run("cmake", "-G")]),
            phase("Build", vec![run("cmake", "--build")]),
            phase_with_post(
                "Install",
                vec![run("cmake", "--install")],
                vec![FrozenStepShape::Shell {
                    interpreter: "/usr/bin/bash".to_owned(),
                    declared_programs: Vec::new(),
                    script: r#""${CAST_INSTALL_ROOT}${CAST_BINDIR}/staged-probe" --self-test"#.to_owned(),
                }],
            ),
            phase("Check", vec![run("ctest", "--test-dir")]),
        ],
        "plugin-output" => vec![
            prepare("cast-plugin-output-fixture"),
            phase("Setup", vec![run("mkdir", "-p")]),
            phase("Build", vec![run("cc", "-std=c11"), run("cc", "-std=c11")]),
            phase(
                "Install",
                vec![FrozenStepShape::Shell {
                    interpreter: "/usr/bin/dash".to_owned(),
                    declared_programs: vec!["/usr/bin/install".to_owned()],
                    script: r#"
install -Dm755 build/cast-plugin-host \
    "${CAST_INSTALL_ROOT}${CAST_BINDIR}/cast-plugin-host"
install -Dm644 build/cast-plugin-output.so \
    "${CAST_INSTALL_ROOT}${CAST_LIBDIR}/cast/plugins/cast-plugin-output.so"
"#
                    .to_owned(),
                }],
            ),
            phase(
                "Check",
                vec![run_built("build/cast-plugin-host", "--plugin")],
            ),
        ],
        "generated-config" => vec![phase(
            "Install",
            vec![FrozenStepShape::Shell {
                interpreter: "/usr/bin/bash".to_owned(),
                declared_programs: vec!["/usr/bin/install".to_owned()],
                script: r#"
printf '%s\n' \
    'format = 1' \
    'profile = "stone-native"' \
    'source = "gluon"' \
    > generated-config.conf
install -Dm644 generated-config.conf \
    "${CAST_INSTALL_ROOT}${CAST_DATADIR}/cast/generated-config.conf"
"#
                .to_owned(),
                }],
        )],
        "generated-shell" => vec![
            phase(
                "Build",
                vec![FrozenStepShape::Shell {
                    interpreter: "/usr/bin/bash".to_owned(),
                    declared_programs: Vec::new(),
                    script: r#"
printf '%s\n' \
    '#!/usr/bin/bash' \
    'set -euo pipefail' \
    '' \
    'if [[ "$#" -eq 0 ]]; then' \
    "    printf '%s\\n' 'cast-generated-shell'" \
    'elif [[ "$#" -eq 1 && "$1" == --self-test ]]; then' \
    "    printf '%s\\n' 'cast-generated-shell: self-test passed'" \
    'else' \
    "    printf '%s\\n' 'usage: cast-generated-shell [--self-test]' >&2" \
    '    exit 64' \
    'fi' \
    > cast-generated-shell
"#
                    .to_owned(),
                }],
            ),
            phase(
                "Install",
                vec![FrozenStepShape::Shell {
                    interpreter: "/usr/bin/bash".to_owned(),
                    declared_programs: vec!["/usr/bin/install".to_owned()],
                    script: r#"install -Dm755 cast-generated-shell "${CAST_INSTALL_ROOT}${CAST_BINDIR}/cast-generated-shell""#
                        .to_owned(),
                }],
            ),
            phase(
                "Check",
                vec![FrozenStepShape::Shell {
                    interpreter: "/usr/bin/bash".to_owned(),
                    declared_programs: Vec::new(),
                    script: r#"
actual="$(source ./cast-generated-shell --self-test)"
if [[ "${actual}" != 'cast-generated-shell: self-test passed' ]]; then
    printf '%s\n' 'generated shell self-test output differed' >&2
    exit 1
fi

set +e
(source ./cast-generated-shell --unexpected >/dev/null 2>&1)
status="$?"
set -e
if [[ "${status}" -ne 64 ]]; then
    printf 'unexpected-argument status was %s, expected 64\n' "${status}" >&2
    exit 1
fi
"#
                    .to_owned(),
                }],
            ),
        ],
        "hooks-patch" => vec![
            prepare("cast-hooks-fixture"),
            phase_with_pre(
                "Setup",
                vec![FrozenStepShape::Shell {
                    interpreter: "/usr/bin/bash".to_owned(),
                    declared_programs: vec!["/usr/bin/patch".to_owned()],
                    script: r#"patch -d "${CAST_SOURCE_DIR}/cast-hooks-fixture" -p1 -i "${CAST_SOURCE_DIR}/pre-setup.patch""#
                        .to_owned(),
                }],
                vec![run("cmake", "-G")],
            ),
            phase("Build", vec![run("cmake", "--build")]),
            phase("Install", vec![run("cmake", "--install")]),
            phase("Check", vec![run("ctest", "--test-dir")]),
        ],
        "userspace-profile" => Vec::new(),
        other => panic!("unexpected execution fixture {other:?}"),
    };

    let actual = job
        .phases
        .iter()
        .map(|phase| {
            assert!(!phase.steps.is_empty(), "{name}/{}: empty frozen phase", phase.name);
            FrozenPhaseShape {
                name: phase.name.clone(),
                pre: phase.pre.iter().map(step_shape).collect(),
                steps: phase.steps.iter().map(step_shape).collect(),
                post: phase.post.iter().map(step_shape).collect(),
            }
        })
        .collect::<Vec<_>>();
    assert_eq!(actual, expected, "{name}: frozen builder phase topology drifted");
    if CMAKE_DERIVED_EXECUTION_FIXTURES.contains(&name) {
        assert_ninja_builder_tool_relations(name, plan, 0, 1);
    }
    if MESON_DERIVED_EXECUTION_FIXTURES.contains(&name) {
        assert_ninja_builder_tool_relations(name, plan, 1, 2);
    }
    if name == "userspace-profile" {
        assert_userspace_profile_relations(plan);
    }
    if name == "autotools" {
        assert_autotools_regeneration_relations(plan);
    }
    if name == "cmake" {
        assert_cmake_zlib_relations(plan);
    }
    if name == "meson" {
        assert_meson_dependency_role_relations(plan);
    }
    if name == "multiple-sources" {
        multiple_sources_topology::assert_contract(plan, job);
    }
    if name == "desktop-integration" {
        desktop_integration_topology::assert_contract(plan, job);
    }
    if name == "external-test-vectors" {
        external_test_vectors_topology::assert_contract(plan, job);
    }
    if name == "font-family" {
        font_family_topology::assert_contract(plan, job);
    }
    if name == "gettext-localization" {
        gettext_localization_topology::assert_contract(plan, job);
    }
    if name == "go-module" {
        go_module_topology::assert_contract(plan, job);
    }
    if name == "python-module" {
        python_module_topology::assert_contract(plan, job);
    }
    if name == "system-integration-assets" {
        system_integration_assets_topology::assert_contract(plan, job);
    }
    if name == "generated-shell" {
        assert_generated_shell_relations(plan);
    }
    if name == "plugin-output" {
        assert_plugin_output_relations(plan);
        let build = job.phases.iter().find(|phase| phase.name == "Build").unwrap();
        let [
            stone_recipe::derivation::StepPlan::Run {
                program: plugin_cc,
                args: plugin_args,
                ..
            },
            stone_recipe::derivation::StepPlan::Run {
                program: host_cc,
                args: host_args,
                ..
            },
        ] = build.steps.as_slice()
        else {
            panic!("plugin-output: frozen Build phase has unexpected steps");
        };
        assert_eq!(plugin_cc.path, "/usr/bin/cc");
        assert_eq!(host_cc.path, "/usr/bin/cc");
        assert_eq!(
            plugin_args.as_slice(),
            [
                "-std=c11",
                "-O2",
                "-g",
                "-Wall",
                "-Wextra",
                "-Werror",
                "-fstack-protector-strong",
                "-D_FORTIFY_SOURCE=3",
                "-fPIC",
                "-shared",
                "plugin.c",
                "-Wl,-soname,cast-plugin-output.so",
                "-Wl,--build-id=sha1",
                "-Wl,-z,relro,-z,now",
                "-Wl,-z,noexecstack",
                "-Wl,-z,separate-code",
                "-Wl,--no-undefined",
                "-o",
                "build/cast-plugin-output.so",
            ]
        );
        assert_eq!(
            host_args.as_slice(),
            [
                "-std=c11",
                "-O2",
                "-g",
                "-Wall",
                "-Wextra",
                "-Werror",
                "-fstack-protector-strong",
                "-D_FORTIFY_SOURCE=3",
                "-fPIE",
                "host.c",
                "-Wl,-pie",
                "-Wl,--build-id=sha1",
                "-Wl,-z,relro,-z,now",
                "-Wl,-z,noexecstack",
                "-Wl,-z,separate-code",
                "-Wl,--as-needed",
                "-ldl",
                "-o",
                "build/cast-plugin-host",
            ]
        );
        let check = job.phases.iter().find(|phase| phase.name == "Check").unwrap();
        let [stone_recipe::derivation::StepPlan::RunBuilt { args, .. }] = check.steps.as_slice() else {
            panic!("plugin-output: frozen Check phase has unexpected steps");
        };
        assert_eq!(args.as_slice(), ["--plugin", "build/cast-plugin-output.so"]);
    }
    if name == "autotools-options" {
        let setup = job.phases.iter().find(|phase| phase.name == "Setup").unwrap();
        let [stone_recipe::derivation::StepPlan::Run { args, .. }] = setup.steps.as_slice() else {
            panic!("autotools-options: frozen Setup phase has unexpected steps");
        };
        assert_eq!(args.last().map(String::as_str), Some("--enable-stone-message"));
        assert!(
            job.phases.iter().all(|phase| phase.name != "Check"),
            "autotools-options: run_tests=false retained a Check phase"
        );
    }
    if name == "cargo-features" {
        let phase_args = |phase_name: &str| {
            let phase = job.phases.iter().find(|phase| phase.name == phase_name).unwrap();
            let [stone_recipe::derivation::StepPlan::Run { args, .. }] = phase.steps.as_slice() else {
                panic!("cargo-features: frozen {phase_name} phase has unexpected steps");
            };
            args
        };
        let build = phase_args("Build");
        assert_eq!(&build[build.len() - 2..], ["--features", "fixture-protocol"]);
        let check = phase_args("Check");
        assert_eq!(
            &check[check.len() - 3..],
            ["--features", "fixture-protocol", "--workspace"]
        );
        let install = phase_args("Install");
        assert_eq!(
            &install[install.len() - 2..],
            [
                "target/x86_64-unknown-linux-gnu/release/cast-feature-client",
                "target/x86_64-unknown-linux-gnu/release/cast-feature-daemon",
            ]
        );
    }
    if name == "factory-override" {
        let setup = job
            .phases
            .iter()
            .find(|phase| phase.name == "Setup")
            .expect("factory-override: frozen CMake Setup phase is missing");
        let [stone_recipe::derivation::StepPlan::Run { args, .. }] = setup.steps.as_slice() else {
            panic!("factory-override: frozen CMake Setup phase has unexpected steps");
        };
        assert!(
            args.iter()
                .any(|argument| argument == "-DCAST_FACTORY_VARIANT=stone-override"),
            "factory-override: frozen Setup command omits the explicit package patch"
        );
        assert!(
            args.iter()
                .all(|argument| argument != "-DCAST_FACTORY_VARIANT=factory-default"),
            "factory-override: frozen Setup command retained the factory default"
        );
    }
}
