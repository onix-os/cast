// SPDX-FileCopyrightText: 2026 AerynOS Developers
// SPDX-License-Identifier: MPL-2.0

//! Resolve and freeze a target-specific Boulder derivation plan.

use std::{
    collections::{BTreeMap, BTreeSet},
    num::{NonZeroU32, NonZeroU64, NonZeroUsize},
    path::{Path, PathBuf},
};

use fs_err as fs;
use moss::{Installation, runtime};
use sha2::{Digest, Sha256};
use stone_recipe::derivation::{
    AnalysisPlan, AnalysisToolchain, BUILD_LOCK_SCHEMA_VERSION, BuildLock, BuilderLayout, CollectionRulePlan,
    DerivationPlan, DerivationValidationError, ExecutionPolicy, JobPlan, LockedIdentity, LockedOutput, LockedOutputRef,
    LockedPackage, LockedRequest, LockedSource, NetworkMode, OutputPlan, OutputRelation, PackageIdentity, Platform,
    RelationPlan, RepositorySnapshot, StepPlan,
};
use thiserror::Error;

use crate::{
    Env,
    build::{self, Builder},
    build_lock,
    package::{Packager, ResolvedOutput},
    profile,
    source_lock::{SOURCE_LOCK_FILE_NAME, SourceResolution},
};

