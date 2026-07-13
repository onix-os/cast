// SPDX-FileCopyrightText: 2026 AerynOS Developers
// SPDX-License-Identifier: MPL-2.0

use std::{
    ffi::OsStr,
    fs::Permissions,
    io,
    os::unix::fs::{MetadataExt, PermissionsExt},
    path::{Path, PathBuf},
};

use fs_err as fs;
use moss::util;
use thiserror::Error;
use tui::{ProgressBar, ProgressStyle};
use url::Url;

/// Upstream based on a Git repository.
#[derive(Clone, Debug)]
pub struct Git {
    /// URL of origin.
    pub url: Url,
    /// Revision to fetch, pinned to the full commit when a source lock exists.
    pub commit: String,
    /// Exact directory name used when sharing this source with the build.
    pub name: String,
    pub original_index: usize,
}

impl Git {
    /// Returns the name of the upstream. It is implied from the URL.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Stores the upstream into the storage directory.
    /// If the upstream was already stored but does not include [Self::commit],
    /// it is updated contextually. If it does not exist, the Git repository is cloned.
    pub async fn store(&self, storage_dir: &Path, pb: &ProgressBar) -> Result<StoredGit, Error> {
        let repo: gitwrap::Repository;
        let mut cached = true;
        match self.stored(storage_dir).await {
            Ok((stored, has_commit)) => {
                repo = stored.repo;
                if !has_commit {
                    cached = false;
                    fetch(&repo, pb).await?;
                }
            }
            Err(Error::Git(_)) => {
                cached = false;
                self.remove(storage_dir)?;
                let stored_path = self.stored_path(storage_dir);
                if let Some(parent) = stored_path.parent() {
                    fs::create_dir_all(parent)?;
                }
                repo = clone(&self.url, &stored_path, pb).await?;
            }
            Err(Error::Io(e)) => return Err(Error::from(e)),
            Err(error @ Error::UnsupportedSubmodules { .. }) => return Err(error),
        }

        let resolved_hash = repo.peel_commit(&self.commit).await?;
        reject_gitlinks(&repo, &resolved_hash).await?;

        Ok(StoredGit {
            name: self.name().to_owned(),
            was_cached: cached,
            repo,
            resolved_hash,
            original_index: self.original_index,
        })
    }

    /// Resolve an authored moving reference against the current remote state.
    ///
    /// Normal build storage may reuse a lock-pinned commit without contacting
    /// the network. Explicit lock refreshes must instead fetch an existing
    /// mirror so branches and tags can advance before they are pinned again.
    pub async fn resolve(&self, storage_dir: &Path, pb: &ProgressBar) -> Result<StoredGit, Error> {
        let repo = match self.stored(storage_dir).await {
            Ok((stored, _)) => {
                fetch(&stored.repo, pb).await?;
                stored.repo
            }
            Err(Error::Git(_)) => {
                self.remove(storage_dir)?;
                let stored_path = self.stored_path(storage_dir);
                if let Some(parent) = stored_path.parent() {
                    fs::create_dir_all(parent)?;
                }
                clone(&self.url, &stored_path, pb).await?
            }
            Err(Error::Io(error)) => return Err(Error::Io(error)),
            Err(error @ Error::UnsupportedSubmodules { .. }) => return Err(error),
        };
        let resolved_hash = repo.peel_commit(&self.commit).await?;
        reject_gitlinks(&repo, &resolved_hash).await?;

        Ok(StoredGit {
            name: self.name().to_owned(),
            was_cached: false,
            repo,
            resolved_hash,
            original_index: self.original_index,
        })
    }

    /// Unconditionally removes the directory, within the storage
    /// directory, that would store the Git repository.
    /// If the directory does not exist, this function returns
    /// successfully (it is idempotent).
    ///
    /// Careful: this function does not validate the content
    /// of the directory! Resources will be deleted even if they
    /// do not belong to a Git repository.
    pub fn remove(&self, storage_dir: &Path) -> Result<(), Error> {
        let dir = self.stored_path(storage_dir);
        util::remove_dir_all(&dir).map_err(Error::from)
    }

