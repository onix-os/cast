// SPDX-FileCopyrightText: 2026 AerynOS Developers
// SPDX-License-Identifier: MPL-2.0

use std::{fs::File, io, os::unix::fs::PermissionsExt as _, path::Path};

use clap::{Args, Parser};
use container::Container;
use forge::util;
use humansize::BINARY;
use rayon::iter::{ParallelBridge, ParallelIterator};
use thiserror::Error;
use walkdir::WalkDir;

use crate::Env;

#[derive(Debug, Parser)]
#[command(about = "Manage Cast build caches")]
pub struct Command {
    #[command(flatten)]
    pub global: Global,
    #[command(subcommand)]
    subcommand: Subcommand,
}

#[derive(Debug, Args)]
pub struct Global {
    #[arg(
        short,
        long = "build-cache",
        help = "Select the package-build cache",
        default_value_t = false,
        global = true
    )]
    pub build_cache: bool,
    #[arg(
        short = 'p',
        long = "package-cache",
        help = "Select the package-resolution cache used by builds",
        default_value_t = false,
        global = true
    )]
    pub package_cache: bool,
}

#[derive(Debug, clap::Subcommand)]
pub enum Subcommand {
    #[command(about = "Clean out the cache(s) for the current environment")]
    Clean,
    #[command(about = "Show the cache size(s) for the current environment")]
    Size,
}

pub fn handle(command: Command, env: Env) -> Result<(), Error> {
    let build_cache = command.global.build_cache;
    let package_cache = command.global.package_cache;

    match command.subcommand {
        Subcommand::Clean => clean(env, build_cache, package_cache),
        Subcommand::Size => size(env, build_cache, package_cache),
    }
}

fn clean(env: Env, build_cache: bool, package_cache: bool) -> Result<(), Error> {
    for cache in selected_caches(&env, build_cache, package_cache) {
        // The path is only a name witness. The retained descriptor selected by
        // Env remains the destructive authority even if the name is changed
        // after this check.
        crate::paths::require_workspace_root_path(cache.anchor, cache.path)?;

        let tmpdir = tempfile::Builder::new()
            .permissions(std::fs::Permissions::from_mode(0o700))
            .tempdir()?;
        // A restrictive umask may remove owner bits requested at creation.
        // Normalize only this fresh TempDir and propagate failures instead of
        // panicking in a user-facing cache operation.
        std::fs::set_permissions(tmpdir.path(), std::fs::Permissions::from_mode(0o700))?;
        let remove_target = tmpdir.path().join("remove");
        let (_remove_target, remove_target_anchor) =
            crate::paths::prepare_private_workspace_root_pinned(&remove_target)?;
        let container_root = crate::paths::pin_workspace_root(tmpdir.path())?;

        println!("Deleting {} directory: {}", cache.name, cache.path.display());

        Container::new_anchored(tmpdir.path(), &container_root)?
            .bind_rw_pinned(cache.anchor, cache.path, Path::new("/remove"))?
            .run(|| util::par_remove_dir_contents(Path::new("/remove")))?;

        // Keep the authenticated target inode pinned through activation and
        // prove the public cache name still denotes the retained source after
        // the destructive interval. The target descriptor also prevents an
        // inode-number reuse from making a replaced mountpoint look unchanged.
        drop(remove_target_anchor);
        crate::paths::require_workspace_root_path(cache.anchor, cache.path)?;
    }
    Ok(())
}

fn size(env: Env, build_cache: bool, package_cache: bool) -> Result<(), Error> {
    for cache in selected_caches(&env, build_cache, package_cache) {
        crate::paths::require_workspace_root_path(cache.anchor, cache.path)?;
        let size: u64 = WalkDir::new(cache.path)
            .into_iter()
            .par_bridge()
            .filter_map(Result::ok)
            .filter_map(|e| e.metadata().ok())
            .map(|m| m.len())
            .sum();
        crate::paths::require_workspace_root_path(cache.anchor, cache.path)?;
        println!(
            "{} ({}): {}",
            cache.name,
            cache.path.display(),
            humansize::format_size(size, BINARY)
        );
    }
    Ok(())
}

#[derive(Clone, Copy)]
struct SelectedCache<'a> {
    name: &'static str,
    path: &'a Path,
    anchor: &'a File,
}

fn selected_caches(env: &Env, build_cache: bool, package_cache: bool) -> Vec<SelectedCache<'_>> {
    let select_all = !build_cache && !package_cache;
    let mut v = Vec::new();
    if select_all || build_cache {
        v.push(SelectedCache {
            name: "build cache",
            path: &env.cache_dir,
            anchor: env.cache_dir_anchor.as_ref(),
        });
    }
    if select_all || package_cache {
        v.push(SelectedCache {
            name: "package cache",
            path: &env.forge_dir,
            anchor: env.forge_dir_anchor.as_ref(),
        });
    }
    v
}

