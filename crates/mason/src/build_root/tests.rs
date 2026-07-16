
use std::collections::{BTreeMap, BTreeSet};

use forge::package::{Flags, Meta, Name};
use gluon_config::Source;
use stone_recipe::UpstreamSpec;
use stone_recipe::derivation::{JobPlan, LockedOutput, LockedOutputRef};

use super::*;

#[test]
#[cfg(target_pointer_width = "64")]
fn input_origin_positions_fail_closed_above_the_lock_schema_range() {
    let overflow = usize::try_from(u64::from(u32::MAX) + 1).unwrap();
    let error = input_origin_index("inputs", overflow).unwrap_err();
    assert!(matches!(
        error,
        Error::InputOriginIndexOverflow { field, index }
            if field == "inputs" && index == overflow
    ));
}

fn package() -> forge::Package {
    forge::Package {
        id: package::Id::from("locked-id".to_owned()),
        meta: Meta {
            name: Name::from("locked".to_owned()),
            version_identifier: "1.2.3".to_owned(),
            source_release: 4,
            build_release: 5,
            architecture: "x86_64".to_owned(),
            summary: String::new(),
            description: String::new(),
            source_id: "locked".to_owned(),
            homepage: String::new(),
            licenses: Vec::new(),
            dependencies: BTreeSet::new(),
            providers: BTreeSet::new(),
            conflicts: BTreeSet::new(),
            uri: None,
            hash: None,
            download_size: None,
        },
        flags: Flags::new().with_available(),
    }
}

fn locked() -> LockedPackage {
    LockedPackage {
        package_id: "locked-id".to_owned(),
        name: "locked".to_owned(),
        version: "1.2.3-4-5".to_owned(),
        architecture: "x86_64".to_owned(),
        repository: "repo".to_owned(),
        outputs: vec![LockedOutput { name: "out".to_owned() }],
        dependencies: Vec::<LockedOutputRef>::new(),
    }
}

fn selected_inputs_package() -> PackageSpec {
    let source = Source::new(
        "stone.glu",
        r#"let cast = import! cast.package.v3
let base = cast.mk_package (cast.meta {
    pname = "example", version = "1.0.0", release = 1,
    homepage = "https://example.invalid", license = ["MPL-2.0"],
})
let scripts = cast.defaults.scripts
let selected = cast.profile_with {
    name = "x86_64",
    builder = cast.builder.shell scripts [cast.dep.binary "profile-builder"],
    hooks = cast.defaults.hooks,
    native_build_inputs = [cast.dep.package "profile-native"],
    build_inputs = [cast.dep.package "profile-build"],
    check_inputs = [cast.dep.package "profile-check"],
}
let unrelated = cast.profile_with {
    name = "aarch64",
    builder = cast.builder.shell scripts [cast.dep.binary "unrelated-builder"],
    hooks = cast.defaults.hooks,
    native_build_inputs = [cast.dep.package "unrelated-native"],
    build_inputs = [], check_inputs = [],
}
{
    builder = cast.builder.shell scripts [cast.dep.binary "base-builder"],
    native_build_inputs = [cast.dep.package "base-native"],
    build_inputs = [cast.dep.package "base-build"],
    check_inputs = [cast.dep.package "base-check"],
    profiles = [selected, unrelated],
    .. base
}
"#,
    );
    stone_recipe::package::evaluate_gluon(&source).unwrap().package
}

fn cmake_package_builder() -> stone_recipe::package::BuilderSpec {
    let source = Source::new(
        "stone.glu",
        r#"let cast = import! cast.package.v3
let cmake = import! cast.builders.cmake.v2
let base = cast.mk_package (cast.meta {
    pname = "example", version = "1.0.0", release = 1,
    homepage = "https://example.invalid", license = ["MPL-2.0"],
})
{ builder = cmake.default, .. base }
"#,
    );
    stone_recipe::package::evaluate_gluon(&source).unwrap().package.builder
}

fn repository_policy() -> BuildPolicySpec {
    crate::BuildPolicy::repository_for_tests().spec
}

#[test]
fn policy_tools_use_canonical_relation_names() {
    assert_eq!(
        [
            BuildToolSpec::Package("package-tool".to_owned()),
            BuildToolSpec::Binary("binary-tool".to_owned()),
            BuildToolSpec::SystemBinary("system-tool".to_owned()),
        ]
        .iter()
        .map(build_tool_name)
        .collect::<Result<Vec<_>, _>>()
        .unwrap(),
        ["package-tool", "binary(binary-tool)", "sysbinary(system-tool)"]
    );
}