pub(crate) const EXECUTOR_ABI: &str = "boulder-executor-v1";

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
    pub request_fingerprint: String,
    pub requested_packages: Vec<String>,
    pub policy_provenance: Vec<crate::policy::PolicySource>,
    pub policy_changes: Vec<crate::policy::PolicyChange>,
    pub profile_fingerprints: Vec<String>,
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
    let builder = Builder::new_with_jobs(
        &request.recipe,
        env,
        request.profile.clone(),
        request.compiler_cache,
        output_dir,
        NonZeroUsize::new(usize::try_from(request.jobs.get()).expect("u32 fits supported usize"))
            .expect("jobs is non-zero"),
        Some(request.source_date_epoch),
        &request.target,
    )?;
    let target = &builder.target;
    let target_policy = &target.target_policy;
    let target_name = &target_policy.name;

    let packager = Packager::new(&builder.paths, &builder.recipe)?;
    let package_names = packager.resolved_packages().keys().cloned().collect::<Vec<_>>();
    let mut requested_packages = build::root::packages(&builder)?;
    for package in packager.resolved_packages().values() {
        requested_packages.extend(
            package
                .runtime_inputs
                .iter()
                .map(|dependency| dependency.to_name())
                .filter(|dependency| !package_names.contains(dependency)),
        );
    }
    requested_packages.sort();
    requested_packages.dedup();

    let source_lock_bytes = match fs::read(builder.recipe.path.with_file_name(SOURCE_LOCK_FILE_NAME)) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound && builder.recipe.declaration.sources.is_empty() => {
            Vec::new()
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Err(Error::MissingSourceLock),
        Err(source) => return Err(Error::ReadSourceLock(source)),
    };
    let source_lock_digest = sha256(&source_lock_bytes);
    let profile_fingerprint = combined_profile_fingerprint(&builder.profile_fingerprints);
    let toolchain_name = match builder.recipe.declaration.options.toolchain {
        stone_recipe::ToolchainSpec::Llvm => "llvm",
        stone_recipe::ToolchainSpec::Gnu => "gnu",
    };
    let boulder_version = tools_buildinfo::get_simple_version();
    let jobs = request.jobs.to_string();
    let builder_fingerprint = builder_fingerprint(&boulder_version, &target.build_policy.fingerprint.sha256);
    let expected_lock = build_lock::ExpectedBuildLockContext {
        requested_providers: requested_packages.clone(),
        build_platform: platform(&target_policy.build_platform),
        host_platform: platform(&target_policy.host_platform),
        target_platform: platform(&target_policy.target_platform),
        policy: LockedIdentity {
            name: target.build_policy.name.clone(),
            fingerprint: target.build_policy.fingerprint.sha256.clone(),
        },
        target: LockedIdentity {
            name: target_name.clone(),
            fingerprint: target_fingerprint(target_name, &target.build_policy.fingerprint.sha256),
        },
        profile: LockedIdentity {
            name: builder.profile.to_string(),
            fingerprint: profile_fingerprint.clone(),
        },
        toolchain: LockedIdentity {
            name: toolchain_name.to_owned(),
            fingerprint: toolchain_fingerprint(toolchain_name, &target.build_policy.fingerprint.sha256),
        },
        builder: LockedIdentity {
            name: EXECUTOR_ABI.to_owned(),
            fingerprint: builder_fingerprint.clone(),
        },
    };
    let request_fingerprint = hash_fields(
        [
            "boulder-build-lock-request-v2",
            builder.recipe.fingerprint.sha256.as_str(),
            source_lock_digest.as_str(),
            target_name.as_str(),
            target.build_policy.fingerprint.sha256.as_str(),
            profile_fingerprint.as_str(),
            toolchain_name,
            builder_fingerprint.as_str(),
            jobs.as_str(),
        ]
        .into_iter()
        .chain(requested_packages.iter().map(String::as_str)),
    );

    let lock_path = build_lock::path_for_recipe(&builder.recipe.path);
    let (build_lock, lock_outcome) = if request.update_lock {
        let lock = resolve_build_lock(
            &builder,
            &requested_packages,
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
    let package_dir = builder.paths.recipe().guest.join("pkg").display().to_string();
    if jobs_use_package_directory(&jobs, &package_dir) {
        return Err(Error::UnsupportedPackageDirectoryInput { package_dir });
    }
    let outputs = freeze_outputs(
        &builder.recipe.declaration.meta.pname,
        packager.resolved_packages(),
        &build_lock,
    )?;
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
    );
    plan.boulder_version = boulder_version;
    plan.recipe_fingerprint = builder.recipe.fingerprint.sha256.clone();
    plan.source_lock_digest = source_lock_digest;
    plan.sources = freeze_sources(&builder.recipe);
    plan.jobs = jobs;
    plan.environment = BTreeMap::from([
        ("HOME".to_owned(), target.jobs[0].build_dir.display().to_string()),
        ("PATH".to_owned(), "/usr/bin:/usr/sbin".to_owned()),
    ]);
    plan.layout = BuilderLayout {
        build_dir: builder.paths.build().guest.display().to_string(),
        source_dir: builder.paths.upstreams().guest.display().to_string(),
        install_dir: builder.paths.install().guest.display().to_string(),
        package_dir,
    };
    plan.execution = ExecutionPolicy {
        network: if builder.recipe.declaration.options.networking {
            NetworkMode::Enabled
        } else {
            NetworkMode::Disabled
        },
        compiler_cache: request.compiler_cache,
        jobs: request.jobs.get(),
    };
    plan.tuning = builder
        .recipe
        .declaration
        .tuning
        .iter()
        .map(|entry| format!("{}={:?}", entry.key, entry.value))
        .collect();
    plan.analyzers = vec![LockedIdentity {
        name: "boulder-package-analysis".to_owned(),
        fingerprint: tools_buildinfo::get_simple_version(),
    }];
    plan.analysis = AnalysisPlan {
        toolchain: match builder.recipe.declaration.options.toolchain {
            stone_recipe::ToolchainSpec::Llvm => AnalysisToolchain::Llvm,
            stone_recipe::ToolchainSpec::Gnu => AnalysisToolchain::Gnu,
        },
        debug: builder.recipe.declaration.options.debug,
        strip: builder.recipe.declaration.options.strip,
        compress_man: builder.recipe.declaration.options.compressman,
        remove_libtool: builder.recipe.declaration.options.lastrip,
    };
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
    let policy_provenance = target.build_policy.sources();
    let policy_changes = target.build_policy.changes().to_vec();
    let profile_fingerprints = builder
        .profile_fingerprints
        .iter()
        .map(|fingerprint| fingerprint.sha256.clone())
        .collect();
    let runtime = builder.into_runtime();

    Ok(Planned {
        plan,
        runtime,
        lock_path,
        lock_outcome,
        request_fingerprint,
        requested_packages,
        policy_provenance,
        policy_changes,
        profile_fingerprints,
    })
}

