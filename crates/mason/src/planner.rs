//! Resolve and freeze a target-specific Cast derivation plan.

use std::{
    collections::{BTreeMap, BTreeSet},
    num::{NonZeroU32, NonZeroU64, NonZeroUsize},
    path::{Path, PathBuf},
};

use forge::{Installation, runtime};
use declarative_config::DeclarationEvaluator;
use sha2::{Digest, Sha256};
use stone_recipe::derivation::{
    AnalysisPlan, AnalysisToolsPlan, BUILD_LOCK_SCHEMA_VERSION, BuildLock, CollectionRulePlan, CompilerCommandPlan,
    CompilerExecutableRole, DerivationPlan, DerivationProvenance, DerivationValidationError, ExecutableCommandPlan,
    ExecutablePlan, ExecutionCredentials, ExecutionPolicy, FilesystemPolicy, InputOrigin, JobPlan, LockedIdentity,
    LockedOutput, LockedOutputRef, LockedPackage, LockedRequest, LockedSource, NetworkMode, OutputPlan, OutputRelation,
    PackageIdentity, Platform, RelationPlan, RepositorySnapshot, RequestedInput, RootMaterializationMode, StepPlan,
    ToolchainCommandsPlan, profile_aggregate_fingerprint, requested_inputs_digest,
};
use stone_recipe::{
    build_policy::{BuildCommandSpec, BuildPolicySpec, BuildToolSpec, CompilerToolsSpec},
    package::{
        BuilderEnvironmentSpec, BuilderSpec, DependencySpec, HooksSpec, PackageSpec, PhaseSpec, ProgramSpec, StepSpec,
    },
};
use thiserror::Error;

use crate::{
    Env,
    build::{self, Builder},
    build_lock, generated_lock,
    package::{Packager, ResolvedOutput},
    profile,
    source_lock::{GluonSourceLockCodec, SOURCE_LOCK_FILE_NAME, SourceLock, SourceResolution},
};

mod freeze;
mod identity;
mod lock_resolution;

use freeze::{
    freeze_analysis, freeze_jobs, freeze_outputs, freeze_sources, freeze_toolchain_commands,
    jobs_use_package_directory, output_name,
};
pub(crate) use identity::executor_fingerprint;
use identity::{
    aggregate_inputs, hash_fields, platform, selected_builder_identity, sha256, target_fingerprint,
    toolchain_fingerprint,
};
#[cfg(test)]
use identity::{structural_builder_fingerprint, structural_builder_name};
use lock_resolution::resolve_build_lock;

pub(crate) const EXECUTOR_ABI: &str = "cast-executor-v1";

#[derive(Debug, Clone)]
pub struct Request {
    pub recipe: PathBuf,
    pub profile: profile::Id,
    pub target: String,
    pub source_date_epoch: i64,
    pub build_release: NonZeroU64,
    pub jobs: NonZeroU32,
    pub compiler_cache: bool,
    pub update_lock: bool,
    pub refresh_repositories: bool,
}

pub struct Planned {
    pub plan: DerivationPlan,
    pub runtime: build::Runtime,
    pub lock_path: PathBuf,
    pub lock_outcome: Option<build_lock::WriteOutcome>,
}

pub fn plan(env: Env, request: Request) -> Result<Planned, Error> {
    plan_with_runtime(env, request, Path::new("."))
}

pub fn plan_for_build(env: Env, request: Request, output_dir: &Path) -> Result<Planned, Error> {
    plan_with_runtime(env, request, output_dir)
}

