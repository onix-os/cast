const REQUIRED_EXECUTION_FIXTURES: [&str; 16] = [
    "autotools",
    "autotools-options",
    "cargo",
    "cargo-features",
    "cargo-vendored",
    "cmake",
    "custom",
    "daemon-generated",
    "factory-override",
    "generated-config",
    "generated-shell",
    "hooks-patch",
    "meson",
    "plugin-output",
    "split",
    "userspace-profile",
];
const EXECUTION_FIXTURE_SELECTOR_ENV: &str = "CAST_EXECUTION_FIXTURE";

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
        "autotools" | "autotools-options" => vec![
            prepare(if name == "autotools" {
                "cast-autotools-fixture"
            } else {
                "cast-autotools-options-fixture"
            }),
            phase("Setup", vec![run("dash", "./configure")]),
            phase("Build", vec![run("make", "VERBOSE=1")]),
            phase("Install", vec![run("make", "install")]),
        ]
        .into_iter()
        .chain((name == "autotools").then(|| phase("Check", vec![run("make", "check")])))
        .collect(),
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
        "daemon-generated" => vec![
            prepare("cast-daemon-fixture"),
            phase("Setup", vec![run("cmake", "-G")]),
            phase("Build", vec![run("cmake", "--build")]),
            phase("Install", vec![run("cmake", "--install")]),
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
                vec![run("patch", "-p1")],
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
    if name == "userspace-profile" {
        assert_userspace_profile_relations(plan);
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