fn resolve_build_lock(
    builder: &Builder,
    requested: &[String],
    request_fingerprint: &str,
    expected: &build_lock::ExpectedBuildLockContext,
    refresh: bool,
) -> Result<BuildLock, Error> {
    let installation = Installation::open(&builder.env.moss_dir, None)?;
    let mut client = moss::Client::builder("boulder-plan", installation)
        .repositories(builder.repositories().clone())
        .build()?;
    if refresh {
        runtime::block_on(client.refresh_repositories())?;
    } else {
        runtime::block_on(client.ensure_repos_initialized())?;
    }
    let references = requested.iter().map(String::as_str).collect::<Vec<_>>();
    let closure = client.resolve_available_closure(&references)?;
    let mut snapshots = client
        .repository_index_snapshots()?
        .into_iter()
        .map(|snapshot| RepositorySnapshot {
            id: snapshot.id.to_string(),
            index_uri: snapshot.index_uri.to_string(),
            snapshot: snapshot.sha256,
        })
        .collect::<Vec<_>>();
    let packages = closure
        .packages
        .iter()
        .map(|resolved| LockedPackage {
            package_id: resolved.package.id.to_string(),
            name: resolved.package.meta.name.to_string(),
            version: format!(
                "{}-{}-{}",
                resolved.package.meta.version_identifier,
                resolved.package.meta.source_release,
                resolved.package.meta.build_release
            ),
            architecture: resolved.package.meta.architecture.clone(),
            repository: resolved.repository.to_string(),
            outputs: vec![LockedOutput { name: "out".to_owned() }],
            dependencies: resolved
                .dependencies
                .iter()
                .map(|dependency| LockedOutputRef {
                    package_id: dependency.to_string(),
                    output: "out".to_owned(),
                })
                .collect(),
        })
        .collect::<Vec<_>>();
    let used_repositories = packages
        .iter()
        .map(|package| package.repository.as_str())
        .collect::<BTreeSet<_>>();
    snapshots.retain(|snapshot| used_repositories.contains(snapshot.id.as_str()));
    let requests = closure
        .requests
        .into_iter()
        .map(|request| LockedRequest {
            request: request.request,
            package_id: request.package.to_string(),
            output: "out".to_owned(),
        })
        .collect::<Vec<_>>();
    let mut lock = BuildLock {
        schema_version: BUILD_LOCK_SCHEMA_VERSION,
        request_fingerprint: request_fingerprint.to_owned(),
        repositories: snapshots,
        requests,
        packages,
        build_platform: expected.build_platform.clone(),
        host_platform: expected.host_platform.clone(),
        target_platform: expected.target_platform.clone(),
        policy: expected.policy.clone(),
        target: expected.target.clone(),
        profile: expected.profile.clone(),
        toolchain: expected.toolchain.clone(),
        builder: expected.builder.clone(),
    };
    lock.normalize();
    lock.validate()?;
    Ok(lock)
}

fn freeze_jobs(target: &build::Target) -> Result<Vec<JobPlan>, Error> {
    Ok(target
        .jobs
        .iter()
        .map(|job| JobPlan {
            pgo_stage: job.pgo_stage.map(|stage| format!("{stage:?}").to_lowercase()),
            pgo_dir: job.pgo_stage.map(|_| format!("{}-pgo", job.build_dir.display())),
            build_dir: job.build_dir.display().to_string(),
            work_dir: job.work_dir.display().to_string(),
            phases: job.phases.values().cloned().collect(),
        })
        .collect())
}

fn jobs_use_package_directory(jobs: &[JobPlan], package_dir: &str) -> bool {
    jobs.iter()
        .flat_map(|job| &job.phases)
        .flat_map(|phase| phase.pre.iter().chain(&phase.steps).chain(&phase.post))
        .any(|step| match step {
            StepPlan::Run {
                program,
                args,
                environment,
                working_dir,
            } => std::iter::once(program)
                .chain(args)
                .chain(environment.values())
                .chain(std::iter::once(working_dir))
                .any(|value| value.contains(package_dir)),
            StepPlan::Shell {
                interpreter,
                script,
                environment,
                working_dir,
            } => std::iter::once(interpreter)
                .chain(std::iter::once(script))
                .chain(environment.values())
                .chain(std::iter::once(working_dir))
                .any(|value| value.contains(package_dir)),
        })
}

fn freeze_outputs(
    root_name: &str,
    packages: &BTreeMap<String, ResolvedOutput>,
    lock: &BuildLock,
) -> Result<Vec<OutputPlan>, Error> {
    let names = packages
        .keys()
        .map(|name| (name.clone(), output_name(root_name, name)))
        .collect::<BTreeMap<_, _>>();
    packages
        .iter()
        .map(|(name, package)| {
            let runtime_inputs = package
                .runtime_inputs
                .iter()
                .map(|dependency| {
                    let request_name = dependency.to_name();
                    if let Some(output) = names.get(&request_name) {
                        Ok(OutputRelation::Planned { output: output.clone() })
                    } else if let Some(request) = lock.requests.iter().find(|request| request.request == request_name) {
                        Ok(OutputRelation::Locked {
                            relation: RelationPlan::from(dependency),
                            reference: LockedOutputRef {
                                package_id: request.package_id.clone(),
                                output: request.output.clone(),
                            },
                        })
                    } else {
                        Err(Error::UnlockedRuntimeDependency {
                            package: name.clone(),
                            dependency: request_name,
                        })
                    }
                })
                .collect::<Result<Vec<_>, _>>()?;
            Ok(OutputPlan {
                name: names[name].clone(),
                package_name: name.clone(),
                summary: package.summary.clone(),
                description: package.description.clone(),
                provides_exclude: package.provides_exclude.clone(),
                runtime_exclude: package.runtime_exclude.clone(),
                runtime_inputs,
                conflicts: package.conflicts.iter().map(RelationPlan::from).collect(),
            })
        })
        .collect()
}