fn plan_with_runtime(env: Env, request: Request, output_dir: &Path) -> Result<Planned, Error> {
    if request.refresh_repositories && !request.update_lock {
        return Err(Error::RefreshRequiresUpdate);
    }
    let builder = Builder::new(build::BuilderRequest {
        recipe_path: request.recipe.clone(),
        env,
        profile: request.profile.clone(),
        compiler_cache: request.compiler_cache,
        output_dir: output_dir.to_owned(),
        jobs: NonZeroUsize::new(usize::try_from(request.jobs.get()).expect("u32 fits supported usize"))
            .expect("jobs is non-zero"),
        source_date_epoch: Some(request.source_date_epoch),
        requested_target: request.target.clone(),
    })?;
    let target = &builder.target;
    let target_policy = &target.target_policy;
    let target_name = &target_policy.name;

    let packager = Packager::new(&builder.paths, &builder.recipe)?;
    let package_names = packager.resolved_packages().keys().cloned().collect::<BTreeSet<_>>();
    let mut unresolved_inputs = build::root::inputs(&builder)?;
    for (package_name, package) in packager.resolved_packages() {
        let output = output_name(&builder.recipe.declaration.meta.pname, package_name);
        for (input_index, dependency) in package.runtime_inputs.iter().enumerate() {
            let request = dependency.to_name();
            if package_names.contains(&request) {
                continue;
            }
            unresolved_inputs.push(build::root::UnresolvedInput {
                request,
                origin: InputOrigin::OutputRuntime {
                    output: output.clone(),
                    index: build::root::input_origin_index("outputs[].runtime_inputs", input_index)?,
                },
            });
        }
    }
    let requested_inputs = aggregate_inputs(unresolved_inputs);

    let source_lock_codec = GluonSourceLockCodec::default();
    let source_lock_bytes = match generated_lock::read(
        &builder.recipe.path.with_file_name(SOURCE_LOCK_FILE_NAME),
        DeclarationEvaluator::<SourceLock>::limits(&source_lock_codec).max_source_bytes,
    ) {
        Ok(bytes) => bytes,
        Err(error) if error.is_not_found() && builder.recipe.declaration.sources.is_empty() => Vec::new(),
        Err(error) if error.is_not_found() => return Err(Error::MissingSourceLock),
        Err(source) => return Err(Error::ReadSourceLock(Box::new(source))),
    };
    let source_lock_digest = sha256(&source_lock_bytes);
    let profile_fingerprint = profile_aggregate_fingerprint(&builder.profile_fragments);
    let profile_name = builder.profile.to_string();
    let toolchain_name = match builder.recipe.declaration.options.toolchain {
        stone_recipe::ToolchainSpec::Llvm => "llvm",
        stone_recipe::ToolchainSpec::Gnu => "gnu",
    };
    let cast_version = tools_buildinfo::get_version().to_owned();
    let cast_fingerprint = tools_buildinfo::get_semantic_fingerprint();
    let jobs = request.jobs.to_string();
    let selected_builder = selected_builder_identity(&builder.recipe, target_policy);
    let executor = LockedIdentity {
        name: EXECUTOR_ABI.to_owned(),
        fingerprint: executor_fingerprint(&cast_version, cast_fingerprint),
    };
    let expected_lock = build_lock::ExpectedBuildLockContext {
        requested_inputs: requested_inputs.clone(),
        build_platform: platform(&target_policy.build_platform),
        host_platform: platform(&target_policy.host_platform),
        target_platform: platform(&target_policy.target_platform),
        policy: LockedIdentity {
            name: target.build_policy.provenance.name.clone(),
            fingerprint: target.build_policy.provenance.root.sha256.clone(),
        },
        target: LockedIdentity {
            name: target_name.clone(),
            fingerprint: target_fingerprint(target_name, &target.build_policy.provenance.root.sha256),
        },
        profile: LockedIdentity {
            name: profile_name.clone(),
            fingerprint: profile_fingerprint.clone(),
        },
        toolchain: LockedIdentity {
            name: toolchain_name.to_owned(),
            fingerprint: toolchain_fingerprint(toolchain_name, &target.build_policy.provenance.root.sha256),
        },
        builder: selected_builder.clone(),
    };
    let input_provenance_digest = requested_inputs_digest(&requested_inputs);
    let request_fingerprint = hash_fields([
        "cast-build-lock-request-v8",
        builder.recipe.fingerprint.sha256.as_str(),
        source_lock_digest.as_str(),
        target_name.as_str(),
        target.build_policy.provenance.root.sha256.as_str(),
        profile_name.as_str(),
        profile_fingerprint.as_str(),
        toolchain_name,
        selected_builder.name.as_str(),
        selected_builder.fingerprint.as_str(),
        jobs.as_str(),
        input_provenance_digest.as_str(),
    ]);

    let lock_path = build_lock::path_for_recipe(&builder.recipe.path);
    let (build_lock, lock_outcome) = if request.update_lock {
        let lock = resolve_build_lock(
            &builder,
            &requested_inputs,
            &request_fingerprint,
            &expected_lock,
            request.refresh_repositories,
        )?;
        let outcome = build_lock::write(&lock_path, &lock)?;
        (lock, Some(outcome))
    } else {
        (
            build_lock::require_current_for_context(&lock_path, &request_fingerprint, &expected_lock)?,
            None,
        )
    };

    let jobs = freeze_jobs(target)?;
    let package_dir = builder.paths.layout().package_dir.clone();
    if jobs_use_package_directory(&jobs, &package_dir) {
        return Err(Error::UnsupportedPackageDirectoryInput { package_dir });
    }
    let outputs = freeze_outputs(
        &builder.recipe.declaration.meta.pname,
        packager.resolved_packages(),
        &build_lock,
    )?;
    let analysis = freeze_analysis(&target.build_policy.spec, &builder.recipe.declaration, &build_lock)?;
    let provenance = DerivationProvenance {
        recipe: builder.recipe.fingerprint.clone(),
        profiles: builder.profile_fragments.clone(),
        policy: target.build_policy.provenance.clone(),
    };
    let mut plan = DerivationPlan::new(
        PackageIdentity {
            name: builder.recipe.declaration.meta.pname.clone(),
            version: builder.recipe.declaration.meta.version.clone(),
            source_release: u64::try_from(builder.recipe.declaration.meta.release).expect("validated package release"),
            build_release: request.build_release.get(),
            homepage: builder.recipe.declaration.meta.homepage.clone(),
            licenses: builder.recipe.declaration.meta.license.clone(),
            architecture: target_policy.artifact_architecture.clone(),
        },
        build_lock,
        provenance,
    );
    plan.cast_version = cast_version;
    plan.cast_fingerprint = cast_fingerprint.to_owned();
    plan.source_lock_digest = source_lock_digest;
    plan.sources = freeze_sources(&builder.recipe);
    plan.jobs = jobs;
    plan.environment = BTreeMap::from([
        ("HOME".to_owned(), target.jobs[0].build_dir.display().to_string()),
        ("PATH".to_owned(), "/usr/bin:/bin".to_owned()),
        ("SOURCE_DATE_EPOCH".to_owned(), request.source_date_epoch.to_string()),
    ]);
    plan.layout = builder.paths.layout().clone();
    plan.execution = ExecutionPolicy {
        executor,
        root_materialization: RootMaterializationMode::LockedClosure,
        credentials: match target.build_policy.spec.sandbox.credentials {
            stone_recipe::build_policy::SandboxCredentialPolicySpec::IsolatedRoot => ExecutionCredentials::IsolatedRoot,
        },
        network: if builder.recipe.declaration.options.networking {
            NetworkMode::Enabled
        } else {
            NetworkMode::Disabled
        },
        filesystems: FilesystemPolicy::from(&target.build_policy.spec.sandbox.filesystems),
        compiler_cache: request.compiler_cache,
        jobs: request.jobs.get(),
    };
    plan.toolchain_commands = freeze_toolchain_commands(
        &target.build_policy.spec,
        &builder.recipe.declaration.options.toolchain,
        request.compiler_cache,
        builder.recipe.declaration.mold,
    );
    plan.analysis = analysis;
    plan.manifest_build_inputs = build::root::declared_inputs(&builder.recipe, target_policy)?;
    plan.collection_rules = packager
        .collection_rules()
        .map(|(package, kind, pattern)| CollectionRulePlan {
            output: output_name(&builder.recipe.declaration.meta.pname, package),
            kind,
            pattern: pattern.to_owned(),
        })
        .collect();
    plan.outputs = outputs;
    plan.source_date_epoch = request.source_date_epoch;
    plan.validate()?;
    let runtime = builder.into_runtime(&plan)?;

    Ok(Planned {
        plan,
        runtime,
        lock_path,
        lock_outcome,
    })
}

