// SPDX-FileCopyrightText: 2024 AerynOS Developers
// SPDX-License-Identifier: MPL-2.0

use std::path::PathBuf;

use forge::{FrozenRootGuard, Installation, MaterializedFrozenRoot, package, repository};
use stone_recipe::{
    ToolchainSpec,
    build_policy::{
        AnalyzerKind, BuildCommandSpec, BuildPolicySpec, BuildProgramSpec, BuildToolSpec, CompilerToolsSpec,
        TargetEmulationSpec, TargetPolicySpec,
    },
    derivation::{
        AnalyzerRole, BuildLock, CompilerCacheRole, CompilerExecutableRole, DerivationPlan, ExecutablePlan,
        InputOrigin, JobExecutableRole, JobStepSection, LockedPackage, PackageInputSelection, RelationPlan,
        RepositorySnapshot, StepPlan,
    },
    package::{DependencySpec, PackageSpec},
};
use thiserror::Error;

use crate::build::{Builder, job::Job};
use crate::{Timing, timing};

/// A materialized root whose final root-visible inputs have not yet been
/// verified. Runtime setup retains this client while locked sources and mount
/// targets are prepared, then consumes it to issue the activation guard as
/// the last operation before the container is constructed.
#[must_use = "a materialized frozen root must be verified before activation"]
pub struct PendingFrozenRoot {
    client: forge::Client,
    materialized_root: MaterializedFrozenRoot,
    package_ids: Vec<package::Id>,
    executable_bindings: Vec<forge::FrozenExecutableBinding>,
}

impl PendingFrozenRoot {
    pub fn materialized_root(&self) -> &MaterializedFrozenRoot {
        &self.materialized_root
    }

    pub fn verify(self) -> Result<FrozenRootGuard, Error> {
        Ok(self.client.require_materialized_frozen_executables(
            self.materialized_root,
            &self.package_ids,
            &self.executable_bindings,
        )?)
    }
}

pub fn populate_frozen(
    paths: &crate::Paths,
    forge_dir: &std::path::Path,
    repositories: repository::Map,
    plan: &DerivationPlan,
    timing: &mut Timing,
    initialize_timer: timing::Timer,
) -> Result<PendingFrozenRoot, Error> {
    let rootfs = paths.rootfs().host;
    let build_lock = &plan.build_lock;

    // Create the Forge client.
    let repositories = locked_repositories(&repositories, &build_lock.repositories)?;
    let installation = Installation::open_frozen(forge_dir, None)?;
    let mut forge_client = forge::Client::frozen(
        super::BUILD_REPOSITORY_CACHE_IDENTITY,
        installation,
        repositories,
        rootfs,
    )?;
    require_locked_repositories(&forge_client, build_lock)?;
    let package_ids = exact_package_ids(&forge_client, build_lock)?;
    let executable_bindings = frozen_executable_bindings(plan)?;

    timing.finish(initialize_timer);

    // The planner already selected the complete package closure. Installing
    // provider strings here would silently cross the freeze boundary and allow
    // a newer candidate to replace a locked package.
    // Publication is strictly no-replace. Explicitly discard a previous
    // completed workspace through Forge's bounded descriptor boundary before
    // asking it to publish the new private staging tree.
    forge_client.discard_frozen_root()?;
    let materialization = forge_client.materialize_frozen_root(&package_ids, plan.source_date_epoch)?;
    let (install_timing, materialized_root) = materialization.into_parts();

    timing.record(timing::Populate::Resolve, install_timing.resolve);
    timing.record(timing::Populate::Fetch, install_timing.fetch);
    timing.record(timing::Populate::Blit, install_timing.blit);

    Ok(PendingFrozenRoot {
        client: forge_client,
        materialized_root,
        package_ids,
        executable_bindings,
    })
}

pub fn discard_frozen(
    paths: &crate::Paths,
    forge_dir: &std::path::Path,
    repositories: repository::Map,
    plan: &DerivationPlan,
) -> Result<(), Error> {
    require_frozen_layout(paths, plan)?;
    let repositories = locked_repositories(&repositories, &plan.build_lock.repositories)?;
    let installation = Installation::open_frozen(forge_dir, None)?;
    let client = forge::Client::frozen(
        super::BUILD_REPOSITORY_CACHE_IDENTITY,
        installation,
        repositories,
        &paths.rootfs().host,
    )?;
    client.discard_frozen_root()?;
    Ok(())
}