fn output_name(root: &str, package: &str) -> String {
    if package == root {
        "out".to_owned()
    } else {
        package.strip_prefix(&format!("{root}-")).unwrap_or(package).to_owned()
    }
}

fn freeze_sources(recipe: &crate::Recipe) -> Vec<LockedSource> {
    recipe
        .source_lock
        .as_ref()
        .map(|lock| {
            lock.sources
                .iter()
                .map(|source| match source {
                    SourceResolution::Archive(source) => LockedSource::Archive {
                        order: source.order,
                        url: source.url.clone(),
                        sha256: source.sha256.clone(),
                        filename: recipe
                            .declaration
                            .sources
                            .get(source.order as usize)
                            .and_then(|upstream| match upstream {
                                stone_recipe::UpstreamSpec::Archive { rename, .. } => rename.clone(),
                                stone_recipe::UpstreamSpec::Git { .. } => None,
                            })
                            .unwrap_or_else(|| {
                                url::Url::parse(&source.url)
                                    .map(|url| moss::util::uri_file_name(&url).to_owned())
                                    .unwrap_or_default()
                            }),
                    },
                    SourceResolution::Git(source) => LockedSource::Git {
                        order: source.order,
                        url: source.url.clone(),
                        requested_ref: source.requested_ref.clone(),
                        commit: source.commit.clone(),
                        directory: url::Url::parse(&source.url)
                            .map(|url| moss::util::uri_file_name(&url).to_owned())
                            .unwrap_or_default(),
                    },
                })
                .collect()
        })
        .unwrap_or_default()
}

fn combined_profile_fingerprint(fingerprints: &[gluon_config::EvaluationFingerprint]) -> String {
    hash_fields(
        std::iter::once("boulder-profile-fragments-v1")
            .chain(fingerprints.iter().map(|fingerprint| fingerprint.sha256.as_str())),
    )
}

fn builder_fingerprint(boulder_version: &str, policy_fingerprint: &str) -> String {
    hash_fields([EXECUTOR_ABI, boulder_version, policy_fingerprint])
}

fn toolchain_fingerprint(toolchain: &str, policy_fingerprint: &str) -> String {
    hash_fields([toolchain, policy_fingerprint])
}

fn target_fingerprint(target: &str, policy_fingerprint: &str) -> String {
    // The composed policy fingerprint binds the complete target value; the
    // exact target name selects one member of that validated catalog.
    hash_fields(["boulder-target-selection-v1", policy_fingerprint, target])
}

fn platform(policy: &stone_recipe::build_policy::PlatformPolicySpec) -> Platform {
    Platform {
        architecture: policy.architecture.clone(),
        vendor: policy.vendor.clone(),
        operating_system: policy.operating_system.clone(),
        abi: policy.abi.clone(),
    }
}

fn hash_fields<'a>(fields: impl IntoIterator<Item = &'a str>) -> String {
    let mut digest = Sha256::new();
    for field in fields {
        digest.update((field.len() as u64).to_le_bytes());
        digest.update(field.as_bytes());
    }
    format!("{:x}", digest.finalize())
}

fn sha256(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
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
    MossClient(#[from] moss::client::Error),
    #[error(transparent)]
    MossResolve(#[from] moss::client::ResolveError),
    #[error(transparent)]
    MossInstallation(#[from] moss::installation::Error),
    #[error("`--refresh-repositories` requires `--update-lock`")]
    RefreshRequiresUpdate,
    #[error("sources.lock.glu is required when a recipe declares sources")]
    MissingSourceLock,
    #[error("read sources.lock.glu")]
    ReadSourceLock(#[source] std::io::Error),
    #[error("output package `{package}` has runtime dependency `{dependency}` absent from the locked closure")]
    UnlockedRuntimeDependency { package: String, dependency: String },
    #[error("frozen execution does not support mutable package-directory input {package_dir}")]
    UnsupportedPackageDirectoryInput { package_dir: String },
}

impl From<build::Error> for Error {
    fn from(error: build::Error) -> Self {
        Self::Builder(Box::new(error))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use stone_recipe::derivation::PhasePlan;

    #[test]
    fn typed_policy_changes_builder_and_toolchain_identities() {
        assert_ne!(
            builder_fingerprint("1", "policy-a"),
            builder_fingerprint("1", "policy-b")
        );
        assert_ne!(
            toolchain_fingerprint("llvm", "policy-a"),
            toolchain_fingerprint("llvm", "policy-b")
        );
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
                    interpreter: "/usr/bin/bash".to_owned(),
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
