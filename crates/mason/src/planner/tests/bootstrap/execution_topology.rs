const REQUIRED_EXECUTION_FIXTURES: [&str; 12] = [
    "autotools",
    "autotools-options",
    "cargo",
    "cargo-features",
    "cargo-vendored",
    "cmake",
    "custom",
    "daemon-generated",
    "factory-override",
    "hooks-patch",
    "meson",
    "split",
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
