// SPDX-FileCopyrightText: 2026 AerynOS Developers
// SPDX-License-Identifier: MPL-2.0

use std::{
    ffi::{CString, OsStr},
    io,
    os::unix::ffi::OsStrExt,
    path::{Path, PathBuf},
};

use fs_err as fs;
use moss::util;
use sha2::{Digest, Sha256};
use thiserror::Error;
use tui::{ProgressBar, ProgressStyle};
use url::Url;

mod materialization;

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
    /// Expected normalized checkout identity. Authored moving references have
    /// no value until explicit lock refresh materializes them.
    pub materialization_sha256: Option<String>,
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
            Err(Error::Git(_) | Error::OriginMismatch { .. }) => {
                cached = false;
                self.remove(storage_dir)?;
                let stored_path = self.stored_path(storage_dir);
                if let Some(parent) = stored_path.parent() {
                    fs::create_dir_all(parent)?;
                }
                repo = clone(&self.url, &stored_path, pb).await?;
            }
            Err(error) => return Err(error),
        }

        let resolved_hash = repo.peel_commit(&self.commit).await?;
        reject_gitlinks(&repo, &resolved_hash).await?;

        Ok(StoredGit {
            name: self.name().to_owned(),
            was_cached: cached,
            repo,
            resolved_hash,
            original_index: self.original_index,
            materialization_sha256: self.materialization_sha256.clone(),
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
            Err(Error::Git(_) | Error::OriginMismatch { .. }) => {
                self.remove(storage_dir)?;
                let stored_path = self.stored_path(storage_dir);
                if let Some(parent) = stored_path.parent() {
                    fs::create_dir_all(parent)?;
                }
                clone(&self.url, &stored_path, pb).await?
            }
            Err(error) => return Err(error),
        };
        let resolved_hash = repo.peel_commit(&self.commit).await?;
        reject_gitlinks(&repo, &resolved_hash).await?;

        Ok(StoredGit {
            name: self.name().to_owned(),
            was_cached: false,
            repo,
            resolved_hash,
            original_index: self.original_index,
            materialization_sha256: self.materialization_sha256.clone(),
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
        let stored_path = self.stored_path(storage_dir);
        let repo = gitwrap::Repository::open_bare(&stored_path).await?;
        let origin = repo.get_remote_url("origin").await?;
        if origin != self.url.as_str() {
            return Err(Error::OriginMismatch { cache: stored_path });
        }
        let has_ref = repo.has_commit(&self.commit).await?;
        let resolved_hash = repo.peel_commit(&self.commit).await?;
        Ok((
            StoredGit {
                name: self.name().to_owned(),
                was_cached: has_ref,
                repo,
                resolved_hash,
                original_index: self.original_index,
                materialization_sha256: self.materialization_sha256.clone(),
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
    /// The readable prefix is deliberately cosmetic. The SHA-256 suffix binds
    /// the cache identity to every byte of the canonical URL, including its
    /// scheme, authority, port, path, query, and user information.
    fn directory_name(&self) -> PathBuf {
        const MAX_READABLE_BYTES: usize = 48;

        let basename = self
            .url
            .path_segments()
            .and_then(|segments| segments.filter(|segment| !segment.is_empty()).next_back())
            .unwrap_or("repository");
        let basename = basename.strip_suffix(".git").unwrap_or(basename);
        let mut readable = basename
            .chars()
            .map(|character| {
                if character.is_ascii_alphanumeric() || matches!(character, '-' | '_') {
                    character
                } else {
                    '-'
                }
            })
            .take(MAX_READABLE_BYTES)
            .collect::<String>();
        while readable.ends_with('-') || readable.ends_with('_') {
            readable.pop();
        }
        let first_safe = readable
            .find(|character: char| character.is_ascii_alphanumeric())
            .unwrap_or(readable.len());
        readable.drain(..first_safe);
        if readable.is_empty() {
            readable.push_str("repository");
        }

        let digest = Sha256::digest(self.url.as_str().as_bytes());
        format!("{readable}-{digest:x}").into()
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
    pub materialization_sha256: Option<String>,
    pub repo: gitwrap::Repository,
}

impl StoredGit {
    /// Export one exact commit, remove Git administration data, normalize the
    /// build-visible tree, and return its canonical SHA-256 identity.
    pub(crate) async fn export_normalized(&self, dest_dir: &Path, source_date_epoch: i64) -> Result<String, Error> {
        self.export_normalized_with_root_access(dest_dir, source_date_epoch, false)
            .await
    }

    async fn export_normalized_with_root_access(
        &self,
        dest_dir: &Path,
        source_date_epoch: i64,
        descriptor_rooted: bool,
    ) -> Result<String, Error> {
        reject_gitlinks(&self.repo, &self.resolved_hash).await?;
        match fs::symlink_metadata(dest_dir) {
            Ok(_) => return Err(Error::DestinationExists(dest_dir.to_owned())),
            Err(source) if source.kind() == io::ErrorKind::NotFound => {}
            Err(source) => return Err(source.into()),
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
        let digest = if descriptor_rooted {
            materialization::normalize_and_hash_descriptor_path(dest_dir, source_date_epoch)
        } else {
            materialization::normalize_and_hash(dest_dir, source_date_epoch)
        };
        digest.map_err(|source| Error::Materialization {
            root: dest_dir.to_owned(),
            source,
        })
    }

    /// Shares the exact Git repository in preparation of a frozen build and
    /// rejects any checkout whose normalized bytes differ from the source lock.
    pub async fn share(&self, dest_dir: &Path, source_date_epoch: i64) -> Result<(), Error> {
        let expected = self
            .materialization_sha256
            .as_deref()
            .ok_or_else(|| Error::MissingMaterializationDigest {
                index: self.original_index,
                commit: self.resolved_hash.clone(),
            })?;
        let parent = dest_dir
            .parent()
            .ok_or_else(|| Error::MissingDestinationParent(dest_dir.to_owned()))?;
        let staging = tempfile::Builder::new()
            .prefix(".boulder-git-")
            .tempdir_in(parent)
            .map_err(|source| Error::CreateStaging {
                parent: parent.to_owned(),
                source,
            })?;
        let checkout = staging.path().join("checkout");
        let found = self
            .export_normalized_with_root_access(&checkout, source_date_epoch, true)
            .await?;
        if found != expected {
            return Err(Error::MaterializationDigestMismatch {
                index: self.original_index,
                commit: self.resolved_hash.clone(),
                expected: expected.to_owned(),
                found,
            });
        }
        rename_noreplace(&checkout, dest_dir).map_err(|source| Error::Install {
            source_path: checkout,
            destination: dest_dir.to_owned(),
            source,
        })?;

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
        // The export directory itself may legitimately be named `.git` by an
        // authored clone_dir. Only administration entries *inside* the export
        // are removable.
        .min_depth(1)
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

fn walk_error(error: walkdir::Error) -> io::Error {
    let message = error.to_string();
    error.into_io_error().unwrap_or_else(|| io::Error::other(message))
}

/// Atomically install a verified checkout without ever replacing or following
/// a destination that appeared after the source-root preflight.
fn rename_noreplace(source: &Path, target: &Path) -> io::Result<()> {
    let source = CString::new(source.as_os_str().as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "staged checkout path contains NUL"))?;
    let target = CString::new(target.as_os_str().as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "final checkout path contains NUL"))?;
    // nix exposes renameat2 only on some libc targets. Boulder supports musl,
    // so use the Linux syscall directly with RENAME_NOREPLACE.
    let result = unsafe {
        nix::libc::syscall(
            nix::libc::SYS_renameat2,
            nix::libc::AT_FDCWD,
            source.as_ptr(),
            nix::libc::AT_FDCWD,
            target.as_ptr(),
            1_u32, // RENAME_NOREPLACE
        )
    };
    if result == -1 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

/// Possible errors returned by functions in this module.
#[derive(Debug, Error)]
pub enum Error {
    /// An error occurred while handling a Git repository.
    #[error("{0}")]
    Git(#[from] gitwrap::Error),
    /// A cache entry belongs to a different canonical source URL.
    #[error("cached Git mirror at {cache:?} does not belong to the requested source URL")]
    OriginMismatch { cache: PathBuf },
    /// Submodules require their own explicit, locked source model.
    #[error("Git commit {commit} contains submodules, which are not supported as implicit sources")]
    UnsupportedSubmodules { commit: String },
    /// A frozen source has no expected normalized-tree identity.
    #[error("Git source {index} at commit {commit} has no locked materialization digest")]
    MissingMaterializationDigest { index: usize, commit: String },
    /// A caller attempted to export over an existing path of any type.
    #[error("refusing to export Git source over existing destination {0:?}")]
    DestinationExists(PathBuf),
    /// A build-visible checkout destination had no containing directory.
    #[error("Git checkout destination has no parent: {0:?}")]
    MissingDestinationParent(PathBuf),
    /// A private staging directory could not be created beside the final path.
    #[error("create private Git checkout staging directory in {parent:?}")]
    CreateStaging {
        parent: PathBuf,
        #[source]
        source: io::Error,
    },
    /// The verified staging tree could not be installed atomically.
    #[error("atomically install verified Git checkout from {source_path:?} at {destination:?}")]
    Install {
        source_path: PathBuf,
        destination: PathBuf,
        #[source]
        source: io::Error,
    },
    /// The normalized checkout differs from the bytes admitted by lock refresh.
    #[error("Git source {index} at commit {commit} materialized as {found}, but sources.lock.glu requires {expected}")]
    MaterializationDigestMismatch {
        index: usize,
        commit: String,
        expected: String,
        found: String,
    },
    /// Canonical tree normalization or hashing failed.
    #[error("normalize and hash Git materialization at {root:?}")]
    Materialization {
        root: PathBuf,
        #[source]
        source: materialization::Error,
    },
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
    use std::{collections::HashSet, os::unix::fs::symlink, process::Command};

    use super::*;

    fn fixture_git(repository: &Path, arguments: &[&str]) -> String {
        let output = Command::new("git")
            .arg("-C")
            .arg(repository)
            .args(arguments)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {arguments:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap().trim().to_owned()
    }

    fn source(url: Url) -> Git {
        Git {
            url,
            commit: "HEAD".to_owned(),
            name: "source".to_owned(),
            original_index: 0,
            materialization_sha256: None,
        }
    }

    fn create_repository(path: &Path, contents: &[u8]) -> String {
        fs::create_dir(path).unwrap();
        fixture_git(path, &["init", "--initial-branch=main"]);
        fixture_git(path, &["config", "user.name", "Boulder Test"]);
        fixture_git(path, &["config", "user.email", "boulder@example.invalid"]);
        fs::write(path.join("source.txt"), contents).unwrap();
        fixture_git(path, &["add", "source.txt"]);
        fixture_git(path, &["commit", "-m", "source"]);
        fixture_git(path, &["rev-parse", "HEAD"])
    }

    #[test]
    fn cache_identity_binds_the_complete_canonical_url() {
        let urls = [
            "https://alice:secret@example.invalid:8443/org/repo.git?transport=one#release",
            "http://alice:secret@example.invalid:8443/org/repo.git?transport=one#release",
            "https://bob:secret@example.invalid:8443/org/repo.git?transport=one#release",
            "https://alice:different@example.invalid:8443/org/repo.git?transport=one#release",
            "https://alice:secret@example.invalid:9443/org/repo.git?transport=one#release",
            "https://alice:secret@example.invalid:8443/other/repo.git?transport=one#release",
            "https://alice:secret@example.invalid:8443/org/repo.git?transport=two#release",
            "https://alice:secret@example.invalid:8443/org/repo.git?transport=one#other",
        ]
        .map(|url| Url::parse(url).unwrap());

        let names = urls
            .iter()
            .map(|url| {
                let name = source(url.clone()).directory_name();
                let name = name.to_str().unwrap();
                let expected_digest = format!("{:x}", Sha256::digest(url.as_str().as_bytes()));
                assert!(name.starts_with("repo-"));
                assert!(name.ends_with(&expected_digest));
                assert!(
                    name.bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
                );
                assert!(!name.contains("alice"));
                assert!(!name.contains("secret"));
                name.to_owned()
            })
            .collect::<HashSet<_>>();

        assert_eq!(names.len(), urls.len());
    }

    #[test]
    fn cache_identity_never_uses_unsafe_url_path_bytes() {
        let url = Url::parse("https://example.invalid/a/%2E%2E/%2Fbad%5Cname%00.git?path=/tmp/escape").unwrap();
        let name = source(url).directory_name();
        let name = name.to_str().unwrap();

        assert_eq!(Path::new(name).components().count(), 1);
        assert!(
            name.bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
        );
        assert!(!matches!(name, "." | ".."));
    }

    #[tokio::test]
    async fn mismatched_cache_origin_is_rejected_and_repaired_before_reuse() {
        let temporary = tempfile::tempdir().unwrap();
        let requested_path = temporary.path().join("requested");
        let wrong_path = temporary.path().join("wrong");
        let requested_commit = create_repository(&requested_path, b"requested source\n");
        create_repository(&wrong_path, b"wrong source\n");
        let requested_url = Url::from_directory_path(&requested_path).unwrap();
        let wrong_url = Url::from_directory_path(&wrong_path).unwrap();
        let requested = source(requested_url.clone());
        let storage = temporary.path().join("storage");
        let cached_path = requested.stored_path(&storage);
        fs::create_dir_all(cached_path.parent().unwrap()).unwrap();
        gitwrap::Repository::clone_mirror(&cached_path, &wrong_url)
            .await
            .unwrap();

        match requested.stored(&storage).await {
            Err(Error::OriginMismatch { cache }) => assert_eq!(cache, cached_path),
            Err(error) => panic!("unexpected cache error: {error}"),
            Ok(_) => panic!("a mirror for another origin was accepted"),
        }

        let stored = requested.store(&storage, &ProgressBar::new(100)).await.unwrap();
        assert_eq!(
            stored.repo.get_remote_url("origin").await.unwrap(),
            requested_url.as_str()
        );
        assert_eq!(stored.resolved_hash, requested_commit);
    }

    #[test]
    fn verified_checkout_install_never_replaces_a_destination_symlink() {
        let temporary = tempfile::tempdir().unwrap();
        let source = temporary.path().join("staged");
        let destination = temporary.path().join("destination");
        let outside = temporary.path().join("outside");
        fs::create_dir(&source).unwrap();
        fs::write(source.join("source.txt"), b"verified").unwrap();
        fs::create_dir(&outside).unwrap();
        symlink(&outside, &destination).unwrap();

        let error = rename_noreplace(&source, &destination).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
        assert!(source.join("source.txt").is_file());
        assert!(fs::symlink_metadata(&destination).unwrap().file_type().is_symlink());
        assert!(fs::read_dir(outside).unwrap().next().is_none());
    }

    #[tokio::test]
    async fn failed_materialization_verification_leaves_no_checkout_or_staging_tree() {
        let temporary = tempfile::tempdir().unwrap();
        let source_path = temporary.path().join("source");
        let commit = create_repository(&source_path, b"locked source\n");
        let source_url = Url::from_directory_path(&source_path).unwrap();
        let mirror_path = temporary.path().join("mirror.git");
        let repo = gitwrap::Repository::clone_mirror(&mirror_path, &source_url)
            .await
            .unwrap();
        let stored = StoredGit {
            name: "source".to_owned(),
            was_cached: false,
            resolved_hash: commit,
            original_index: 0,
            materialization_sha256: Some("0".repeat(64)),
            repo,
        };
        let share_root = temporary.path().join("share");
        fs::create_dir(&share_root).unwrap();

        assert!(matches!(
            stored.share(&share_root.join("source"), 0).await,
            Err(Error::MaterializationDigestMismatch { .. })
        ));
        assert!(fs::read_dir(share_root).unwrap().next().is_none());
    }

    #[test]
    fn exported_git_tree_removes_only_git_administration_state() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join(".git");
        fs::create_dir_all(root.join(".git/objects")).unwrap();
        fs::create_dir_all(root.join("nested")).unwrap();
        fs::write(root.join("nested/.git"), b"gitdir: ../.git/modules/nested\n").unwrap();
        fs::write(root.join("regular"), b"regular").unwrap();
        symlink("regular", root.join("link")).unwrap();
        fs::write(root.join(".git-marker"), b"ordinary committed name").unwrap();

        remove_git_administration(&root).unwrap();

        assert!(root.is_dir());
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
