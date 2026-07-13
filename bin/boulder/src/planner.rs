// SPDX-FileCopyrightText: 2026 AerynOS Developers
// SPDX-License-Identifier: MPL-2.0

//! Resolve and freeze a target-specific Boulder derivation plan.

use std::{
    collections::BTreeMap,
    num::{NonZeroU32, NonZeroU64, NonZeroUsize},
    path::PathBuf,
};

use fs_err as fs;
use moss::{Installation, runtime};
use sha2::{Digest, Sha256};
use stone_recipe::{
    PathKind,
    derivation::{
        AnalysisPlan, AnalysisToolchain, BUILD_LOCK_SCHEMA_VERSION, BuildLock, BuilderLayout, CollectionRulePlan,
        DerivationPlan, DerivationValidationError, ExecutionPolicy, JobPlan, LockedIdentity, LockedOutput,
        LockedOutputRef, LockedPackage, LockedRequest, LockedSource, NetworkMode, OutputPlan, OutputRelation,
        PackageIdentity, PathRuleKind, PathRulePlan, PhasePlan, Platform, RepositorySnapshot, StepPlan,
    },
    script,
    tuning::Toolchain,
};
use thiserror::Error;

use crate::{
    Env,
    build::{self, Builder},
    build_lock,
    package::Packager,
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
    pub lock_path: PathBuf,
    pub lock_outcome: Option<build_lock::WriteOutcome>,
    pub request_fingerprint: String,
    pub requested_packages: Vec<String>,
    pub policy_origins: Vec<String>,
    pub profile_fingerprints: Vec<String>,
}