fn require_frozen_layout(paths: &crate::Paths, plan: &DerivationPlan) -> Result<(), Error> {
    if paths.layout() == &plan.layout {
        Ok(())
    } else {
        Err(Error::FrozenSandboxLayoutMismatch)
    }
}

fn require_locked_repositories(client: &forge::Client, build_lock: &BuildLock) -> Result<(), Error> {
    let mut current = client
        .repository_index_snapshots()?
        .into_iter()
        .map(|snapshot| RepositorySnapshot {
            id: snapshot.id.to_string(),
            index_uri: snapshot.index_uri.to_string(),
            snapshot: snapshot.sha256,
        })
        .collect::<Vec<_>>();
    current.sort_by(|left, right| left.id.cmp(&right.id).then_with(|| left.snapshot.cmp(&right.snapshot)));

    if current != build_lock.repositories {
        return Err(Error::RepositorySnapshotMismatch {
            locked: build_lock.repositories.clone(),
            current,
        });
    }
    Ok(())
}

fn locked_repositories(
    configured: &repository::Map,
    locked_repositories: &[RepositorySnapshot],
) -> Result<repository::Map, Error> {
    locked_repositories
        .iter()
        .map(|locked| {
            let id = repository::Id::new(&locked.id);
            if id.to_string() != locked.id {
                return Err(Error::InvalidLockedRepositoryId(locked.id.clone()));
            }
            let repository = configured
                .get(&id)
                .cloned()
                .ok_or_else(|| Error::MissingLockedRepository(locked.id.clone()))?;
            Ok((id, repository))
        })
        .collect::<Result<repository::Map, Error>>()
}

fn exact_package_ids(client: &forge::Client, build_lock: &BuildLock) -> Result<Vec<package::Id>, Error> {
    build_lock
        .packages
        .iter()
        .map(|locked| {
            let id = package::Id::from(locked.package_id.clone());
            let package = client.resolve_package(&id)?;
            require_locked_metadata(locked, &package)?;
            Ok(id)
        })
        .collect()
}

fn frozen_executable_bindings(plan: &DerivationPlan) -> Result<Vec<forge::FrozenExecutableBinding>, Error> {
    let mut executables = Vec::<&ExecutablePlan>::new();
    executables.extend(
        plan.toolchain_commands
            .compilers
            .iter()
            .map(|compiler| &compiler.command.program),
    );
    executables.extend(
        [
            plan.toolchain_commands.ccache.as_ref(),
            plan.toolchain_commands.sccache.as_ref(),
            plan.toolchain_commands.mold.as_ref().map(|command| &command.program),
        ]
        .into_iter()
        .flatten(),
    );
    executables.extend(
        [
            plan.analysis.tools.pkg_config.as_ref(),
            plan.analysis.tools.python.as_ref(),
            plan.analysis.tools.objcopy.as_ref(),
            plan.analysis.tools.strip.as_ref(),
        ]
        .into_iter()
        .flatten(),
    );
    for phase in plan.jobs.iter().flat_map(|job| &job.phases) {
        for step in phase.pre.iter().chain(&phase.steps).chain(&phase.post) {
            match step {
                StepPlan::Run { program, .. } => executables.push(program),
                StepPlan::RunBuilt { .. } => {}
                StepPlan::Shell {
                    interpreter,
                    declared_programs,
                    ..
                } => {
                    executables.push(interpreter);
                    executables.extend(declared_programs);
                }
                StepPlan::ExtractArchive { .. } => {}
            }
        }
    }

    executables
        .into_iter()
        .map(|executable| {
            let request = executable.requirement.canonical_name();
            let mut matching = plan
                .build_lock
                .requests
                .iter()
                .filter(|locked| locked.request == request);
            let locked = matching
                .next()
                .ok_or_else(|| Error::MissingFrozenExecutableRequest(request.clone()))?;
            if matching.next().is_some() {
                return Err(Error::DuplicateFrozenExecutableRequest(request));
            }
            Ok(forge::FrozenExecutableBinding {
                package: package::Id::from(locked.package_id.clone()),
                path: PathBuf::from(&executable.path),
            })
        })
        .collect()
}

fn require_locked_metadata(locked: &LockedPackage, package: &forge::Package) -> Result<(), Error> {
    if !locked_metadata_matches(locked, package) {
        return Err(Error::LockedPackageMetadataMismatch {
            package_id: locked.package_id.clone(),
        });
    }
    Ok(())
}