#[test]
fn repository_base_does_not_request_ambient_or_interactive_tools() {
    let policy = repository_policy();
    let package = selected_inputs_package();
    let target = policy.targets.iter().find(|target| target.name == "x86_64").unwrap();
    let requests = inputs_for(&policy, "policy.glu", &package, None, target, false)
        .unwrap()
        .into_iter()
        .map(|input| input.request)
        .collect::<BTreeSet<_>>();

    for removed in [
        "bash",
        "cast",
        "coreutils",
        "dash",
        "diffutils",
        "findutils",
        "gawk",
        "grep",
        "libarchive",
        "os-info",
        "pkgconf",
        "sed",
        "util-linux",
        "binary(git)",
        "binary(hx)",
        "binary(less)",
        "binary(nano)",
        "binary(ps)",
        "binary(rg)",
        "binary(vim)",
    ] {
        assert!(!requests.contains(removed), "unexpected ambient request {removed}");
    }
    assert!(requests.contains("glibc-devel"));
    assert!(requests.contains("layout"));
    assert!(requests.contains("linux-headers"));
}

#[test]
fn selected_root_features_combine_typed_policy_and_builder_tools() {
    let mut policy = repository_policy();
    policy.build_root.base = vec![BuildToolSpec::Package("policy-base".to_owned())];
    policy.build_root.toolchains.llvm = vec![BuildToolSpec::Binary("wrong-llvm".to_owned())];
    policy.build_root.toolchains.gnu = vec![BuildToolSpec::Binary("policy-gnu".to_owned())];
    policy.build_root.emul32.base = vec![BuildToolSpec::SystemBinary("policy-emul-base".to_owned())];
    policy.build_root.emul32.toolchains.llvm = vec![BuildToolSpec::Package("wrong-llvm32".to_owned())];
    policy.build_root.emul32.toolchains.gnu = vec![BuildToolSpec::Package("policy-gnu32".to_owned())];
    policy.build_root.mold.linker.program = BuildProgramSpec {
        path: "/usr/bin/policy-mold".to_owned(),
        requirement: BuildToolSpec::Binary("policy-mold".to_owned()),
    };
    policy.build_root.compiler_cache.ccache = BuildProgramSpec {
        path: "/usr/bin/policy-cache".to_owned(),
        requirement: BuildToolSpec::Binary("policy-cache".to_owned()),
    };
    policy.build_root.compiler_cache.sccache = BuildProgramSpec {
        path: "/usr/bin/policy-scache".to_owned(),
        requirement: BuildToolSpec::Binary("policy-scache".to_owned()),
    };

    let mut package = selected_inputs_package();
    package.options.toolchain = ToolchainSpec::Gnu;
    package.builder = cmake_package_builder();
    package.mold = true;
    package.sources = vec![
        UpstreamSpec::Archive {
            url: "https://example.invalid/skipped.zip".to_owned(),
            hash: "skipped".to_owned(),
            rename: None,
            strip_dirs: None,
            unpack: false,
            unpack_dir: None,
        },
        UpstreamSpec::Archive {
            url: "https://example.invalid/download".to_owned(),
            hash: "archive".to_owned(),
            rename: Some("renamed.rpm".to_owned()),
            strip_dirs: None,
            unpack: true,
            unpack_dir: None,
        },
        UpstreamSpec::Git {
            url: "https://example.invalid/source.git".to_owned(),
            git_ref: "main".to_owned(),
            clone_dir: None,
        },
    ];
    let target = policy
        .targets
        .iter()
        .find(|target| target.name == "emul32/x86_64")
        .unwrap()
        .clone();

    let inputs = inputs_for(&policy, "policy.glu", &package, None, &target, true).unwrap();
    let packages = inputs
        .iter()
        .map(|input| input.request.clone())
        .collect::<BTreeSet<_>>();
    let expected = [
        "base-build",
        "base-check",
        "base-native",
        "binary(ninja)",
        "binary(g++)",
        "binary(gcc)",
        "binary(gcc-ar)",
        "binary(gcc-nm)",
        "binary(gcc-ranlib)",
        "binary(ld.bfd)",
        "binary(objcopy)",
        "binary(pkg-config)",
        "binary(policy-cache)",
        "binary(policy-gnu)",
        "binary(policy-mold)",
        "binary(policy-scache)",
        "binary(python3)",
        "binary(strip)",
        "policy-base",
        "policy-gnu32",
        "sysbinary(policy-emul-base)",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect::<BTreeSet<_>>();

    assert_eq!(packages, expected);
    for (request, origin) in [
        (
            "policy-base",
            InputOrigin::Policy {
                source: "policy.glu".to_owned(),
                field: "build_root.base".to_owned(),
                index: 0,
            },
        ),
        (
            "binary(policy-gnu)",
            InputOrigin::Policy {
                source: "policy.glu".to_owned(),
                field: "build_root.toolchains.gnu".to_owned(),
                index: 0,
            },
        ),
        (
            "binary(ninja)",
            InputOrigin::BuilderTool {
                selection: PackageInputSelection::Package,
                index: 0,
            },
        ),
        (
            "base-native",
            InputOrigin::NativeBuild {
                selection: PackageInputSelection::Package,
                index: 0,
            },
        ),
        (
            "base-build",
            InputOrigin::Build {
                selection: PackageInputSelection::Package,
                index: 0,
            },
        ),
        (
            "base-check",
            InputOrigin::Check {
                selection: PackageInputSelection::Package,
                index: 0,
            },
        ),
        (
            "binary(objcopy)",
            InputOrigin::Policy {
                source: "policy.glu".to_owned(),
                field: "build_root.analyzer_tools.gnu.objcopy".to_owned(),
                index: 0,
            },
        ),
        (
            "binary(objcopy)",
            InputOrigin::Analyzer {
                role: AnalyzerRole::Objcopy,
            },
        ),
        (
            "binary(gcc)",
            InputOrigin::CompilerExecutable {
                role: CompilerExecutableRole::Cc,
            },
        ),
        (
            "binary(policy-cache)",
            InputOrigin::CompilerCache {
                role: CompilerCacheRole::Ccache,
            },
        ),
        ("binary(policy-mold)", InputOrigin::MoldLinker),
    ] {
        assert!(
            inputs
                .iter()
                .any(|input| input.request == request && input.origin == origin),
            "missing {request:?} origin {origin:?}"
        );
    }
    assert!(!packages.contains("binary(ldc2)"));
}

#[test]
fn analyzer_tools_follow_handlers_options_and_selected_toolchain_exactly() {
    let mut policy = repository_policy();
    policy.build_root.analyzer_tools.pkg_config = BuildToolSpec::Binary("policy-pkg-config".to_owned());
    policy.build_root.analyzer_tools.python = BuildToolSpec::Binary("policy-python".to_owned());
    policy.build_root.analyzer_tools.llvm.objcopy = BuildToolSpec::Binary("policy-llvm-objcopy".to_owned());
    policy.build_root.analyzer_tools.llvm.strip = BuildToolSpec::Binary("policy-llvm-strip".to_owned());
    policy.build_root.analyzer_tools.gnu.objcopy = BuildToolSpec::Binary("policy-gnu-objcopy".to_owned());
    policy.build_root.analyzer_tools.gnu.strip = BuildToolSpec::Binary("policy-gnu-strip".to_owned());
    let mut package = selected_inputs_package();

    policy.analyzers = vec![AnalyzerKind::IncludeAny];
    let selected = selected_analyzer_tools(&policy, &package);
    assert!(selected.pkg_config.is_none());
    assert!(selected.python.is_none());
    assert!(selected.objcopy.is_none());
    assert!(selected.strip.is_none());

    policy.analyzers = vec![AnalyzerKind::PkgConfig, AnalyzerKind::Python, AnalyzerKind::IncludeAny];
    let selected = selected_analyzer_tools(&policy, &package);
    assert_eq!(
        selected
            .pkg_config
            .and_then(BuildToolSpec::executable_program)
            .as_deref(),
        Some("/usr/bin/policy-pkg-config")
    );
    assert_eq!(
        selected.python.and_then(BuildToolSpec::executable_program).as_deref(),
        Some("/usr/bin/policy-python")
    );
    assert!(selected.objcopy.is_none());
    assert!(selected.strip.is_none());

    policy.analyzers = vec![AnalyzerKind::Elf, AnalyzerKind::IncludeAny];
    package.options.toolchain = ToolchainSpec::Llvm;
    package.options.debug = true;
    package.options.strip = false;
    let selected = selected_analyzer_tools(&policy, &package);
    assert_eq!(
        selected.objcopy.and_then(BuildToolSpec::executable_program).as_deref(),
        Some("/usr/bin/policy-llvm-objcopy")
    );
    assert!(selected.strip.is_none());

    package.options.toolchain = ToolchainSpec::Gnu;
    package.options.debug = false;
    package.options.strip = true;
    let selected = selected_analyzer_tools(&policy, &package);
    assert!(selected.objcopy.is_none());
    assert_eq!(
        selected.strip.and_then(BuildToolSpec::executable_program).as_deref(),
        Some("/usr/bin/policy-gnu-strip")
    );
}

fn executable(name: &str) -> ExecutablePlan {
    ExecutablePlan {
        path: format!("/usr/bin/{name}"),
        requirement: RelationPlan {
            kind: stone_recipe::derivation::RelationKind::Binary,
            name: name.to_owned(),
        },
    }
}

#[test]
fn exact_root_executables_follow_only_reachable_frozen_jobs() {
    let jobs = [Job {
        pgo_stage: None,
        phases: BTreeMap::from([(
            crate::build::job::Phase::Workload,
            stone_recipe::derivation::PhasePlan {
                name: "workload".to_owned(),
                pre: vec![StepPlan::Run {
                    program: executable("remove"),
                    args: Vec::new(),
                    environment: BTreeMap::new(),
                    working_dir: "/mason/build".to_owned(),
                }],
                steps: vec![StepPlan::Shell {
                    interpreter: executable("bash"),
                    declared_programs: vec![executable("profdata")],
                    script: "ambient-command must-not-be-inferred".to_owned(),
                    environment: BTreeMap::new(),
                    working_dir: "/mason/build".to_owned(),
                }],
                post: vec![StepPlan::RunBuilt {
                    program: "/mason/build/generated/self-test".to_owned(),
                    args: vec!["--verify".to_owned()],
                    environment: BTreeMap::new(),
                    working_dir: "/mason/build".to_owned(),
                }],
            },
        )]),
        work_dir: PathBuf::from("/mason/build"),
        build_dir: PathBuf::from("/mason/build"),
    }];

    let mut requested = Vec::new();
    extend_job_executables(&mut requested, &jobs).unwrap();
    assert_eq!(
        requested
            .iter()
            .map(|input| input.request.clone())
            .collect::<BTreeSet<_>>(),
        ["binary(bash)", "binary(profdata)", "binary(remove)"]
            .into_iter()
            .map(str::to_owned)
            .collect()
    );
    for (request, section, role) in [
        ("binary(remove)", JobStepSection::Pre, JobExecutableRole::RunProgram),
        (
            "binary(bash)",
            JobStepSection::Steps,
            JobExecutableRole::ShellInterpreter,
        ),
        (
            "binary(profdata)",
            JobStepSection::Steps,
            JobExecutableRole::ShellDeclaredProgram { index: 0 },
        ),
    ] {
        assert!(requested.iter().any(|input| {
            input.request == request
                && input.origin
                    == InputOrigin::JobExecutable {
                        job: 0,
                        phase: 0,
                        phase_name: "workload".to_owned(),
                        section,
                        step: 0,
                        role: role.clone(),
                    }
        }));
    }
}

#[test]
fn frozen_executable_bindings_name_exact_locked_provider_packages() {
    let mut plan = crate::package::test_derivation_plan();
    plan.jobs.push(JobPlan {
        pgo_stage: None,
        pgo_dir: None,
        build_dir: "/mason/build".to_owned(),
        work_dir: "/mason/build".to_owned(),
        phases: vec![stone_recipe::derivation::PhasePlan {
            name: "check".to_owned(),
            pre: Vec::new(),
            steps: vec![StepPlan::RunBuilt {
                program: "/mason/build/generated/self-test".to_owned(),
                args: Vec::new(),
                environment: BTreeMap::new(),
                working_dir: "/mason/build".to_owned(),
            }],
            post: Vec::new(),
        }],
    });
    plan.validate().unwrap();
    let bindings = frozen_executable_bindings(&plan).unwrap();

    assert_eq!(bindings.len(), 16);
    assert_eq!(
        bindings
            .iter()
            .map(|binding| (binding.package.as_str(), binding.path.as_path()))
            .collect::<BTreeSet<_>>(),
        [
            ("analyzer-tools-id", std::path::Path::new("/usr/bin/llvm-strip")),
            ("analyzer-tools-id", std::path::Path::new("/usr/bin/pkg-config")),
            ("analyzer-tools-id", std::path::Path::new("/usr/bin/python3")),
        ]
        .into_iter()
        .collect()
    );
}

#[test]
fn frozen_executable_binding_rejects_missing_or_duplicate_request_mapping() {
    let mut missing = crate::package::test_derivation_plan();
    missing
        .build_lock
        .requests
        .retain(|request| request.request != "binary(python3)");
    assert!(matches!(
        frozen_executable_bindings(&missing),
        Err(Error::MissingFrozenExecutableRequest(request)) if request == "binary(python3)"
    ));

    let mut duplicate = crate::package::test_derivation_plan();
    let repeated = duplicate
        .build_lock
        .requests
        .iter()
        .find(|request| request.request == "binary(pkg-config)")
        .unwrap()
        .clone();
    duplicate.build_lock.requests.push(repeated);
    assert!(matches!(
        frozen_executable_bindings(&duplicate),
        Err(Error::DuplicateFrozenExecutableRequest(request)) if request == "binary(pkg-config)"
    ));
}

#[test]
fn exact_root_rejects_locked_metadata_drift() {
    let locked = locked();
    let mut package = package();
    assert!(locked_metadata_matches(&locked, &package));

    package.meta.name = Name::from("replacement".to_owned());
    assert!(!locked_metadata_matches(&locked, &package));
    package = self::package();
    package.meta.build_release += 1;
    assert!(!locked_metadata_matches(&locked, &package));
    package = self::package();
    package.meta.architecture = "aarch64".to_owned();
    assert!(!locked_metadata_matches(&locked, &package));
}

#[test]
fn frozen_root_excludes_unlocked_higher_priority_repositories_before_resolution() {
    let configured = repository::Map::with([
        (
            repository::Id::new("locked"),
            repository::Repository {
                description: "locked".to_owned(),
                source: repository::Source::DirectIndex("https://locked.invalid/stone.index".parse().unwrap()),
                priority: repository::Priority::new(1),
                active: true,
            },
        ),
        (
            repository::Id::new("ambient"),
            repository::Repository {
                description: "unlocked higher-priority source".to_owned(),
                source: repository::Source::DirectIndex("https://ambient.invalid/stone.index".parse().unwrap()),
                priority: repository::Priority::new(u64::MAX),
                active: true,
            },
        ),
    ]);
    let locked = [RepositorySnapshot {
        id: "locked".to_owned(),
        index_uri: "https://locked.invalid/stone.index".to_owned(),
        snapshot: "snapshot".to_owned(),
    }];

    let selected = locked_repositories(&configured, &locked).unwrap();
    assert_eq!(
        selected.iter().map(|(id, _)| id.to_string()).collect::<Vec<_>>(),
        ["locked"]
    );
    assert!(!selected.contains_id(&repository::Id::new("ambient")));
}

#[test]
fn direct_inputs_use_root_only_without_a_profile() {
    let package = selected_inputs_package();
    let inputs = declared_inputs_for(&package, None)
        .unwrap()
        .into_iter()
        .map(|relation| relation.canonical_name())
        .collect::<Vec<_>>();

    assert_eq!(
        inputs,
        ["binary(base-builder)", "base-native", "base-build", "base-check"]
    );
}

#[test]
fn direct_inputs_use_only_the_selected_profile() {
    let package = selected_inputs_package();
    let selected = declared_inputs_for(&package, Some("x86_64"))
        .unwrap()
        .into_iter()
        .map(|relation| relation.canonical_name())
        .collect::<Vec<_>>();

    assert_eq!(
        selected,
        [
            "binary(profile-builder)",
            "profile-native",
            "profile-build",
            "profile-check"
        ]
    );
    assert!(selected.iter().all(|input| !input.contains("unrelated")));
    assert!(selected.iter().all(|input| !input.contains("base-")));
}
