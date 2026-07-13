// SPDX-FileCopyrightText: 2024 AerynOS Developers
// SPDX-License-Identifier: MPL-2.0

use std::collections::BTreeSet;
use std::{io, iter, path::PathBuf};

use fs_err as fs;
use moss::{Installation, package, repository, runtime, util};
use stone_recipe::upstream;
use stone_recipe::{
    derivation::{BuildLock, DerivationPlan, LockedPackage, RepositorySnapshot},
    tuning::Toolchain,
};
use thiserror::Error;

use crate::build::Builder;
use crate::{Timing, container, timing};

pub fn populate_locked(
    builder: &Builder,
    build_lock: &BuildLock,
    timing: &mut Timing,
    initialize_timer: timing::Timer,
) -> Result<(), Error> {
    populate_frozen(
        &builder.paths,
        &builder.env.moss_dir,
        builder.repositories().clone(),
        build_lock,
        timing,
        initialize_timer,
    )
}

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

pub fn populate(
    builder: &Builder,
    repositories: repository::Map,
    timing: &mut Timing,
    initialize_timer: timing::Timer,
    update_repos: bool,
) -> Result<(), Error> {
    let packages = packages(builder);
    let rootfs = builder.paths.rootfs().host;
    let installation = Installation::open(&builder.env.moss_dir, None)?;
    let mut moss_client = moss::Client::builder("boulder", installation)
        .repositories(repositories)
        .ephemeral(rootfs)
        .build()?;

    if update_repos {
        runtime::block_on(moss_client.refresh_repositories())?;
        println!();
    } else if runtime::block_on(moss_client.ensure_repos_initialized())? > 0 {
        println!();
    }
    timing.finish(initialize_timer);
    let install_timing = moss_client.install(&packages, true, false)?;
    timing.record(timing::Populate::Resolve, install_timing.resolve);
    timing.record(timing::Populate::Fetch, install_timing.fetch);
    timing.record(timing::Populate::Blit, install_timing.blit);
    Ok(())
}

pub fn recreate(builder: &Builder) -> Result<(), Error> {
    clean(builder)?;

    // Now we can safely recreate the rootfs
    util::recreate_dir(&builder.paths.rootfs().host)?;

    Ok(())
}

pub fn remove(builder: &Builder) -> Result<(), Error> {
    if builder.paths.rootfs().host.exists() {
        clean(builder)?;

        // Now we can safely remove the rootfs
        fs::remove_dir_all(&builder.paths.rootfs().host)?;
    }

    Ok(())
}

fn clean(builder: &Builder) -> Result<(), Error> {
    // Dont't need to clean if it doesn't exist
    if !builder.paths.rootfs().host.exists() {
        return Ok(());
    }

    // We remove certain paths inside the container so we don't
    // get permissions error if this is a rootless build
    // and there's subuid mappings into the user namespace
    container::exec(&builder.paths, false, || {
        // Remove install dir
        let install_dir = builder.paths.install().guest;
        if install_dir.exists() {
            fs::remove_dir_all(install_dir)?;
        }

        for target in &builder.targets {
            for job in &target.jobs {
                if job.build_dir.exists() {
                    fs::remove_dir_all(&job.build_dir)?;
                }
            }
        }

        Ok(()) as io::Result<_>
    })?;

    Ok(())
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

pub(crate) fn packages(builder: &Builder) -> Vec<&str> {
    let mut packages = BASE_PACKAGES.to_vec();

    match builder.recipe.parsed.options.toolchain {
        Toolchain::Llvm => packages.extend(LLVM_PACKAGES),
        Toolchain::Gnu => packages.extend(GNU_PACKAGES),
    }

    if builder.recipe.parsed.emul32 {
        packages.extend(BASE32_PACKAGES);

        match builder.recipe.parsed.options.toolchain {
            Toolchain::Llvm => packages.extend(LLVM32_PACKAGES),
            Toolchain::Gnu => packages.extend(GNU32_PACKAGES),
        }
    }

    if builder.recipe.parsed.mold {
        packages.extend(MOLD_PACKAGES);
    }

    if builder.ccache {
        packages.extend(CCACHE_PACKAGES);
    }

    packages.extend(
        builder.recipe.parsed.build.build_deps.iter().map(String::as_str).chain(
            builder
                .recipe
                .parsed
                .profiles
                .iter()
                .flat_map(|kv| kv.value.build_deps.iter().map(String::as_str)),
        ),
    );
    packages.extend(
        builder.recipe.parsed.build.check_deps.iter().map(String::as_str).chain(
            builder
                .recipe
                .parsed
                .profiles
                .iter()
                .flat_map(|kv| kv.value.check_deps.iter().map(String::as_str)),
        ),
    );

    for upstream in &builder.recipe.parsed.upstreams {
        if let upstream::Props::Plain { rename, .. } = &upstream.props {
            let path = upstream.url.path();

            for path in iter::once(path).chain(rename.as_deref()) {
                if let Some((_, ext)) = path.rsplit_once('.') {
                    match ext {
                        "xz" => {
                            packages.push("binary(bsdtar-static)");
                        }
                        "zst" => {
                            packages.push("binary(bsdtar-static)");
                        }
                        "bz2" => {
                            packages.push("binary(bsdtar-static)");
                        }
                        "gz" => {
                            packages.push("binary(bsdtar-static)");
                        }
                        "lz" => {
                            packages.push("binary(bsdtar-static)");
                        }
                        "tgz" => {
                            packages.push("binary(bsdtar-static)");
                        }
                        "7z" => {
                            packages.push("binary(bsdtar-static)");
                        }
                        "zip" => {
                            packages.push("binary(bsdtar-static)");
                        }
                        "rpm" => {
                            packages.extend(["binary(rpm2cpio)", "cpio"]);
                        }
                        "deb" => {
                            packages.push("binary(ar)");
                        }
                        _ => {}
                    }
                }
            }
        }
    }

    // Dependencies from all scripts in the builder
    let extra_deps = builder.extra_deps();

    packages
        .into_iter()
        .chain(extra_deps)
        // Remove dupes
        .collect::<BTreeSet<_>>()
        .into_iter()
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
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

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
}
