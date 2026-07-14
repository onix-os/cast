// SPDX-FileCopyrightText: 2024 AerynOS Developers
// SPDX-License-Identifier: MPL-2.0

use std::{io, path::PathBuf};

use forge::{Installation, package, repository, util};
use fs_err as fs;
use stone_recipe::{
    ToolchainSpec,
    build_policy::{AnalyzerKind, BuildPolicySpec, BuildToolSpec, TargetEmulationSpec, TargetPolicySpec},
    derivation::{
        AnalyzerRole, BuildLock, DerivationPlan, ExecutablePlan, InputOrigin, JobExecutableRole, JobStepSection,
        LockedPackage, PackageInputSelection, RelationPlan, RepositorySnapshot, StepPlan,
    },
    package::{DependencySpec, PackageSpec},
};
use thiserror::Error;

use crate::build::{Builder, job::Job};
use crate::{Timing, container, timing};

pub fn populate_frozen(
    paths: &crate::Paths,
    forge_dir: &std::path::Path,
    repositories: repository::Map,
    plan: &DerivationPlan,
    timing: &mut Timing,
    initialize_timer: timing::Timer,
) -> Result<(), Error> {
    let rootfs = paths.rootfs().host;
    let build_lock = &plan.build_lock;

    // Create the Forge client.
    let repositories = locked_repositories(&repositories, &build_lock.repositories)?;
    let installation = Installation::open_frozen(forge_dir, None)?;
    let mut forge_client = forge::Client::frozen("cast", installation, repositories, rootfs)?;
    require_locked_repositories(&forge_client, build_lock)?;
    let package_ids = exact_package_ids(&forge_client, build_lock)?;

    timing.finish(initialize_timer);

    // The planner already selected the complete package closure. Installing
    // provider strings here would silently cross the freeze boundary and allow
    // a newer candidate to replace a locked package.
    let install_timing = forge_client.materialize_frozen_root(&package_ids, plan.source_date_epoch)?;
    let executable_bindings = frozen_executable_bindings(plan)?;
    forge_client.require_frozen_executables(&package_ids, &executable_bindings)?;

    timing.record(timing::Populate::Resolve, install_timing.resolve);
    timing.record(timing::Populate::Fetch, install_timing.fetch);
    timing.record(timing::Populate::Blit, install_timing.blit);

    Ok(())
}

pub fn recreate_frozen(paths: &crate::Paths, plan: &DerivationPlan) -> Result<(), Error> {
    require_frozen_layout(paths, plan)?;
    if paths.rootfs().host.exists() {
        remove_frozen(paths, plan)?;
    }
    util::recreate_dir(&paths.rootfs().host)?;
    Ok(())
}

pub fn remove_frozen(paths: &crate::Paths, plan: &DerivationPlan) -> Result<(), Error> {
    require_frozen_layout(paths, plan)?;
    if !paths.rootfs().host.exists() {
        return Ok(());
    }
    let build_root = PathBuf::from(&plan.layout.build_dir);
    let unsafe_job_path = plan
        .jobs
        .iter()
        .flat_map(|job| [&job.work_dir, &job.build_dir].into_iter().chain(job.pgo_dir.iter()))
        .map(PathBuf::from)
        .any(|path| !safe_child(&build_root, &path));
    if unsafe_job_path {
        return Err(Error::UnsafeFrozenJobPath);
    }

    container::exec_frozen(paths, plan, || {
        let install_dir = PathBuf::from(&plan.layout.install_dir);
        if install_dir.exists() {
            fs::remove_dir_all(&install_dir)?;
        }
        if build_root.exists() {
            for entry in fs::read_dir(&build_root)? {
                let entry = entry?;
                let path = entry.path();
                if entry.file_type()?.is_dir() {
                    fs::remove_dir_all(path)?;
                } else {
                    fs::remove_file(path)?;
                }
            }
        }
        Ok(()) as io::Result<()>
    })?;
    fs::remove_dir_all(&paths.rootfs().host)?;
    Ok(())
}

fn require_frozen_layout(paths: &crate::Paths, plan: &DerivationPlan) -> Result<(), Error> {
    if paths.layout() == &plan.layout {
        Ok(())
    } else {
        Err(Error::FrozenSandboxLayoutMismatch)
    }
}

fn safe_child(root: &std::path::Path, path: &std::path::Path) -> bool {
    path.is_absolute()
        && path.starts_with(root)
        && !path.components().any(|component| {
            matches!(
                component,
                std::path::Component::ParentDir | std::path::Component::CurDir
            )
        })
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
                StepPlan::Shell {
                    interpreter,
                    declared_programs,
                    ..
                } => {
                    executables.push(interpreter);
                    executables.extend(declared_programs);
                }
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
        extend_policy_tools(
            &mut inputs,
            policy_source,
            "build_root.mold.required_tools",
            &policy.build_root.mold.required_tools,
        )?;
    }
    if compiler_cache {
        extend_policy_tools(
            &mut inputs,
            policy_source,
            "build_root.compiler_cache.required_tools",
            &policy.build_root.compiler_cache.required_tools,
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
                    }
                }
            }
        }
    }
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
    #[error("io")]
    Io(#[from] io::Error),
    #[error("Forge client")]
    ForgeClient(#[from] forge::client::Error),
    #[error("Forge installation")]
    ForgeInstallation(#[from] forge::installation::Error),
    #[error("container")]
    Container(#[from] container::Error),
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
    #[error("frozen job cleanup path escapes the runtime build directory")]
    UnsafeFrozenJobPath,
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
mod tests {
    use std::collections::{BTreeMap, BTreeSet};

    use forge::package::{Flags, Meta, Name};
    use gluon_config::Source;
    use stone_recipe::UpstreamSpec;
    use stone_recipe::derivation::{LockedOutput, LockedOutputRef};

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
    fn selected_root_features_combine_typed_policy_and_builder_tools() {
        let mut policy = repository_policy();
        policy.build_root.base = vec![BuildToolSpec::Package("policy-base".to_owned())];
        policy.build_root.toolchains.llvm = vec![BuildToolSpec::Binary("wrong-llvm".to_owned())];
        policy.build_root.toolchains.gnu = vec![BuildToolSpec::Binary("policy-gnu".to_owned())];
        policy.build_root.emul32.base = vec![BuildToolSpec::SystemBinary("policy-emul-base".to_owned())];
        policy.build_root.emul32.toolchains.llvm = vec![BuildToolSpec::Package("wrong-llvm32".to_owned())];
        policy.build_root.emul32.toolchains.gnu = vec![BuildToolSpec::Package("policy-gnu32".to_owned())];
        policy.build_root.mold.required_tools = vec![BuildToolSpec::Binary("policy-mold".to_owned())];
        policy.build_root.compiler_cache.required_tools = vec![BuildToolSpec::Binary("policy-cache".to_owned())];

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
            "binary(objcopy)",
            "binary(pkg-config)",
            "binary(policy-cache)",
            "binary(policy-gnu)",
            "binary(policy-mold)",
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
        ] {
            assert!(
                inputs
                    .iter()
                    .any(|input| input.request == request && input.origin == origin),
                "missing {request:?} origin {origin:?}"
            );
        }
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
                    post: Vec::new(),
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
        let plan = crate::package::test_derivation_plan();
        let bindings = frozen_executable_bindings(&plan).unwrap();

        assert_eq!(bindings.len(), 3);
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
}