fn locked_metadata_matches(locked: &LockedPackage, package: &forge::Package) -> bool {
    let version = format!(
        "{}-{}-{}",
        package.meta.version_identifier, package.meta.source_release, package.meta.build_release
    );
    package.meta.name.as_str() == locked.name
        && version == locked.version
        && package.meta.architecture == locked.architecture
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct UnresolvedInput {
    pub request: String,
    pub origin: InputOrigin,
}

pub(crate) fn inputs(builder: &Builder) -> Result<Vec<UnresolvedInput>, Error> {
    let mut inputs = inputs_for(
        &builder.target.build_policy.spec,
        &builder.target.build_policy.provenance.root.root_logical_name,
        &builder.recipe.declaration,
        builder.recipe.build_target_profile_key(&builder.target.target_policy),
        &builder.target.target_policy,
        builder.ccache,
    )?;
    extend_job_executables(&mut inputs, &builder.target.jobs)?;
    Ok(inputs)
}

fn inputs_for(
    policy: &BuildPolicySpec,
    policy_source: &str,
    package: &PackageSpec,
    profile: Option<&str>,
    target: &TargetPolicySpec,
    compiler_cache: bool,
) -> Result<Vec<UnresolvedInput>, Error> {
    let mut inputs = Vec::new();
    extend_policy_tools(&mut inputs, policy_source, "build_root.base", &policy.build_root.base)?;

    let (toolchain_field, toolchain_tools) = match package.options.toolchain {
        ToolchainSpec::Llvm => ("build_root.toolchains.llvm", &policy.build_root.toolchains.llvm),
        ToolchainSpec::Gnu => ("build_root.toolchains.gnu", &policy.build_root.toolchains.gnu),
    };
    extend_policy_tools(&mut inputs, policy_source, toolchain_field, toolchain_tools)?;
    let (compiler_field, compiler_tools) = match package.options.toolchain {
        ToolchainSpec::Llvm => ("toolchains.llvm", &policy.toolchains.llvm),
        ToolchainSpec::Gnu => ("toolchains.gnu", &policy.toolchains.gnu),
    };
    extend_compiler_commands(&mut inputs, policy_source, compiler_field, compiler_tools)?;

    if matches!(&target.emulation, TargetEmulationSpec::Emul32 { .. }) {
        extend_policy_tools(
            &mut inputs,
            policy_source,
            "build_root.emul32.base",
            &policy.build_root.emul32.base,
        )?;
        let (toolchain_field, toolchain_tools) = match package.options.toolchain {
            ToolchainSpec::Llvm => (
                "build_root.emul32.toolchains.llvm",
                &policy.build_root.emul32.toolchains.llvm,
            ),
            ToolchainSpec::Gnu => (
                "build_root.emul32.toolchains.gnu",
                &policy.build_root.emul32.toolchains.gnu,
            ),
        };
        extend_policy_tools(&mut inputs, policy_source, toolchain_field, toolchain_tools)?;
    }

    if package.mold {
        push_policy_program_input(
            &mut inputs,
            policy_source,
            "build_root.mold.linker.program",
            &policy.build_root.mold.linker.program,
            InputOrigin::MoldLinker,
        )?;
    }
    if compiler_cache {
        push_policy_program_input(
            &mut inputs,
            policy_source,
            "build_root.compiler_cache.ccache",
            &policy.build_root.compiler_cache.ccache,
            InputOrigin::CompilerCache {
                role: CompilerCacheRole::Ccache,
            },
        )?;
        push_policy_program_input(
            &mut inputs,
            policy_source,
            "build_root.compiler_cache.sccache",
            &policy.build_root.compiler_cache.sccache,
            InputOrigin::CompilerCache {
                role: CompilerCacheRole::Sccache,
            },
        )?;
    }

    for (role, field, tool) in selected_analyzer_tools(policy, package).entries(&package.options.toolchain) {
        let request = build_tool_name(tool).map_err(|source| Error::InvalidPolicyInput {
            field: field.to_owned(),
            index: 0,
            source,
        })?;
        inputs.push(UnresolvedInput {
            request: request.clone(),
            origin: InputOrigin::Policy {
                source: policy_source.to_owned(),
                field: field.to_owned(),
                index: 0,
            },
        });
        inputs.push(UnresolvedInput {
            request,
            origin: InputOrigin::Analyzer { role },
        });
    }

    let selection = profile.map_or(PackageInputSelection::Package, |name| PackageInputSelection::Profile {
        name: name.to_owned(),
    });
    extend_declared_inputs(
        &mut inputs,
        package.builder_for_profile(profile).required_tools(),
        "builder.required_tools",
        |index| InputOrigin::BuilderTool {
            selection: selection.clone(),
            index,
        },
    )?;
    extend_declared_inputs(
        &mut inputs,
        package.native_build_inputs_for_profile(profile),
        "native_build_inputs",
        |index| InputOrigin::NativeBuild {
            selection: selection.clone(),
            index,
        },
    )?;
    extend_declared_inputs(
        &mut inputs,
        package.build_inputs_for_profile(profile),
        "build_inputs",
        |index| InputOrigin::Build {
            selection: selection.clone(),
            index,
        },
    )?;
    extend_declared_inputs(
        &mut inputs,
        package.check_inputs_for_profile(profile),
        "check_inputs",
        |index| InputOrigin::Check {
            selection: selection.clone(),
            index,
        },
    )?;
    Ok(inputs)
}

fn extend_job_executables(inputs: &mut Vec<UnresolvedInput>, jobs: &[Job]) -> Result<(), Error> {
    for (job_index, job) in jobs.iter().enumerate() {
        let job_index = input_origin_index("jobs", job_index)?;
        for (phase_index, phase) in job.phases.values().enumerate() {
            let phase_index = input_origin_index("jobs[].phases", phase_index)?;
            for (section, steps) in [
                (JobStepSection::Pre, &phase.pre),
                (JobStepSection::Steps, &phase.steps),
                (JobStepSection::Post, &phase.post),
            ] {
                for (step_index, step) in steps.iter().enumerate() {
                    let step_index = input_origin_index("jobs[].phases[].steps", step_index)?;
                    let origin = |role| InputOrigin::JobExecutable {
                        job: job_index,
                        phase: phase_index,
                        phase_name: phase.name.clone(),
                        section,
                        step: step_index,
                        role,
                    };
                    match step {
                        StepPlan::Run { program, .. } => inputs.push(UnresolvedInput {
                            request: program.requirement.canonical_name(),
                            origin: origin(JobExecutableRole::RunProgram),
                        }),
                        StepPlan::RunBuilt { .. } => {}
                        StepPlan::Shell {
                            interpreter,
                            declared_programs,
                            ..
                        } => {
                            inputs.push(UnresolvedInput {
                                request: interpreter.requirement.canonical_name(),
                                origin: origin(JobExecutableRole::ShellInterpreter),
                            });
                            for (program_index, program) in declared_programs.iter().enumerate() {
                                inputs.push(UnresolvedInput {
                                    request: program.requirement.canonical_name(),
                                    origin: origin(JobExecutableRole::ShellDeclaredProgram {
                                        index: input_origin_index(
                                            "jobs[].phases[].steps[].declared_programs",
                                            program_index,
                                        )?,
                                    }),
                                });
                            }
                        }
                        StepPlan::ExtractArchive { .. } => {}
                    }
                }
            }
        }
    }
    Ok(())
}

fn extend_compiler_commands(
    inputs: &mut Vec<UnresolvedInput>,
    policy_source: &str,
    field: &str,
    tools: &CompilerToolsSpec,
) -> Result<(), Error> {
    for (name, role, command) in [
        ("cc", CompilerExecutableRole::Cc, &tools.cc),
        ("cxx", CompilerExecutableRole::Cxx, &tools.cxx),
        ("objc", CompilerExecutableRole::Objc, &tools.objc),
        ("objcxx", CompilerExecutableRole::Objcxx, &tools.objcxx),
        ("cpp", CompilerExecutableRole::Cpp, &tools.cpp),
        ("objcpp", CompilerExecutableRole::Objcpp, &tools.objcpp),
        ("objcxxcpp", CompilerExecutableRole::Objcxxcpp, &tools.objcxxcpp),
        ("ar", CompilerExecutableRole::Ar, &tools.ar),
        ("ld", CompilerExecutableRole::Ld, &tools.ld),
        ("objcopy", CompilerExecutableRole::Objcopy, &tools.objcopy),
        ("nm", CompilerExecutableRole::Nm, &tools.nm),
        ("ranlib", CompilerExecutableRole::Ranlib, &tools.ranlib),
        ("strip", CompilerExecutableRole::Strip, &tools.strip),
    ] {
        push_policy_command_input(
            inputs,
            policy_source,
            &format!("{field}.{name}.program"),
            command,
            InputOrigin::CompilerExecutable { role },
        )?;
    }
    Ok(())
}

fn push_policy_command_input(
    inputs: &mut Vec<UnresolvedInput>,
    policy_source: &str,
    field: &str,
    command: &BuildCommandSpec,
    origin: InputOrigin,
) -> Result<(), Error> {
    push_policy_program_input(inputs, policy_source, field, &command.program, origin)
}

fn push_policy_program_input(
    inputs: &mut Vec<UnresolvedInput>,
    policy_source: &str,
    field: &str,
    program: &BuildProgramSpec,
    origin: InputOrigin,
) -> Result<(), Error> {
    let request = build_tool_name(&program.requirement).map_err(|source| Error::InvalidPolicyInput {
        field: field.to_owned(),
        index: 0,
        source,
    })?;
    inputs.push(UnresolvedInput {
        request: request.clone(),
        origin: InputOrigin::Policy {
            source: policy_source.to_owned(),
            field: field.to_owned(),
            index: 0,
        },
    });
    inputs.push(UnresolvedInput { request, origin });
    Ok(())
}

fn extend_policy_tools(
    inputs: &mut Vec<UnresolvedInput>,
    source_name: &str,
    field: &str,
    tools: &[BuildToolSpec],
) -> Result<(), Error> {
    for (tool_index, tool) in tools.iter().enumerate() {
        inputs.push(UnresolvedInput {
            request: build_tool_name(tool).map_err(|source| Error::InvalidPolicyInput {
                field: field.to_owned(),
                index: tool_index,
                source,
            })?,
            origin: InputOrigin::Policy {
                source: source_name.to_owned(),
                field: field.to_owned(),
                index: input_origin_index(field, tool_index)?,
            },
        });
    }
    Ok(())
}

fn extend_declared_inputs(
    inputs: &mut Vec<UnresolvedInput>,
    dependencies: &[DependencySpec],
    field: &str,
    mut origin: impl FnMut(u32) -> InputOrigin,
) -> Result<(), Error> {
    for (dependency_index, dependency) in dependencies.iter().enumerate() {
        inputs.push(UnresolvedInput {
            request: dependency
                .dependency()
                .map_err(|source| Error::InvalidDeclaredInput {
                    field: field.to_owned(),
                    index: dependency_index,
                    source,
                })?
                .to_name(),
            origin: origin(input_origin_index(field, dependency_index)?),
        });
    }
    Ok(())
}

pub(crate) fn input_origin_index(field: &str, index: usize) -> Result<u32, Error> {
    u32::try_from(index).map_err(|_| Error::InputOriginIndexOverflow {
        field: field.to_owned(),
        index,
    })
}

fn build_tool_name(tool: &BuildToolSpec) -> Result<String, stone::relation::ParseError> {
    tool.dependency().map(|dependency| dependency.to_name())
}

/// Exact analyzer executable capabilities reachable from one frozen handler
/// and package-options combination.
#[derive(Debug, Clone, Copy)]
pub(crate) struct SelectedAnalyzerTools<'a> {
    pub pkg_config: Option<&'a BuildToolSpec>,
    pub python: Option<&'a BuildToolSpec>,
    pub objcopy: Option<&'a BuildToolSpec>,
    pub strip: Option<&'a BuildToolSpec>,
}

