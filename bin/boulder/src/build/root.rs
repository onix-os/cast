// SPDX-FileCopyrightText: 2024 AerynOS Developers
// SPDX-License-Identifier: MPL-2.0

use std::collections::BTreeSet;
use std::{io, path::PathBuf};

use fs_err as fs;
use moss::{Installation, package, repository, util};
use stone::relation::{Dependency, Kind as RelationKind};
use stone_recipe::{
    ToolchainSpec, UpstreamSpec,
    build_policy::{BuildPolicySpec, BuildToolSpec, TargetEmulationSpec, TargetPolicySpec},
    derivation::{BuildLock, DerivationPlan, LockedPackage, RelationPlan, RepositorySnapshot},
    package::PackageSpec,
};
use thiserror::Error;

use crate::build::{Builder, pgo};
use crate::{Timing, container, timing};

pub fn populate_frozen(
    paths: &crate::Paths,
    moss_dir: &std::path::Path,
    repositories: repository::Map,
    build_lock: &BuildLock,
    timing: &mut Timing,
    initialize_timer: timing::Timer,
) -> Result<(), Error> {
    let rootfs = paths.rootfs().host;

    // Create the moss client
    let installation = Installation::open(moss_dir, None)?;
    let mut moss_client = moss::Client::builder("boulder", installation)
        .repositories(repositories)
        .ephemeral(rootfs)
        .build()?;
    require_locked_repositories(&moss_client, build_lock)?;
    let package_ids = exact_package_ids(&moss_client, build_lock)?;

    timing.finish(initialize_timer);

    // The planner already selected the complete package closure. Installing
    // provider strings here would silently cross the freeze boundary and allow
    // a newer candidate to replace a locked package.
    let install_timing = moss_client.install_exact(&package_ids, true, false)?;

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

fn require_locked_repositories(client: &moss::Client, build_lock: &BuildLock) -> Result<(), Error> {
    let locked_ids = build_lock
        .repositories
        .iter()
        .map(|repository| repository.id.clone())
        .collect::<BTreeSet<_>>();
    let mut current = client
        .repository_index_snapshots()?
        .into_iter()
        .filter(|snapshot| locked_ids.contains(&snapshot.id.to_string()))
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

fn exact_package_ids(client: &moss::Client, build_lock: &BuildLock) -> Result<Vec<package::Id>, Error> {
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

fn require_locked_metadata(locked: &LockedPackage, package: &moss::Package) -> Result<(), Error> {
    if !locked_metadata_matches(locked, package) {
        return Err(Error::LockedPackageMetadataMismatch {
            package_id: locked.package_id.clone(),
        });
    }
    Ok(())
}

fn locked_metadata_matches(locked: &LockedPackage, package: &moss::Package) -> bool {
    let version = format!(
        "{}-{}-{}",
        package.meta.version_identifier, package.meta.source_release, package.meta.build_release
    );
    package.meta.name.as_str() == locked.name
        && version == locked.version
        && package.meta.architecture == locked.architecture
}

pub(crate) fn packages(builder: &Builder) -> Result<Vec<String>, Error> {
    packages_for(
        &builder.target.build_policy.spec,
        &builder.recipe.declaration,
        builder.recipe.build_target_profile_key(&builder.target.target_policy),
        &builder.target.target_policy,
        builder.ccache,
        pgo::stages(&builder.recipe, &builder.target.target_policy).is_some(),
    )
}

fn packages_for(
    policy: &BuildPolicySpec,
    package: &PackageSpec,
    profile: Option<&str>,
    target: &TargetPolicySpec,
    compiler_cache: bool,
    pgo_enabled: bool,
) -> Result<Vec<String>, Error> {
    let mut packages = Vec::new();
    extend_policy_tools(&mut packages, "build_root.base", &policy.build_root.base)?;

    let toolchain_tools = match package.options.toolchain {
        ToolchainSpec::Llvm => &policy.build_root.toolchains.llvm,
        ToolchainSpec::Gnu => &policy.build_root.toolchains.gnu,
    };
    extend_policy_tools(&mut packages, "build_root.toolchains", toolchain_tools)?;

    if matches!(&target.emulation, TargetEmulationSpec::Emul32 { .. }) {
        extend_policy_tools(&mut packages, "build_root.emul32.base", &policy.build_root.emul32.base)?;
        let toolchain_tools = match package.options.toolchain {
            ToolchainSpec::Llvm => &policy.build_root.emul32.toolchains.llvm,
            ToolchainSpec::Gnu => &policy.build_root.emul32.toolchains.gnu,
        };
        extend_policy_tools(&mut packages, "build_root.emul32.toolchains", toolchain_tools)?;
    }

    if package.mold {
        extend_policy_tools(
            &mut packages,
            "build_root.mold.required_tools",
            &policy.build_root.mold.required_tools,
        )?;
    }
    if compiler_cache {
        extend_policy_tools(
            &mut packages,
            "build_root.compiler_cache.required_tools",
            &policy.build_root.compiler_cache.required_tools,
        )?;
    }

    if pgo_enabled && matches!(package.options.toolchain, ToolchainSpec::Llvm) {
        extend_policy_tools(&mut packages, "pgo.required_tools", &policy.pgo.required_tools)?;
    }

    packages.extend(
        declared_inputs_for(package, profile)?
            .into_iter()
            .map(|relation| relation.canonical_name()),
    );
    extend_source_tools(&mut packages, policy, &package.sources)?;

    Ok(packages.into_iter().collect::<BTreeSet<_>>().into_iter().collect())
}

fn extend_source_tools(
    packages: &mut Vec<String>,
    policy: &BuildPolicySpec,
    sources: &[UpstreamSpec],
) -> Result<(), Error> {
    for source in sources {
        match source {
            UpstreamSpec::Archive { unpack: true, .. } => {
                extend_policy_tools(
                    packages,
                    "sources.archive.required_tools",
                    &policy.sources.archive.required_tools,
                )?;
            }
            UpstreamSpec::Archive { unpack: false, .. } => {}
            UpstreamSpec::Git { .. } => extend_policy_tools(
                packages,
                "sources.git.required_tools",
                &policy.sources.git.required_tools,
            )?,
        }
    }
    Ok(())
}

fn extend_policy_tools(packages: &mut Vec<String>, field: &'static str, tools: &[BuildToolSpec]) -> Result<(), Error> {
    packages.extend(
        tools
            .iter()
            .enumerate()
            .map(|(index, tool)| {
                build_tool_name(tool).map_err(|source| Error::InvalidPolicyInput { field, index, source })
            })
            .collect::<Result<Vec<_>, _>>()?,
    );
    Ok(())
}

fn build_tool_name(tool: &BuildToolSpec) -> Result<String, stone::relation::ParseError> {
    let (kind, target) = match tool {
        BuildToolSpec::Package(target) => (RelationKind::PackageName, target),
        BuildToolSpec::Binary(target) => (RelationKind::Binary, target),
        BuildToolSpec::SystemBinary(target) => (RelationKind::SystemBinary, target),
    };
    Dependency::new(kind, target.clone()).map(|dependency| dependency.to_name())
}

pub(crate) fn declared_inputs(recipe: &crate::Recipe, target: &TargetPolicySpec) -> Result<Vec<RelationPlan>, Error> {
    declared_inputs_for(&recipe.declaration, recipe.build_target_profile_key(target))
}

fn declared_inputs_for(package: &PackageSpec, profile: Option<&str>) -> Result<Vec<RelationPlan>, Error> {
    package
        .builder_for_profile(profile)
        .required_tools()
        .iter()
        .chain(package.native_build_inputs_for_profile(profile))
        .chain(package.build_inputs_for_profile(profile))
        .chain(package.check_inputs_for_profile(profile))
        .enumerate()
        .map(|(index, dependency)| {
            dependency
                .dependency()
                .map(RelationPlan::from)
                .map_err(|source| Error::InvalidDeclaredInput { index, source })
        })
        .collect()
}

#[derive(Debug, Error)]
pub enum Error {
    #[error("io")]
    Io(#[from] io::Error),
    #[error("moss client")]
    MossClient(#[from] moss::client::Error),
    #[error("moss installation")]
    MossInstallation(#[from] moss::installation::Error),
    #[error("container")]
    Container(#[from] container::Error),
    #[error("repository indexes no longer match build.lock.glu")]
    RepositorySnapshotMismatch {
        locked: Vec<RepositorySnapshot>,
        current: Vec<RepositorySnapshot>,
    },
    #[error("locked metadata no longer matches package {package_id}")]
    LockedPackageMetadataMismatch { package_id: String },
    #[error("frozen plan sandbox layout does not match runtime paths")]
    FrozenSandboxLayoutMismatch,
    #[error("frozen job cleanup path escapes the runtime build directory")]
    UnsafeFrozenJobPath,
    #[error("selected package input {index} is invalid")]
    InvalidDeclaredInput {
        index: usize,
        #[source]
        source: stone::relation::ParseError,
    },
    #[error("build-policy input {field}[{index}] is invalid")]
    InvalidPolicyInput {
        field: &'static str,
        index: usize,
        #[source]
        source: stone::relation::ParseError,
    },
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use gluon_config::Source;
    use moss::package::{Flags, Meta, Name};
    use stone_recipe::derivation::{LockedOutput, LockedOutputRef};

    use super::*;

    fn package() -> moss::Package {
        moss::Package {
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
            r#"let b = import! boulder.package.v2
let base = b.mk_package (b.meta {
    pname = "example", version = "1.0.0", release = 1,
    homepage = "https://example.invalid", license = ["MPL-2.0"],
})
let scripts = b.defaults.scripts
let selected = b.profile_with {
    name = "x86_64",
    builder = b.builder.shell scripts [b.dep.binary "profile-builder"],
    hooks = b.defaults.hooks,
    native_build_inputs = [b.dep.package "profile-native"],
    build_inputs = [b.dep.package "profile-build"],
    check_inputs = [b.dep.package "profile-check"],
}
let unrelated = b.profile_with {
    name = "aarch64",
    builder = b.builder.shell scripts [b.dep.binary "unrelated-builder"],
    hooks = b.defaults.hooks,
    native_build_inputs = [b.dep.package "unrelated-native"],
    build_inputs = [], check_inputs = [],
}
{
    builder = b.builder.shell scripts [b.dep.binary "base-builder"],
    native_build_inputs = [b.dep.package "base-native"],
    build_inputs = [b.dep.package "base-build"],
    check_inputs = [b.dep.package "base-check"],
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
            r#"let b = import! boulder.package.v2
let cmake = import! boulder.builders.cmake.v1
let base = b.mk_package (b.meta {
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
        policy.sources.archive.required_tools = vec![BuildToolSpec::Binary("policy-archive".to_owned())];
        policy.sources.git.required_tools = vec![BuildToolSpec::SystemBinary("policy-git".to_owned())];

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

        let packages = packages_for(&policy, &package, None, &target, true, false).unwrap();
        let expected = [
            "base-build",
            "base-check",
            "base-native",
            "binary(cmake)",
            "binary(ctest)",
            "binary(ninja)",
            "binary(policy-archive)",
            "binary(policy-cache)",
            "binary(policy-gnu)",
            "binary(policy-mold)",
            "policy-base",
            "policy-gnu32",
            "sysbinary(policy-emul-base)",
            "sysbinary(policy-git)",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect::<BTreeSet<_>>();

        assert_eq!(packages.into_iter().collect::<BTreeSet<_>>(), expected);
    }

    #[test]
    fn pgo_policy_tools_follow_the_llvm_finish_command() {
        let mut policy = repository_policy();
        policy.pgo.required_tools = vec![BuildToolSpec::SystemBinary("policy-profdata".to_owned())];
        let mut package = selected_inputs_package();
        let target = policy
            .targets
            .iter()
            .find(|target| target.name == "x86_64")
            .unwrap()
            .clone();

        package.options.toolchain = ToolchainSpec::Llvm;
        let llvm = packages_for(&policy, &package, None, &target, false, true).unwrap();
        let llvm_without_pgo = packages_for(&policy, &package, None, &target, false, false).unwrap();
        package.options.toolchain = ToolchainSpec::Gnu;
        let gnu = packages_for(&policy, &package, None, &target, false, true).unwrap();

        assert!(llvm.contains(&"sysbinary(policy-profdata)".to_owned()));
        assert!(!llvm_without_pgo.contains(&"sysbinary(policy-profdata)".to_owned()));
        assert!(!gnu.contains(&"sysbinary(policy-profdata)".to_owned()));
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