pub fn plan(env: Env, request: Request) -> Result<Planned, Error> {
    if request.refresh_repositories && !request.update_lock {
        return Err(Error::RefreshRequiresUpdate);
    }
    let builder = Builder::new_with_jobs(
        &request.recipe,
        None,
        env,
        request.profile.clone(),
        request.compiler_cache,
        ".",
        NonZeroUsize::new(usize::try_from(request.jobs.get()).expect("u32 fits supported usize"))
            .expect("jobs is non-zero"),
        Some(request.source_date_epoch),
    )?;
    let target = builder
        .targets
        .iter()
        .find(|target| target.build_target.to_string() == request.target)
        .ok_or_else(|| Error::UnknownTarget {
            requested: request.target.clone(),
            available: builder
                .targets
                .iter()
                .map(|target| target.build_target.to_string())
                .collect(),
        })?;

    let packager = Packager::new(
        &builder.paths,
        &builder.recipe,
        &builder.macros,
        std::slice::from_ref(target),
        request.build_release,
    )?;
    let package_names = packager.resolved_packages().keys().cloned().collect::<Vec<_>>();
    let mut requested_packages = build::root::packages(&builder)
        .into_iter()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    for package in packager.resolved_packages().values() {
        requested_packages.extend(
            package
                .run_deps
                .iter()
                .filter(|dependency| !package_names.contains(dependency))
                .cloned(),
        );
    }
    requested_packages.sort();
    requested_packages.dedup();

    let source_lock_bytes = match fs::read(builder.recipe.path.with_file_name(SOURCE_LOCK_FILE_NAME)) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound && builder.recipe.parsed.upstreams.is_empty() => {
            Vec::new()
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Err(Error::MissingSourceLock),
        Err(source) => return Err(Error::ReadSourceLock(source)),
    };
    let source_lock_digest = sha256(&source_lock_bytes);
    let profile_fingerprint = combined_profile_fingerprint(&builder.profile_fingerprints);
    let toolchain_name = match builder.recipe.parsed.options.toolchain {
        Toolchain::Llvm => "llvm",
        Toolchain::Gnu => "gnu",
    };
    let boulder_version = tools_buildinfo::get_simple_version();
    let jobs = request.jobs.to_string();
    let builder_fingerprint = hash_fields([
        "boulder-executor-v1",
        boulder_version.as_str(),
        target.policy.fingerprint.sha256.as_str(),
    ]);
    let target_name = target.build_target.to_string();
    let request_fingerprint = hash_fields(
        [
            "boulder-build-lock-request-v2",
            builder.recipe.fingerprint.sha256.as_str(),
            source_lock_digest.as_str(),
            target_name.as_str(),
            target.policy.fingerprint.sha256.as_str(),
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
            target,
            &requested_packages,
            &request_fingerprint,
            &profile_fingerprint,
            toolchain_name,
            &builder_fingerprint,
            request.refresh_repositories,
        )?;
        let outcome = build_lock::write(&lock_path, &lock)?;
        (lock, Some(outcome))
    } else {
        (build_lock::require_current(&lock_path, &request_fingerprint)?, None)
    };

    let jobs = freeze_jobs(target)?;
    let outputs = freeze_outputs(
        &builder.recipe.parsed.source.name,
        packager.resolved_packages(),
        &build_lock,
    )?;
    let mut plan = DerivationPlan::new(
        PackageIdentity {
            name: builder.recipe.parsed.source.name.clone(),
            version: builder.recipe.parsed.source.version.clone(),
            source_release: builder.recipe.parsed.source.release,
            build_release: request.build_release.get(),
            homepage: builder.recipe.parsed.source.homepage.clone(),
            licenses: builder.recipe.parsed.source.license.clone(),
            architecture: artifact_architecture(target.build_target)?.to_string(),
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
        package_dir: builder.paths.recipe().guest.join("pkg").display().to_string(),
    };
    plan.execution = ExecutionPolicy {
        network: if builder.recipe.parsed.options.networking {
            NetworkMode::Enabled
        } else {
            NetworkMode::Disabled
        },
        compiler_cache: request.compiler_cache,
        jobs: request.jobs.get(),
    };
    plan.tuning = builder
        .recipe
        .parsed
        .tuning
        .iter()
        .map(|entry| format!("{}={:?}", entry.key, entry.value))
        .collect();
    plan.analyzers = vec![LockedIdentity {
        name: "boulder-package-analysis".to_owned(),
        fingerprint: tools_buildinfo::get_simple_version(),
    }];
    plan.analysis = AnalysisPlan {
        toolchain: match builder.recipe.parsed.options.toolchain {
            Toolchain::Llvm => AnalysisToolchain::Llvm,
            Toolchain::Gnu => AnalysisToolchain::Gnu,
        },
        debug: builder.recipe.parsed.options.debug,
        strip: builder.recipe.parsed.options.strip,
        compress_man: builder.recipe.parsed.options.compressman,
        remove_libtool: builder.recipe.parsed.options.lastrip,
    };
    plan.manifest_build_inputs = builder
        .recipe
        .parsed
        .build
        .build_deps
        .iter()
        .chain(&builder.recipe.parsed.build.check_deps)
        .cloned()
        .collect();
    plan.collection_rules = packager
        .collection_rules()
        .map(|(package, kind, pattern)| CollectionRulePlan {
            output: output_name(&builder.recipe.parsed.source.name, package),
            kind: path_rule_kind(kind),
            pattern: pattern.to_owned(),
        })
        .collect();
    plan.outputs = outputs;
    plan.source_date_epoch = request.source_date_epoch;
    plan.validate()?;
    let policy_origins = target
        .policy
        .changes
        .iter()
        .map(|change| change.origin.clone())
        .collect();
    let profile_fingerprints = builder
        .profile_fingerprints
        .iter()
        .map(|fingerprint| fingerprint.sha256.clone())
        .collect();

    Ok(Planned {
        plan,
        lock_path,
        lock_outcome,
        request_fingerprint,
        requested_packages,
        policy_origins,
        profile_fingerprints,
    })
}

fn resolve_build_lock(
    builder: &Builder,
    target: &build::Target,
    requested: &[String],
    request_fingerprint: &str,
    profile_fingerprint: &str,
    toolchain_name: &str,
    builder_fingerprint: &str,
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
    let snapshots = client
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
            outputs: vec![LockedOutput {
                name: "out".to_owned(),
                id: resolved.package.id.to_string(),
            }],
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
    let requests = closure
        .requests
        .into_iter()
        .map(|request| LockedRequest {
            request: request.request,
            package_id: request.package.to_string(),
            output: "out".to_owned(),
        })
        .collect::<Vec<_>>();
    let target_platform = platform(target.build_target.to_string());
    let build_platform = platform(crate::architecture::host().to_string());
    let host_platform = platform(target.build_target.to_string());
    let mut lock = BuildLock {
        schema_version: BUILD_LOCK_SCHEMA_VERSION,
        request_fingerprint: request_fingerprint.to_owned(),
        base_state: hash_fields(packages.iter().map(|package| package.package_id.as_str())),
        repositories: snapshots,
        requests,
        packages,
        build_platform,
        host_platform,
        target_platform,
        policy: LockedIdentity {
            name: target.policy.target.clone(),
            fingerprint: target.policy.fingerprint.sha256.clone(),
        },
        profile: LockedIdentity {
            name: builder.profile.to_string(),
            fingerprint: profile_fingerprint.to_owned(),
        },
        toolchain: LockedIdentity {
            name: toolchain_name.to_owned(),
            fingerprint: hash_fields([toolchain_name, target.policy.fingerprint.sha256.as_str()]),
        },
        builder: LockedIdentity {
            name: EXECUTOR_ABI.to_owned(),
            fingerprint: builder_fingerprint.to_owned(),
        },
    };
    lock.normalize();
    lock.validate()?;
    Ok(lock)
}

fn freeze_jobs(target: &build::Target) -> Result<Vec<JobPlan>, Error> {
    let mut jobs = Vec::new();
    for job in &target.jobs {
        let mut phases = Vec::new();
        for (phase, script) in &job.phases {
            let mut steps = Vec::new();
            let working_dir = if matches!(phase, build::job::Phase::Prepare) {
                &job.build_dir
            } else {
                &job.work_dir
            };
            for command in &script.commands {
                match command {
                    script::Command::Content(content) => steps.push(StepPlan::Shell {
                        interpreter: "/usr/bin/bash".to_owned(),
                        script: content.clone(),
                        environment: BTreeMap::new(),
                        working_dir: working_dir.display().to_string(),
                    }),
                    script::Command::Break(_) => {
                        return Err(Error::InteractiveBreakpoint {
                            phase: phase.to_string(),
                        });
                    }
                }
            }
            phases.push(PhasePlan {
                name: phase.to_string(),
                pre: Vec::new(),
                steps,
                post: Vec::new(),
            });
        }
        jobs.push(JobPlan {
            pgo_stage: job.pgo_stage.map(|stage| format!("{stage:?}").to_lowercase()),
            build_dir: job.build_dir.display().to_string(),
            work_dir: job.work_dir.display().to_string(),
            phases,
        });
    }
    Ok(jobs)
}

fn freeze_outputs(
    root_name: &str,
    packages: &BTreeMap<String, stone_recipe::Package>,
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
                .run_deps
                .iter()
                .map(|dependency| {
                    if let Some(output) = names.get(dependency) {
                        Ok(OutputRelation::Planned { output: output.clone() })
                    } else if let Some(request) = lock.requests.iter().find(|request| request.request == *dependency) {
                        Ok(OutputRelation::Locked {
                            request: request.request.clone(),
                            reference: LockedOutputRef {
                                package_id: request.package_id.clone(),
                                output: request.output.clone(),
                            },
                        })
                    } else {
                        Err(Error::UnlockedRuntimeDependency {
                            package: name.clone(),
                            dependency: dependency.clone(),
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
                runtime_exclude: package.run_deps_exclude.clone(),
                paths: package
                    .paths
                    .iter()
                    .map(|path| PathRulePlan {
                        kind: match path.kind {
                            PathKind::Any => PathRuleKind::Any,
                            PathKind::Exe => PathRuleKind::Executable,
                            PathKind::Symlink => PathRuleKind::Symlink,
                            PathKind::Special => PathRuleKind::Special,
                        },
                        pattern: path.path.clone(),
                    })
                    .collect(),
                runtime_inputs,
                conflicts: package.conflicts.clone(),
            })
        })
        .collect()
}

fn path_rule_kind(kind: PathKind) -> PathRuleKind {
    match kind {
        PathKind::Any => PathRuleKind::Any,
        PathKind::Exe => PathRuleKind::Executable,
        PathKind::Symlink => PathRuleKind::Symlink,
        PathKind::Special => PathRuleKind::Special,
    }
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
                            .parsed
                            .upstreams
                            .get(source.order as usize)
                            .and_then(|upstream| match &upstream.props {
                                stone_recipe::upstream::Props::Plain { rename, .. } => rename.clone(),
                                stone_recipe::upstream::Props::Git { .. } => None,
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

fn platform(architecture: impl Into<String>) -> Platform {
    Platform {
        architecture: architecture.into(),
        vendor: "unknown".to_owned(),
        operating_system: "linux".to_owned(),
        abi: "gnu".to_owned(),
    }
}

fn artifact_architecture(target: crate::architecture::BuildTarget) -> Result<crate::Architecture, Error> {
    match target {
        crate::architecture::BuildTarget::Native(architecture) => Ok(architecture),
        crate::architecture::BuildTarget::Emul32(crate::Architecture::X86_64 | crate::Architecture::X86) => {
            Ok(crate::Architecture::X86)
        }
        crate::architecture::BuildTarget::Emul32(architecture) => {
            Err(Error::UnsupportedEmul32ArtifactArchitecture(architecture.to_string()))
        }
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
    #[error("unknown build target `{requested}`; available targets: {}", available.join(", "))]
    UnknownTarget { requested: String, available: Vec<String> },
    #[error("`--refresh-repositories` requires `--update-lock`")]
    RefreshRequiresUpdate,
    #[error("sources.lock.glu is required when a recipe declares sources")]
    MissingSourceLock,
    #[error("read sources.lock.glu")]
    ReadSourceLock(#[source] std::io::Error),
    #[error("interactive breakpoint in phase `{phase}` cannot be frozen into a derivation plan")]
    InteractiveBreakpoint { phase: String },
    #[error("output package `{package}` has runtime dependency `{dependency}` absent from the locked closure")]
    UnlockedRuntimeDependency { package: String, dependency: String },
    #[error("no emitted Stone architecture mapping for emul32/{0}")]
    UnsupportedEmul32ArtifactArchitecture(String),
}

impl From<build::Error> for Error {
    fn from(error: build::Error) -> Self {
        Self::Builder(Box::new(error))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::architecture::BuildTarget;

    #[test]
    fn emitted_architecture_mapping_is_explicit_for_emul32() {
        assert_eq!(
            artifact_architecture(BuildTarget::Native(crate::Architecture::X86_64)).unwrap(),
            crate::Architecture::X86_64
        );
        assert_eq!(
            artifact_architecture(BuildTarget::Emul32(crate::Architecture::X86_64)).unwrap(),
            crate::Architecture::X86
        );
        assert!(artifact_architecture(BuildTarget::Emul32(crate::Architecture::Aarch64)).is_err());
    }
}