impl<'a> SelectedAnalyzerTools<'a> {
    fn entries(
        self,
        toolchain: &ToolchainSpec,
    ) -> impl Iterator<Item = (AnalyzerRole, &'static str, &'a BuildToolSpec)> {
        let (objcopy_field, strip_field) = match toolchain {
            ToolchainSpec::Llvm => (
                "build_root.analyzer_tools.llvm.objcopy",
                "build_root.analyzer_tools.llvm.strip",
            ),
            ToolchainSpec::Gnu => (
                "build_root.analyzer_tools.gnu.objcopy",
                "build_root.analyzer_tools.gnu.strip",
            ),
        };
        [
            self.pkg_config
                .map(|tool| (AnalyzerRole::PkgConfig, "build_root.analyzer_tools.pkg_config", tool)),
            self.python
                .map(|tool| (AnalyzerRole::Python, "build_root.analyzer_tools.python", tool)),
            self.objcopy.map(|tool| (AnalyzerRole::Objcopy, objcopy_field, tool)),
            self.strip.map(|tool| (AnalyzerRole::Strip, strip_field, tool)),
        ]
        .into_iter()
        .flatten()
    }
}

pub(crate) fn selected_analyzer_tools<'a>(
    policy: &'a BuildPolicySpec,
    package: &PackageSpec,
) -> SelectedAnalyzerTools<'a> {
    let handlers = &policy.analyzers;
    let elf_tools = match package.options.toolchain {
        ToolchainSpec::Llvm => &policy.build_root.analyzer_tools.llvm,
        ToolchainSpec::Gnu => &policy.build_root.analyzer_tools.gnu,
    };
    let has_elf = handlers.contains(&AnalyzerKind::Elf);

    SelectedAnalyzerTools {
        pkg_config: handlers
            .contains(&AnalyzerKind::PkgConfig)
            .then_some(&policy.build_root.analyzer_tools.pkg_config),
        python: handlers
            .contains(&AnalyzerKind::Python)
            .then_some(&policy.build_root.analyzer_tools.python),
        objcopy: (has_elf && package.options.debug).then_some(&elf_tools.objcopy),
        strip: (has_elf && package.options.strip).then_some(&elf_tools.strip),
    }
}