#[derive(Debug, Error)]
pub enum Error {
    #[error("cache container operation: {0}")]
    Container(#[from] container::Error),
    #[error("cache filesystem operation: {0}")]
    Io(#[from] io::Error),
}

#[cfg(feature = "cache-clean-test-support")]
pub(crate) fn run_harness_free_test() {
    use std::{os::unix::fs::symlink, process};

    assert_exact_main_task("harness-free cache-clean startup");

    let root = crate::private_tempdir();
    let forge = root.path().join("forge");
    let env = Env::new(
        Some(root.path().join("build")),
        Some(root.path().join("config")),
        Some(root.path().join("data")),
        Some(forge.clone()),
    )
    .expect("prepare cache-clean environment");
    std::fs::create_dir(forge.join("nested")).expect("create nested package-cache directory");
    std::fs::write(forge.join("nested/file"), b"remove").expect("write package-cache child");
    let outside = root.path().join("outside");
    std::fs::create_dir(&outside).expect("create external directory");
    std::fs::write(outside.join("keep"), b"keep outside").expect("write external witness");
    symlink(&outside, forge.join("outside-link")).expect("create package-cache symlink");

    // Cloning must retain the same directory capabilities rather than
    // reopening pathnames or manufacturing a weaker optional authority.
    let clean_env = env.clone();
    match clean(clean_env, false, true) {
        Ok(()) => {
            assert!(forge.is_dir(), "cache clean must preserve its retained root");
            assert!(
                std::fs::read_dir(&forge)
                    .expect("read cleaned package-cache root")
                    .next()
                    .is_none(),
                "cache clean must remove every child without removing its root"
            );
            assert_eq!(
                std::fs::read(outside.join("keep")).expect("read external witness after cache clean"),
                b"keep outside",
                "cache clean must not follow a symlink stored inside the retained root"
            );
        }
        Err(Error::Container(error)) if explicit_container_capability_denial(&error) => {
            eprintln!("SKIP cache clean success path: explicit container capability denial: {error}");
            assert!(forge.is_dir(), "a denied cache clean must preserve its retained root");
            assert_eq!(
                std::fs::read(forge.join("nested/file")).expect("read retained cache child after denial"),
                b"remove"
            );
            assert!(
                std::fs::symlink_metadata(forge.join("outside-link"))
                    .expect("inspect retained cache symlink after denial")
                    .file_type()
                    .is_symlink(),
                "a denied cache clean must preserve the unprocessed symlink"
            );
            assert_eq!(
                std::fs::read(outside.join("keep")).expect("read external witness after denial"),
                b"keep outside"
            );
        }
        Err(error) => panic!("cache clean success path failed: {error}"),
    }

    assert_exact_main_task("harness-free cache-clean completion");

    fn assert_exact_main_task(context: &str) {
        let task_directory = format!("/proc/{}/task", process::id());
        let mut tasks = std::fs::read_dir(&task_directory)
            .unwrap_or_else(|source| panic!("enumerate {task_directory}: {source}"))
            .map(|entry| {
                let entry = entry.unwrap_or_else(|source| panic!("read {task_directory} entry: {source}"));
                entry
                    .file_name()
                    .to_str()
                    .and_then(|name| name.parse::<u32>().ok())
                    .unwrap_or_else(|| panic!("non-numeric task entry in {task_directory}"))
            })
            .collect::<Vec<_>>();
        tasks.sort_unstable();
        assert_eq!(tasks, [process::id()], "{context} was not an exact single-task process");
    }

    fn explicit_container_capability_denial(error: &container::Error) -> bool {
        error.execution_capability_unavailable()
    }
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::{PermissionsExt as _, symlink};

    use super::*;

    #[test]
    fn package_cache_clean_rejects_post_environment_symlink_without_touching_either_directory() {
        let root = crate::private_tempdir();
        let forge = root.path().join("forge");
        let env = Env::new(
            Some(root.path().join("build")),
            Some(root.path().join("config")),
            Some(root.path().join("data")),
            Some(forge.clone()),
        )
        .unwrap();
        std::fs::write(forge.join("original"), b"keep original").unwrap();
        let displaced = root.path().join("displaced-forge");
        std::fs::rename(&forge, &displaced).unwrap();

        let unrelated = root.path().join("unrelated");
        std::fs::create_dir(&unrelated).unwrap();
        std::fs::set_permissions(&unrelated, std::fs::Permissions::from_mode(0o700)).unwrap();
        std::fs::write(unrelated.join("keep"), b"keep unrelated").unwrap();
        symlink(&unrelated, &forge).unwrap();

        let error = clean(env, false, true).unwrap_err();

        assert!(matches!(error, Error::Io(_)));
        assert!(std::fs::symlink_metadata(&forge).unwrap().file_type().is_symlink());
        assert_eq!(std::fs::read(displaced.join("original")).unwrap(), b"keep original");
        assert_eq!(std::fs::read(unrelated.join("keep")).unwrap(), b"keep unrelated");
    }

    #[test]
    fn package_cache_clean_rejects_post_environment_directory_replacement_without_touching_either_directory() {
        let root = crate::private_tempdir();
        let forge = root.path().join("forge");
        let env = Env::new(
            Some(root.path().join("build")),
            Some(root.path().join("config")),
            Some(root.path().join("data")),
            Some(forge.clone()),
        )
        .unwrap();
        std::fs::write(forge.join("original"), b"keep original").unwrap();
        let displaced = root.path().join("displaced-forge");
        std::fs::rename(&forge, &displaced).unwrap();

        std::fs::create_dir(&forge).unwrap();
        std::fs::set_permissions(&forge, std::fs::Permissions::from_mode(0o700)).unwrap();
        std::fs::write(forge.join("replacement"), b"keep replacement").unwrap();

        let error = clean(env, false, true).unwrap_err();

        assert!(matches!(error, Error::Io(_)));
        assert_eq!(std::fs::read(displaced.join("original")).unwrap(), b"keep original");
        assert_eq!(std::fs::read(forge.join("replacement")).unwrap(), b"keep replacement");
    }
}