#[derive(Debug, Error)]
pub enum Error {
    #[error("build planner context")]
    Builder(#[source] Box<build::Error>),
    #[error(transparent)]
    Package(#[from] crate::package::Error),
    #[error(transparent)]
    Root(#[from] build::root::Error),
    #[error(transparent)]
    Policy(#[from] crate::policy::Error),
    #[error(transparent)]
    BuildLock(#[from] build_lock::Error),
    #[error(transparent)]
    BuildLockValidation(#[from] stone_recipe::derivation::BuildLockValidationError),
    #[error(transparent)]
    Derivation(#[from] DerivationValidationError),
    #[error(transparent)]
    ForgeClient(#[from] forge::client::Error),
    #[error(transparent)]
    ForgeResolve(#[from] forge::client::ResolveError),
    #[error(transparent)]
    ForgeInstallation(#[from] forge::installation::Error),
    #[error("`--refresh-repositories` requires `--update-lock`")]
    RefreshRequiresUpdate,
    #[error("sources.lock.glu is required when a recipe declares sources")]
    MissingSourceLock,
    #[error("read sources.lock.glu")]
    ReadSourceLock(#[source] Box<generated_lock::ReadError>),
    #[error("output package `{package}` has runtime dependency `{dependency}` absent from the locked closure")]
    UnlockedRuntimeDependency { package: String, dependency: String },
    #[error("frozen execution does not support mutable package-directory input {package_dir}")]
    UnsupportedPackageDirectoryInput { package_dir: String },
    #[error("invalid policy analyzer tool at {field}")]
    InvalidAnalyzerTool {
        field: &'static str,
        #[source]
        source: stone::relation::ParseError,
    },
    #[error("policy analyzer tool at {field} is not an executable capability")]
    AnalyzerToolNotExecutable { field: &'static str },
    #[error("policy analyzer tool at {field} has no exact provider in build.lock.glu: {request}")]
    UnlockedAnalyzerTool { field: &'static str, request: String },
    #[error("package resolution returned an input request with no typed planner origin: {request}")]
    UnclassifiedResolvedInput { request: String },
    #[error("package resolution omitted a typed input request from its exact result: {request}")]
    MissingResolvedInput { request: String },
}

impl From<build::Error> for Error {
    fn from(error: build::Error) -> Self {
        Self::Builder(Box::new(error))
    }
}

#[cfg(test)]
mod tests {
    use declarative_config::DeclarationCodec;
    use fs_err as fs;

    use super::*;
    use crate::source_lock::GluonSourceLockCodec;
    use stone_recipe::derivation::PhasePlan;

    fn package_shell(script: &str) -> StepSpec {
        StepSpec::Shell {
            interpreter: ProgramSpec {
                path: "/usr/bin/bash".to_owned(),
                requirement: DependencySpec::Binary("bash".to_owned()),
            },
            declared_programs: Vec::new(),
            script: script.to_owned(),
        }
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
    fn input_aggregation_keeps_every_distinct_reason_before_provider_deduplication() {
        let builder = InputOrigin::BuilderTool {
            selection: stone_recipe::derivation::PackageInputSelection::Package,
            index: 0,
        };
        let check = InputOrigin::Check {
            selection: stone_recipe::derivation::PackageInputSelection::Package,
            index: 0,
        };
        let aggregated = aggregate_inputs(vec![
            build::root::UnresolvedInput {
                request: "binary(shared)".to_owned(),
                origin: check.clone(),
            },
            build::root::UnresolvedInput {
                request: "binary(shared)".to_owned(),
                origin: builder.clone(),
            },
            build::root::UnresolvedInput {
                request: "binary(shared)".to_owned(),
                origin: builder.clone(),
            },
        ]);

        assert_eq!(
            aggregated,
            [RequestedInput {
                request: "binary(shared)".to_owned(),
                origins: vec![builder, check],
            }]
        );
    }

    #[test]
    fn authored_git_clone_dir_and_digest_reach_the_frozen_plan() {
        const COMMIT: &str = "0123456789abcdef0123456789abcdef01234567";
        const DIGEST: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
        const URL: &str = "https://example.invalid/source.git";

        let root = tempfile::tempdir().unwrap();
        fs::write(
            root.path().join("stone.glu"),
            format!(
                r#"let b = import! cast.package.v3
let base = b.mk_package (b.meta {{
    pname = "example",
    version = "1.0.0",
    release = 1,
    homepage = "https://example.invalid",
    license = ["MPL-2.0"],
}})
{{
    sources = [b.source.git_with {{
        url = "{URL}",
        git_ref = "main",
        clone_dir = b.optional.set "chosen-source",
    }}],
    .. base
}}"#
            ),
        )
        .unwrap();
        let lock =
            SourceLock::new(vec![SourceResolution::Git(crate::source_lock::GitResolution {
                order: 0,
                url: URL.to_owned(),
                requested_ref: "main".to_owned(),
                commit: COMMIT.to_owned(),
                materialization_sha256: DIGEST.to_owned(),
            })]);
        fs::write(
            root.path().join(SOURCE_LOCK_FILE_NAME),
            GluonSourceLockCodec::default().encode(&lock).unwrap(),
        )
        .unwrap();

        let recipe = crate::recipe::Recipe::load(root.path()).unwrap();
        let sources = freeze_sources(&recipe);

        assert!(matches!(
            &sources[..],
            [LockedSource::Git {
                commit,
                materialization_sha256,
                directory,
                ..
            }] if commit == COMMIT
                && materialization_sha256 == DIGEST
                && directory == "chosen-source"
        ));
    }

    #[test]
    fn cast_implementation_changes_executor_identity() {
        assert_ne!(
            executor_fingerprint("1", "semantic-a"),
            executor_fingerprint("2", "semantic-a")
        );
        assert_ne!(
            executor_fingerprint("1", "semantic-a"),
            executor_fingerprint("1", "semantic-b")
        );
    }

    #[test]
    fn typed_policy_changes_toolchain_identity() {
        assert_ne!(
            toolchain_fingerprint("llvm", "policy-a"),
            toolchain_fingerprint("llvm", "policy-b")
        );
    }

    fn sample_structural_builder() -> BuilderSpec {
        BuilderSpec {
            required_tools: vec![
                DependencySpec::Binary("cmake".to_owned()),
                DependencySpec::Binary("ninja".to_owned()),
            ],
            environment: vec![BuilderEnvironmentSpec::CMake],
            phases: stone_recipe::package::PhasesSpec {
                setup: PhaseSpec::new([
                    StepSpec::CMakeConfigure {
                        flags: vec!["-DBUILD_TESTS=ON".to_owned()],
                    },
                    package_shell("printf configured"),
                ]),
                build: PhaseSpec::new([StepSpec::CMakeBuild]),
                install: PhaseSpec::new([StepSpec::CMakeInstall]),
                check: PhaseSpec::new([StepSpec::CMakeTest]),
                workload: PhaseSpec::default(),
            },
            supported_hooks: stone_recipe::package::SupportedHooksSpec::all(),
        }
    }

    fn sample_hooks() -> HooksSpec {
        HooksSpec {
            pre_build: vec![package_shell("printf pre")],
            post_install: vec![package_shell("printf post")],
            ..HooksSpec::default()
        }
    }

    #[test]
    fn selected_builder_fingerprint_binds_complete_structure_hooks_and_profile() {
        let builder = sample_structural_builder();
        let hooks = sample_hooks();
        let original = structural_builder_fingerprint(&builder, &hooks, Some("emul32/x86_64"));
        assert_eq!(
            original,
            structural_builder_fingerprint(&builder.clone(), &hooks.clone(), Some("emul32/x86_64"))
        );
        assert_eq!(structural_builder_name(&builder), "cast.builders.cmake.v2");

        let builder_mutations: Vec<(&str, Box<dyn Fn(&mut BuilderSpec)>)> = vec![
            (
                "required-tool",
                Box::new(|builder| builder.required_tools[0] = DependencySpec::Binary("cmake-next".to_owned())),
            ),
            (
                "required-tool-kind",
                Box::new(|builder| {
                    builder.required_tools[0] = DependencySpec::SystemBinary("cmake".to_owned());
                }),
            ),
            (
                "required-tool-order",
                Box::new(|builder| builder.required_tools.reverse()),
            ),
            (
                "environment",
                Box::new(|builder| builder.environment[0] = BuilderEnvironmentSpec::Meson),
            ),
            (
                "phase-arguments",
                Box::new(|builder| {
                    let StepSpec::CMakeConfigure { flags } = &mut builder.phases.setup.steps[0] else {
                        unreachable!();
                    };
                    flags.push("-DENABLE_EXTRA=ON".to_owned());
                }),
            ),
            (
                "phase-step-order",
                Box::new(|builder| builder.phases.setup.steps.reverse()),
            ),
            (
                "phase-membership",
                Box::new(|builder| {
                    builder.phases.workload.steps.push(StepSpec::CMakeBuild);
                }),
            ),
            (
                "supported-hooks",
                Box::new(|builder| builder.supported_hooks.workload = false),
            ),
        ];
        for (field, mutate) in builder_mutations {
            let mut changed = builder.clone();
            mutate(&mut changed);
            assert_ne!(
                original,
                structural_builder_fingerprint(&changed, &hooks, Some("emul32/x86_64")),
                "{field} was not fingerprinted"
            );
        }

        let mut changed_hooks = hooks.clone();
        let StepSpec::Shell { script, .. } = &mut changed_hooks.pre_build[0] else {
            unreachable!();
        };
        script.push_str(" changed");
        assert_ne!(
            original,
            structural_builder_fingerprint(&builder, &changed_hooks, Some("emul32/x86_64"))
        );
        assert_ne!(
            original,
            structural_builder_fingerprint(&builder, &hooks, Some("emul32"))
        );
        assert_ne!(original, structural_builder_fingerprint(&builder, &hooks, None));
    }

    #[test]
    fn builder_name_is_explanatory_not_the_identity() {
        let builder = sample_structural_builder();
        let hooks = sample_hooks();
        let mut changed = builder.clone();
        let StepSpec::CMakeConfigure { flags } = &mut changed.phases.setup.steps[0] else {
            unreachable!();
        };
        flags.push("-DCHANGED=ON".to_owned());

        assert_eq!(structural_builder_name(&builder), structural_builder_name(&changed));
        assert_ne!(
            structural_builder_fingerprint(&builder, &hooks, None),
            structural_builder_fingerprint(&changed, &hooks, None)
        );
        assert_eq!(structural_builder_name(&BuilderSpec::default()), "custom");
    }

    #[test]
    fn mutable_package_directory_references_are_rejected_before_freeze() {
        let package_dir = "/mason/recipe/pkg";
        let job = JobPlan {
            pgo_stage: None,
            pgo_dir: None,
            build_dir: "/mason/build/x86_64".to_owned(),
            work_dir: "/mason/build/x86_64/source".to_owned(),
            phases: vec![PhasePlan {
                name: "build".to_owned(),
                pre: Vec::new(),
                steps: vec![StepPlan::Shell {
                    interpreter: executable("bash"),
                    declared_programs: vec![ExecutablePlan {
                        path: "/mason/recipe/pkg/helper".to_owned(),
                        requirement: RelationPlan {
                            kind: stone_recipe::derivation::RelationKind::PackageName,
                            name: "helper-package".to_owned(),
                        },
                    }],
                    script: "install /mason/recipe/pkg/helper /usr/bin/helper".to_owned(),
                    environment: BTreeMap::new(),
                    working_dir: "/mason/build/x86_64/source".to_owned(),
                }],
                post: Vec::new(),
            }],
        };

        assert!(jobs_use_package_directory(&[job], package_dir));
    }
}

#[cfg(any(test, feature = "delegated-fixture-test-support"))]
#[path = "planner/tests.rs"]
mod hermetic_tests;

#[cfg(feature = "delegated-fixture-test-support")]
pub(crate) fn run_delegated_execution_fixture() -> hermetic_tests::DelegatedExecutionOutcome {
    hermetic_tests::run_delegated_execution_fixture()
}