pub(crate) fn declared_inputs(recipe: &crate::Recipe, target: &TargetPolicySpec) -> Result<Vec<RelationPlan>, Error> {
    declared_inputs_for(&recipe.declaration, recipe.build_target_profile_key(target))
}

fn declared_inputs_for(package: &PackageSpec, profile: Option<&str>) -> Result<Vec<RelationPlan>, Error> {
    let groups = [
        (
            "builder.required_tools",
            package.builder_for_profile(profile).required_tools(),
        ),
        ("native_build_inputs", package.native_build_inputs_for_profile(profile)),
        ("build_inputs", package.build_inputs_for_profile(profile)),
        ("check_inputs", package.check_inputs_for_profile(profile)),
    ];
    groups
        .into_iter()
        .flat_map(|(field, dependencies)| {
            dependencies.iter().enumerate().map(move |(index, dependency)| {
                dependency
                    .dependency()
                    .map(RelationPlan::from)
                    .map_err(|source| Error::InvalidDeclaredInput {
                        field: field.to_owned(),
                        index,
                        source,
                    })
            })
        })
        .collect()
}

#[derive(Debug, Error)]
pub enum Error {
    #[error("Forge client")]
    ForgeClient(#[from] forge::client::Error),
    #[error("Forge installation")]
    ForgeInstallation(#[from] forge::installation::Error),
    #[error("repository indexes no longer match build.lock.glu")]
    RepositorySnapshotMismatch {
        locked: Vec<RepositorySnapshot>,
        current: Vec<RepositorySnapshot>,
    },
    #[error("locked repository is not configured: {0}")]
    MissingLockedRepository(String),
    #[error("locked repository ID is not canonical: {0}")]
    InvalidLockedRepositoryId(String),
    #[error("locked metadata no longer matches package {package_id}")]
    LockedPackageMetadataMismatch { package_id: String },
    #[error("frozen executable request is absent from build.lock.glu: {0}")]
    MissingFrozenExecutableRequest(String),
    #[error("frozen executable request appears more than once in build.lock.glu: {0}")]
    DuplicateFrozenExecutableRequest(String),
    #[error("frozen plan sandbox layout does not match runtime paths")]
    FrozenSandboxLayoutMismatch,
    #[error("selected package input {field}[{index}] is invalid")]
    InvalidDeclaredInput {
        field: String,
        index: usize,
        #[source]
        source: stone::relation::ParseError,
    },
    #[error("build-policy input {field}[{index}] is invalid")]
    InvalidPolicyInput {
        field: String,
        index: usize,
        #[source]
        source: stone::relation::ParseError,
    },
    #[error("typed input origin {field}[{index}] exceeds the build-lock schema's 32-bit position range")]
    InputOriginIndexOverflow { field: String, index: usize },
}

#[cfg(test)]
#[path = "../build_root/tests.rs"]
mod tests;
