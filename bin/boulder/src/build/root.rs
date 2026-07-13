// SPDX-FileCopyrightText: 2024 AerynOS Developers
// SPDX-License-Identifier: MPL-2.0

use std::collections::BTreeSet;
use std::{io, path::PathBuf};

use fs_err as fs;
use moss::{Installation, package, repository, util};
use stone_recipe::{
    ToolchainSpec, UpstreamSpec,
    derivation::{BuildLock, DerivationPlan, LockedPackage, RepositorySnapshot},
};
use thiserror::Error;

use crate::build::Builder;
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
    if paths.rootfs().host.exists() {
        remove_frozen(paths, plan)?;
    }
    util::recreate_dir(&paths.rootfs().host)?;
    Ok(())
}

pub fn remove_frozen(paths: &crate::Paths, plan: &DerivationPlan) -> Result<(), Error> {
    if !paths.rootfs().host.exists() {
        return Ok(());
    }
    let build_root = paths.build().guest;
    if plan.layout.build_dir != build_root.display().to_string() {
        return Err(Error::FrozenBuildLayoutMismatch);
    }
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
        let install_dir = &paths.install().guest;
        if install_dir.exists() {
            fs::remove_dir_all(install_dir)?;
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
    let mut packages = BASE_PACKAGES
        .iter()
        .map(|package| (*package).to_owned())
        .collect::<Vec<_>>();

    match builder.recipe.declaration.options.toolchain {
        ToolchainSpec::Llvm => packages.extend(LLVM_PACKAGES.iter().map(|package| (*package).to_owned())),
        ToolchainSpec::Gnu => packages.extend(GNU_PACKAGES.iter().map(|package| (*package).to_owned())),
    }

    if builder.target.build_target.emul32() {
        packages.extend(BASE32_PACKAGES.iter().map(|package| (*package).to_owned()));

        match builder.recipe.declaration.options.toolchain {
            ToolchainSpec::Llvm => packages.extend(LLVM32_PACKAGES.iter().map(|package| (*package).to_owned())),
            ToolchainSpec::Gnu => packages.extend(GNU32_PACKAGES.iter().map(|package| (*package).to_owned())),
        }
    }

    if builder.recipe.declaration.mold {
        packages.extend(MOLD_PACKAGES.iter().map(|package| (*package).to_owned()));
    }

    if builder.ccache {
        packages.extend(CCACHE_PACKAGES.iter().map(|package| (*package).to_owned()));
    }

    packages.extend(declared_inputs(&builder.recipe, builder.target.build_target)?);

    for source in &builder.recipe.declaration.sources {
        if let UpstreamSpec::Archive { url, rename, .. } = source {
            let path = url::Url::parse(url)
                .expect("validated package source URL")
                .path()
                .to_owned();

            for path in std::iter::once(path.as_str()).chain(rename.as_deref()) {
                if let Some((_, ext)) = path.rsplit_once('.') {
                    match ext {
                        "xz" => {
                            packages.push("binary(bsdtar-static)".to_owned());
                        }
                        "zst" => {
                            packages.push("binary(bsdtar-static)".to_owned());
                        }
                        "bz2" => {
                            packages.push("binary(bsdtar-static)".to_owned());
                        }
                        "gz" => {
                            packages.push("binary(bsdtar-static)".to_owned());
                        }
                        "lz" => {
                            packages.push("binary(bsdtar-static)".to_owned());
                        }
                        "tgz" => {
                            packages.push("binary(bsdtar-static)".to_owned());
                        }
                        "7z" => {
                            packages.push("binary(bsdtar-static)".to_owned());
                        }
                        "zip" => {
                            packages.push("binary(bsdtar-static)".to_owned());
                        }
                        "rpm" => {
                            packages.extend(["binary(rpm2cpio)".to_owned(), "cpio".to_owned()]);
                        }
                        "deb" => {
                            packages.push("binary(ar)".to_owned());
                        }
                        _ => {}
                    }
                }
            }
        }
    }

    // Dependencies from all scripts in the builder
    packages.extend(builder.extra_deps().map(str::to_owned));

    Ok(packages
        .into_iter()
        // Remove dupes
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect())
}

pub(crate) fn declared_inputs(
    recipe: &crate::Recipe,
    target: crate::architecture::BuildTarget,
) -> Result<Vec<String>, Error> {
    declared_inputs_for(&recipe.declaration, recipe.build_target_profile_key(target))
}

fn declared_inputs_for(
    package: &stone_recipe::package::PackageSpec,
    profile: Option<&str>,
) -> Result<Vec<String>, Error> {
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
                .map(|dependency| dependency.to_name())
                .map_err(|source| Error::InvalidDeclaredInput { index, source })
        })
        .collect()
}

const BASE_PACKAGES: &[&str] = &[
    "bash",
    "boulder",
    "coreutils",
    "dash",
    "diffutils",
    "findutils",
    "gawk",
    "glibc-devel",
    "grep",
    "layout",
    "libarchive",
    "linux-headers",
    "os-info",
    "pkgconf",
    "sed",
    "util-linux",
    // Needed for chroot
    "binary(git)",
    "binary(hx)",
    "binary(less)",
    "binary(nano)",
    "binary(ps)",
    "binary(rg)",
    "binary(vim)",
];
const BASE32_PACKAGES: &[&str] = &["glibc-32bit-devel"];

const GNU_PACKAGES: &[&str] = &["binary(ld.bfd)", "binary(gcc)", "binary(g++)"];
const GNU32_PACKAGES: &[&str] = &["gcc-32bit", "libstdc++-32bit-devel"];

const LLVM_PACKAGES: &[&str] = &["clang"];
const LLVM32_PACKAGES: &[&str] = &["clang-32bit"];

const MOLD_PACKAGES: &[&str] = &["binary(mold)"];

const CCACHE_PACKAGES: &[&str] = &["binary(ccache)", "binary(sccache)"];

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
    #[error("frozen plan build layout does not match runtime paths")]
    FrozenBuildLayoutMismatch,
    #[error("frozen job cleanup path escapes the runtime build directory")]
    UnsafeFrozenJobPath,
    #[error("selected package input {index} is invalid")]
    InvalidDeclaredInput {
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
            outputs: vec![LockedOutput {
                name: "out".to_owned(),
                id: "locked-id".to_owned(),
            }],
            dependencies: Vec::<LockedOutputRef>::new(),
        }
    }

    fn selected_inputs_package() -> stone_recipe::package::PackageSpec {
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

        assert_eq!(
            declared_inputs_for(&package, None).unwrap(),
            ["binary(base-builder)", "base-native", "base-build", "base-check"]
        );
    }

    #[test]
    fn direct_inputs_use_only_the_selected_profile() {
        let package = selected_inputs_package();
        let selected = declared_inputs_for(&package, Some("x86_64")).unwrap();

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