    /// Returns the stored upstream if it exists.
    ///
    /// If successful, a tuple is returned containing the
    /// stored upstream and a boolean flag, indicating whether
    /// the stored Git repository contains [Self::commit].
    pub async fn stored(&self, storage_dir: &Path) -> Result<(StoredGit, bool), Error> {
        let repo = gitwrap::Repository::open_bare(&self.stored_path(storage_dir)).await?;
        let has_ref = repo.has_commit(&self.commit).await?;
        let resolved_hash = repo.peel_commit(&self.commit).await?;
        Ok((
            StoredGit {
                name: self.name().to_owned(),
                was_cached: has_ref,
                repo,
                resolved_hash,
                original_index: self.original_index,
            },
            has_ref,
        ))
    }

    /// Returns a relative PathBuf where this Git repository
    /// should be stored within the storage directory.
    fn stored_path(&self, storage_dir: &Path) -> PathBuf {
        storage_dir.join("git").join(self.directory_name())
    }

    /// Returns the name of the directory that should contain
    /// the Git repository.
    /// It is a composition of the hostname and the repository name
    /// so that it's unique.
    fn directory_name(&self) -> PathBuf {
        let host = self.url.host_str();
        let path = self.url.path();

        let mut name = String::with_capacity(host.unwrap_or("").len() + 1 + path.len());
        if let Some(host) = host {
            name.push_str(host);
            name.push('_');
        }
        name.push_str(&path.trim_start_matches('/').replace('/', "."));
        name.into()
    }
}

/// Information available after [Git] is stored on disk.
pub struct StoredGit {
    /// Name of the upstream, as returned by [Git::name].
    pub name: String,
    /// Whether the stored Git repository was
    /// synchronized with [Git],
    /// that is, it existed and contained [Git::commit].
    pub was_cached: bool,
    pub resolved_hash: String,
    pub original_index: usize,
    pub repo: gitwrap::Repository,
}

impl StoredGit {
    /// Shares the Git repository in preparation of a build.
    pub async fn share(&self, dest_dir: &Path, source_date_epoch: i64) -> Result<(), Error> {
        reject_gitlinks(&self.repo, &self.resolved_hash).await?;
        if let Some(parent) = dest_dir.parent() {
            fs::create_dir_all(parent)?;
        }

        // Clone from our mirror to destdir
        let cloned = self.repo.clone_to(dest_dir).await?;

        // Cloning sets origin to the local mirror, but we want to use
        // the original remote as submodule resolving may depend on this
        let source_origin = self.repo.get_remote_url("origin").await?;
        cloned.set_remote_url("origin", &source_origin).await?;

        // Finally checkout the desired commit
        cloned.checkout(&self.resolved_hash).await?;

        // Git administration data contains checkout-time and host-specific
        // state and is not part of the locked commit tree.
        remove_git_administration(dest_dir)?;
        normalize_exported_tree(dest_dir, source_date_epoch)?;

        Ok(())
    }
}

async fn reject_gitlinks(repo: &gitwrap::Repository, commit: &str) -> Result<(), Error> {
    if repo.contains_gitlinks(commit).await? {
        Err(Error::UnsupportedSubmodules {
            commit: commit.to_owned(),
        })
    } else {
        Ok(())
    }
}

fn remove_git_administration(root: &Path) -> Result<(), Error> {
    let entries = walkdir::WalkDir::new(root)
        .contents_first(true)
        .follow_links(false)
        .into_iter()
        .collect::<Result<Vec<_>, _>>()
        .map_err(walk_error)?;
    for entry in entries {
        if entry.file_name() != OsStr::new(".git") {
            continue;
        }
        let metadata = fs::symlink_metadata(entry.path())?;
        if metadata.is_dir() {
            fs::remove_dir_all(entry.path())?;
        } else {
            fs::remove_file(entry.path())?;
        }
    }
    Ok(())
}

