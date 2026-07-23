use gluon_config::{EvaluationFingerprint, GluonEngine, ImportPolicy, Source};
use stone::relation::{Dependency, Kind as StoneRelationKind};

use crate::{build_policy::layers::BuildPolicyOperation, spec::SourceUrlValidationError};

use super::*;

const SOURCE_LOCK_BYTES: &[u8] = b"canonical sample source lock";
type NamedMutation<T> = (&'static str, Box<dyn Fn(&mut T)>);

fn evaluation(logical_name: &str, source: &str, explicit_inputs: &[u8]) -> EvaluationFingerprint {
    GluonEngine::default()
        .evaluate_with_inputs::<i64>(&Source::new(logical_name, source), explicit_inputs)
        .unwrap()
        .fingerprint
}

fn evaluation_with_import(logical_name: &str, explicit_inputs: &[u8]) -> EvaluationFingerprint {
    let policy = ImportPolicy::new()
        .with_embedded_module("sample.provenance", "4")
        .unwrap();
    GluonEngine::default()
        .with_import_policy(policy)
        .evaluate_with_inputs::<i64>(&Source::new(logical_name, "import! sample.provenance"), explicit_inputs)
        .unwrap()
        .fingerprint
}

fn sample_provenance() -> DerivationProvenance {
    let profiles = vec![
        ProfileFragmentProvenance {
            logical_name: "vendor/profile.glu".to_owned(),
            evaluation: evaluation_with_import("profile.glu", &[]),
        },
        ProfileFragmentProvenance {
            logical_name: "admin/profile.d/local.glu".to_owned(),
            evaluation: evaluation("profile.d/local.glu", "2", &[]),
        },
    ];
    let layers = vec![
        PolicyLayerProvenance {
            name: "foundation".to_owned(),
            transitions: vec![PolicyTransitionProvenance {
                operation: BuildPolicyOperation::Add,
                origin: "default.glu".to_owned(),
                evaluation: evaluation_with_import("default.glu", &[]),
            }],
        },
        PolicyLayerProvenance {
            name: "site".to_owned(),
            transitions: Vec::new(),
        },
    ];
    let policy_inputs = policy_composition_identity("aerynos", &layers);
    DerivationProvenance {
        recipe: evaluation_with_import("stone.glu", SOURCE_LOCK_BYTES),
        profiles,
        policy: PolicyProvenance {
            name: "aerynos".to_owned(),
            root: evaluation("policy.glu", "5", &policy_inputs),
            layers,
        },
    }
}

fn sample_plan() -> DerivationPlan {
    let provenance = sample_provenance();
    let mut build_lock = build_lock::sample_lock();
    build_lock.requests.extend(
        [
            "pkg-config",
            "python3",
            "llvm-objcopy",
            "llvm-strip",
            "objcopy",
            "strip",
            "cmake",
            "bash",
        ]
        .into_iter()
        .map(|name| {
            let mut origins = vec![InputOrigin::Policy {
                source: "policy.glu".to_owned(),
                field: "build_root.base".to_owned(),
                index: 0,
            }];
            if name == "cmake" {
                origins.extend(
                    ToolchainCommandsPlan::COMPILER_ROLES
                        .into_iter()
                        .map(|role| InputOrigin::CompilerExecutable { role }),
                );
            }
            LockedRequest {
                request: format!("binary({name})"),
                package_id: "hello-id".to_owned(),
                output: "out".to_owned(),
                origins,
            }
        }),
    );
    build_lock.policy.name = provenance.policy.name.clone();
    build_lock.policy.fingerprint = provenance.policy.root.sha256.clone();
    build_lock.profile.fingerprint = profile_aggregate_fingerprint(&provenance.profiles);
    build_lock.normalize();
    let mut plan = DerivationPlan::new(
        PackageIdentity {
            name: "hello".to_owned(),
            version: "1.0.0".to_owned(),
            source_release: 1,
            build_release: 1,
            homepage: "https://example.invalid/hello".to_owned(),
            licenses: vec!["MPL-2.0".to_owned()],
            architecture: "x86_64".to_owned(),
        },
        build_lock,
        provenance,
    );
    plan.cast_version = "0.26.6".to_owned();
    plan.cast_fingerprint = "sha256:test-cast-semantics".to_owned();
    plan.source_lock_digest = sha256(SOURCE_LOCK_BYTES);
    plan.sources = vec![LockedSource::Archive {
        order: 0,
        url: "https://example.invalid/hello.tar.zst".to_owned(),
        sha256: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_owned(),
        filename: "hello.tar.zst".to_owned(),
    }];
    plan.jobs = vec![JobPlan {
        pgo_stage: None,
        pgo_dir: None,
        build_dir: "/mason/build".to_owned(),
        work_dir: "/mason/build/hello".to_owned(),
        phases: vec![PhasePlan {
            name: "build".to_owned(),
            pre: Vec::new(),
            steps: vec![StepPlan::Run {
                program: ExecutablePlan {
                    path: "/usr/bin/cmake".to_owned(),
                    requirement: RelationPlan {
                        kind: RelationKind::Binary,
                        name: "cmake".to_owned(),
                    },
                },
                args: vec!["--build".to_owned(), ".".to_owned()],
                environment: BTreeMap::from([("CFLAGS".to_owned(), "-O2".to_owned())]),
                working_dir: "/mason/build".to_owned(),
            }],
            post: Vec::new(),
        }],
    }];
    plan.environment = BTreeMap::from([
        ("HOME".to_owned(), "/mason/build".to_owned()),
        ("PATH".to_owned(), "/usr/bin:/bin".to_owned()),
    ]);
    plan.layout = BuilderLayout {
        hostname: "cast-builder".to_owned(),
        guest_root: "/mason".to_owned(),
        artifacts_dir: "/mason/artefacts".to_owned(),
        build_dir: "/mason/build".to_owned(),
        source_dir: "/mason/sources".to_owned(),
        recipe_dir: "/mason/recipe".to_owned(),
        install_dir: "/mason/install".to_owned(),
        package_dir: "/mason/recipe/pkg".to_owned(),
        ccache_dir: "/mason/ccache".to_owned(),
        sccache_dir: "/mason/sccache".to_owned(),
        go_cache_dir: "/mason/gocache".to_owned(),
        go_mod_cache_dir: "/mason/gomodcache".to_owned(),
        cargo_cache_dir: "/mason/cargocache".to_owned(),
        zig_cache_dir: "/mason/zigcache".to_owned(),
    };
    plan.execution = ExecutionPolicy {
        executor: LockedIdentity {
            name: "cast-executor-v1".to_owned(),
            fingerprint: "executor-fingerprint".to_owned(),
        },
        root_materialization: RootMaterializationMode::LockedClosure,
        credentials: ExecutionCredentials::IsolatedRoot,
        network: NetworkMode::Disabled,
        filesystems: FilesystemPolicy::default(),
        compiler_cache: false,
        jobs: 4,
    };
    plan.toolchain_commands.compilers = ToolchainCommandsPlan::COMPILER_ROLES
        .into_iter()
        .map(|role| CompilerCommandPlan {
            role,
            command: ExecutableCommandPlan {
                program: sample_analyzer_tool("cmake"),
                args: (role == CompilerExecutableRole::Cpp)
                    .then(|| vec!["-E".to_owned()])
                    .unwrap_or_default(),
            },
        })
        .collect();
    plan.analysis.handlers = vec![AnalyzerKind::Elf, AnalyzerKind::Python, AnalyzerKind::IncludeAny];
    plan.analysis.tools.python = Some(sample_analyzer_tool("python3"));
    plan.analysis.tools.strip = Some(sample_analyzer_tool("llvm-strip"));
    plan.manifest_build_inputs = vec![RelationPlan {
        kind: RelationKind::Binary,
        name: "cmake".to_owned(),
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
    plan.outputs = vec![OutputPlan {
        name: "out".to_owned(),
        package_name: "hello".to_owned(),
        include_in_manifest: true,
        summary: Some("Hello".to_owned()),
        description: None,
        provides_exclude: Vec::new(),
        runtime_exclude: Vec::new(),
        runtime_inputs: Vec::new(),
        conflicts: vec![RelationPlan {
            kind: RelationKind::PackageName,
            name: "busybox".to_owned(),
        }],
    }];
    plan.source_date_epoch = 1_700_000_000;
    plan
}

fn sample_analyzer_tool(name: &str) -> ExecutablePlan {
    ExecutablePlan {
        path: format!("/usr/bin/{name}"),
        requirement: RelationPlan {
            kind: RelationKind::Binary,
            name: name.to_owned(),
        },
    }
}

fn sample_git_source(order: u32, directory: &str) -> LockedSource {
    LockedSource::Git {
        order,
        url: format!("https://example.invalid/source-{order}.git"),
        requested_ref: "main".to_owned(),
        commit: "0123456789abcdef0123456789abcdef01234567".to_owned(),
        materialization_sha256: "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb".to_owned(),
        directory: directory.to_owned(),
    }
}

fn sample_step_mut(plan: &mut DerivationPlan) -> &mut StepPlan {
    &mut plan.jobs[0].phases[0].steps[0]
}

fn insert_prepare_archive_steps(plan: &mut DerivationPlan, steps: Vec<StepPlan>) {
    plan.jobs[0].phases.insert(
        0,
        PhasePlan {
            name: "Prepare".to_owned(),
            pre: Vec::new(),
            steps,
            post: Vec::new(),
        },
    );
}

fn archive_step(source: u32, destination: &str, strip_components: u32) -> StepPlan {
    StepPlan::ExtractArchive {
        source,
        destination: destination.to_owned(),
        strip_components,
    }
}

fn make_sample_shell(plan: &mut DerivationPlan) {
    let (environment, working_dir) = match sample_step_mut(plan) {
        StepPlan::Run {
            environment,
            working_dir,
            ..
        } => (environment.clone(), working_dir.clone()),
        StepPlan::RunBuilt { .. } | StepPlan::Shell { .. } | StepPlan::ExtractArchive { .. } => return,
    };
    *sample_step_mut(plan) = StepPlan::Shell {
        interpreter: sample_analyzer_tool("bash"),
        declared_programs: vec![sample_analyzer_tool("cmake")],
        script: "printf '%s\\n' hardened".to_owned(),
        environment,
        working_dir,
    };
}

fn make_sample_run_built(plan: &mut DerivationPlan) {
    let (environment, working_dir) = match sample_step_mut(plan) {
        StepPlan::Run {
            environment,
            working_dir,
            ..
        } => (environment.clone(), working_dir.clone()),
        StepPlan::RunBuilt { .. } | StepPlan::Shell { .. } | StepPlan::ExtractArchive { .. } => return,
    };
    *sample_step_mut(plan) = StepPlan::RunBuilt {
        program: "/mason/build/bin/self-test".to_owned(),
        args: vec!["--verify".to_owned()],
        environment,
        working_dir,
    };
}

fn measured_process_budget(plan: &DerivationPlan) -> ProcessDataBudget {
    let mut budget = ProcessDataBudget::new(DerivationValidationLimits::default());
    budget.validate(plan).unwrap();
    budget
}

fn assert_process_limit(
    error: DerivationValidationError,
    expected_field: &str,
    expected_actual: usize,
    expected_limit: usize,
    expected_unit: &'static str,
) {
    let DerivationValidationError::LimitExceeded {
        field,
        actual,
        limit,
        unit,
    } = error
    else {
        panic!("expected a process-data limit, found: {error}");
    };
    assert_eq!(field, expected_field);
    assert_eq!(actual, expected_actual);
    assert_eq!(limit, expected_limit);
    assert_eq!(unit, expected_unit);
}

include!("identity_and_limits.rs");
include!("policy_and_provenance.rs");
include!("relations_and_tools.rs");
include!("execution_and_layout.rs");
include!("sources_and_outputs.rs");