fn normalize_exported_tree(root: &Path, source_date_epoch: i64) -> Result<(), Error> {
    let timestamp = filetime::FileTime::from_unix_time(source_date_epoch, 0);
    for entry in walkdir::WalkDir::new(root).contents_first(true).follow_links(false) {
        let entry = entry.map_err(walk_error)?;
        let path = entry.path();
        let metadata = fs::symlink_metadata(path)?;
        if metadata.file_type().is_symlink() {
            filetime::set_symlink_file_times(path, timestamp, timestamp)?;
            continue;
        }

        let mode = if metadata.is_dir() {
            0o755
        } else if metadata.mode() & 0o111 == 0 {
            0o644
        } else {
            0o755
        };
        fs::set_permissions(path, Permissions::from_mode(mode))?;
        filetime::set_file_times(path, timestamp, timestamp)?;
    }
    Ok(())
}

fn walk_error(error: walkdir::Error) -> io::Error {
    let message = error.to_string();
    error.into_io_error().unwrap_or_else(|| io::Error::other(message))
}

/// Possible errors returned by functions in this module.
#[derive(Debug, Error)]
pub enum Error {
    /// An error occurred while handling a Git repository.
    #[error("{0}")]
    Git(#[from] gitwrap::Error),
    /// Submodules require their own explicit, locked source model.
    #[error("Git commit {commit} contains submodules, which are not supported as implicit sources")]
    UnsupportedSubmodules { commit: String },
    /// A generic I/O error occurred.
    #[error("{0}")]
    Io(#[from] io::Error),
}

async fn clone(url: &Url, path: &Path, pb: &ProgressBar) -> Result<gitwrap::Repository, gitwrap::Error> {
    let cb = set_progress_bar_style(pb);

    let result = gitwrap::Repository::clone_mirror_progress(path, url, cb).await;
    pb.finish_and_clear();

    result
}

async fn fetch(repo: &gitwrap::Repository, pb: &ProgressBar) -> Result<(), gitwrap::Error> {
    let cb = set_progress_bar_style(pb);

    let result = repo.fetch_progress(cb).await;
    pb.finish_and_clear();

    result
}

fn set_progress_bar_style(pb: &ProgressBar) -> impl Fn(gitwrap::FetchProgress) {
    pb.set_length(100);
    pb.set_style(
        ProgressStyle::with_template(" {spinner} |{percent:>3}%| {wide_msg} {prefix:>.dim} ")
            .unwrap()
            .tick_chars("--=≡■≡=--"),
    );

    |prog| {
        pb.set_position(prog.percent as u64);
        pb.set_prefix(prog.speed);
    }
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::{MetadataExt, symlink};

    use super::*;

    #[test]
    fn exported_git_tree_has_fixed_modes_and_timestamps_without_admin_state() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("checkout");
        fs::create_dir_all(root.join(".git/objects")).unwrap();
        fs::create_dir_all(root.join("nested")).unwrap();
        fs::write(root.join("nested/.git"), b"gitdir: ../.git/modules/nested\n").unwrap();
        fs::write(root.join("regular"), b"regular").unwrap();
        fs::write(root.join("executable"), b"executable").unwrap();
        fs::set_permissions(root.join("regular"), Permissions::from_mode(0o666)).unwrap();
        fs::set_permissions(root.join("executable"), Permissions::from_mode(0o711)).unwrap();
        symlink("regular", root.join("link")).unwrap();
        fs::write(root.join(".git-marker"), b"ordinary committed name").unwrap();

        remove_git_administration(&root).unwrap();
        normalize_exported_tree(&root, 1_700_000_000).unwrap();

        assert_eq!(fs::metadata(root.join("regular")).unwrap().mode() & 0o7777, 0o644);
        assert_eq!(fs::metadata(root.join("executable")).unwrap().mode() & 0o7777, 0o755);
        assert_eq!(fs::metadata(root.join("nested")).unwrap().mode() & 0o7777, 0o755);
        for path in [root.clone(), root.join("regular"), root.join("executable")] {
            assert_eq!(fs::metadata(path).unwrap().mtime(), 1_700_000_000);
        }
        assert!(!root.join(".git").exists());
        assert!(!root.join("nested/.git").exists());
        assert!(
            fs::symlink_metadata(root.join("link"))
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert!(root.join(".git-marker").is_file());
    }
}
