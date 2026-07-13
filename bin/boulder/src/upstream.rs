// SPDX-FileCopyrightText: 2026 AerynOS Developers
// SPDX-License-Identifier: MPL-2.0

use std::{fs::Permissions, io, os::unix::fs::PermissionsExt, path::Path, time::Duration};

use crate::{
    recipe::Recipe,
    source_lock::{
        self, ArchiveResolution, GitResolution, SOURCE_LOCK_FILE_NAME, SourceLock, SourceResolution, WriteOutcome,
    },
};
use futures_util::{StreamExt, TryStreamExt, stream};
use moss::runtime;
use stone_recipe::{UpstreamSpec, derivation::LockedSource};
use thiserror::Error;
use tui::{MultiProgress, ProgressBar, ProgressStyle, Styled};

use crate::upstream::{
    git::{Git, StoredGit},
    plain::{Plain, StoredPlain},
};

mod git;
mod plain;

/// An upstream is a backend where
/// to get source code from.
#[derive(Debug, Clone)]
pub enum Upstream {
    /// An archive containing source code, typically
    /// a tarball. In order to be usable, it must compatible with
    /// [bsdtar](https://man.freebsd.org/cgi/man.cgi?query=bsdtar&sektion=1&format=html).
    Plain(Plain),
    /// The source code is from a Git repository.
    Git(Git),
}

impl Upstream {
    /// Constructs an upstream from one concrete package-v3 source request.
    pub fn from_package_source(
        source: &UpstreamSpec,
        original_index: usize,
        locked_commit: Option<&str>,
    ) -> Result<Self, Error> {
        match source {
            UpstreamSpec::Archive { url, hash, rename, .. } => Ok(Self::Plain(Plain {
                url: url.parse().map_err(|source| Error::AuthoredSourceUrl {
                    index: original_index,
                    source,
                })?,
                hash: hash.parse().map_err(plain::Error::from)?,
                rename: rename.clone(),
            })),
            UpstreamSpec::Git { url, git_ref, .. } => {
                let commit = locked_commit.unwrap_or(git_ref).to_owned();
                let url = url.parse().map_err(|source| Error::AuthoredSourceUrl {
                    index: original_index,
                    source,
                })?;
                let name = moss::util::uri_file_name(&url).to_owned();
                Ok(Self::Git(Git {
                    url,
                    commit,
                    name,
                    original_index,
                }))
            }
        }
    }

    /// Returns the name of the upstream. This is an informal
    /// name used for logging or when a path to be created
    /// doesn't need to be unique.
    fn name(&self) -> &str {
        match self {
            Upstream::Plain(plain) => plain.name(),
            Upstream::Git(git) => git.name(),
        }
    }

    /// Stores the upstream into the storage directory.
    /// The final path contained in the storage directory, and the write logic,
    /// depend on the upstream variant. The final path where the upstream is stored
    /// is unique inside the storage directory.
    async fn store(&self, storage_dir: &Path, pb: &ProgressBar) -> Result<Stored, Error> {
        Ok(match self {
            Upstream::Plain(plain) => Stored::Plain(plain.store(storage_dir, pb).await?),
            Upstream::Git(git) => Stored::Git(git.store(storage_dir, pb).await?),
        })
    }

    /// Store an authored source while refreshing moving Git references.
    async fn resolve(&self, storage_dir: &Path, pb: &ProgressBar) -> Result<Stored, Error> {
        Ok(match self {
            Upstream::Plain(plain) => Stored::Plain(plain.store(storage_dir, pb).await?),
            Upstream::Git(git) => Stored::Git(git.resolve(storage_dir, pb).await?),
        })
    }
}

/// Information available after [Upstream] is stored on disk.
pub(crate) enum Stored {
    Plain(StoredPlain),
    Git(StoredGit),
}

impl Stored {
    /// Whether the upstream did not need to be written at the moment
    /// of being stored, because the constant was already there and valid.
    fn was_cached(&self) -> bool {
        match self {
            Stored::Plain(plain) => plain.was_cached,
            Stored::Git(git) => git.was_cached,
        }
    }

    /// Shares the upstream in preparation of a build.
    ///
    /// Build-visible source files always receive independent inodes so a build
    /// cannot mutate the verified persistent cache.
    async fn share(&self, dest_dir: &Path, source_date_epoch: i64) -> Result<(), Error> {
        match self {
            Stored::Plain(plain) => plain.share(dest_dir, source_date_epoch)?,
            Stored::Git(git) => {
                git.share(&dest_dir.join(&git.name), source_date_epoch).await?;
            }
        }
        Ok(())
    }
}

/// Returns a list of upstream from a Stone recipe.
pub fn parse_recipe(recipe: &Recipe) -> Result<Vec<Upstream>, Error> {
    recipe
        .declaration
        .sources
        .iter()
        .enumerate()
        .map(|(index, source)| {
            let locked_commit = recipe
                .source_lock
                .as_ref()
                .and_then(|lock| lock.source(index))
                .and_then(|source| match source {
                    SourceResolution::Git(source) => Some(source.commit.as_str()),
                    SourceResolution::Archive(_) => None,
                });
            Upstream::from_package_source(source, index, locked_commit)
        })
        .collect()
}

/// Resolve every authored upstream and atomically refresh its generated lock.
///
/// Callers must load the recipe through [`Recipe::load_authored`] so an old or
/// malformed generated lock cannot pin Git resolution or block regeneration.
pub(crate) fn refresh_source_lock(recipe: &Recipe, storage_dir: &Path) -> Result<WriteOutcome, Error> {
    let upstreams = parse_recipe(recipe)?;
    println!();
    println!("Resolving {} upstream(s) for {SOURCE_LOCK_FILE_NAME}:", upstreams.len());

    let progress = MultiProgress::new();
    let stored = runtime::block_on(
        stream::iter(&upstreams)
            .map(|upstream| {
                let progress = &progress;
                async move {
                    let bar = progress.add(
                        ProgressBar::new(u64::MAX)
                            .with_message(format!("{} {}", "Resolving".blue(), upstream.name().bold()))
                            .with_style(
                                ProgressStyle::with_template(" {spinner} {wide_msg} {binary_bytes_per_sec:>.dim} ")
                                    .unwrap()
                                    .tick_chars("--=≡■≡=--"),
                            ),
                    );
                    bar.enable_steady_tick(Duration::from_millis(150));
                    let stored = upstream.resolve(storage_dir, &bar).await;
                    bar.finish_and_clear();
                    progress.remove(&bar);
                    stored
                }
            })
            .buffer_unordered(moss::environment::MAX_NETWORK_CONCURRENCY)
            .try_collect::<Vec<_>>(),
    )?;
    progress.clear()?;

    write_resolved_source_lock(recipe, &stored)
}

/// Fetch and share only the exact source identities frozen into a derivation.
///
/// This path does not load or rewrite authored recipe/source-lock state.
pub fn sync_locked(
    sources: &[LockedSource],
    storage_dir: &Path,
    share_dir: &Path,
    source_date_epoch: i64,
) -> Result<Vec<Stored>, Error> {
    let upstreams = locked_upstreams(sources)?;
    sync_upstreams(&upstreams, storage_dir, share_dir, source_date_epoch)
}

fn locked_upstreams(sources: &[LockedSource]) -> Result<Vec<Upstream>, Error> {
    sources
        .iter()
        .enumerate()
        .map(|(index, source)| {
            Ok(match source {
                LockedSource::Archive {
                    url, sha256, filename, ..
                } => Upstream::Plain(Plain {
                    url: url.parse().map_err(|source| Error::LockedSourceUrl { index, source })?,
                    hash: sha256.parse().map_err(plain::Error::from)?,
                    rename: Some(filename.clone()),
                }),
                LockedSource::Git {
                    url, commit, directory, ..
                } => Upstream::Git(Git {
                    url: url.parse().map_err(|source| Error::LockedSourceUrl { index, source })?,
                    commit: commit.clone(),
                    name: directory.clone(),
                    original_index: index,
                }),
            })
        })
        .collect()
}

fn sync_upstreams(
    upstreams: &[Upstream],
    storage_dir: &Path,
    share_dir: &Path,
    source_date_epoch: i64,
) -> Result<Vec<Stored>, Error> {
    println!();
    println!("Sharing {} upstream(s) with the build container:", upstreams.len());

    let mp = MultiProgress::new();
    let tp = mp.add(
        ProgressBar::new(upstreams.len() as u64).with_style(
            ProgressStyle::with_template("\n|{bar:20.cyan/blue}| {pos}/{len}")
                .unwrap()
                .progress_chars("■≡=- "),
        ),
    );
    tp.tick();

    let stored = runtime::block_on(
        stream::iter(upstreams)
            .map(|upstream| async {
                let pb = mp.insert_before(
                    &tp,
                    ProgressBar::new(u64::MAX).with_message(format!(
                        "{} {}",
                        "Downloading".blue(),
                        upstream.name().bold(),
                    )),
                );
                pb.enable_steady_tick(Duration::from_millis(150));

                let stored = upstream.store(storage_dir, &pb).await?;

                pb.set_message(format!("{} {}", "Copying".yellow(), upstream.name().bold()));
                pb.set_style(
                    ProgressStyle::with_template(" {spinner} {wide_msg} ")
                        .unwrap()
                        .tick_chars("--=≡■≡=--"),
                );

                stored.share(share_dir, source_date_epoch).await?;

                let cached_tag = stored
                    .was_cached()
                    .then_some(format!("{}", " (cached)".dim()))
                    .unwrap_or_default();

                pb.finish();
                mp.remove(&pb);
                mp.suspend(|| println!("{} {}{cached_tag}", "Shared".green(), upstream.name().bold()));
                tp.inc(1);

                Ok(stored) as Result<_, Error>
            })
            .buffer_unordered(moss::environment::MAX_NETWORK_CONCURRENCY)
            .try_collect::<Vec<_>>(),
    )?;

    mp.clear()?;
    normalize_share_root(share_dir, source_date_epoch)?;
    println!();

    Ok(stored)
}

fn normalize_share_root(share_dir: &Path, source_date_epoch: i64) -> Result<(), Error> {
    fs_err::create_dir_all(share_dir)?;
    fs_err::set_permissions(share_dir, Permissions::from_mode(0o755))?;
    let timestamp = filetime::FileTime::from_unix_time(source_date_epoch, 0);
    filetime::set_file_times(share_dir, timestamp, timestamp)?;
    Ok(())
}

pub(crate) fn write_resolved_source_lock(recipe: &Recipe, stored: &[Stored]) -> Result<WriteOutcome, Error> {
    let sources = recipe
        .declaration
        .sources
        .iter()
        .enumerate()
        .map(|(index, source)| {
            let order = u32::try_from(index).map_err(|_| Error::SourceOrderTooLarge(index))?;
            Ok(match source {
                UpstreamSpec::Archive { url, hash, .. } => SourceResolution::Archive(ArchiveResolution {
                    order,
                    url: url.clone(),
                    sha256: hash.clone(),
                }),
                UpstreamSpec::Git { url, git_ref, .. } => {
                    let stored = stored
                        .iter()
                        .find_map(|stored| match stored {
                            Stored::Git(stored) if stored.original_index == index => Some(stored),
                            _ => None,
                        })
                        .ok_or(Error::MissingStoredSource(index))?;
                    SourceResolution::Git(GitResolution {
                        order,
                        url: url.clone(),
                        requested_ref: git_ref.clone(),
                        commit: stored.resolved_hash.clone(),
                    })
                }
            })
        })
        .collect::<Result<Vec<_>, Error>>()?;
    let lock = SourceLock::new(sources);
    lock.validate_against(&recipe.declaration.sources)
        .map_err(|error| Error::GeneratedSourceLock(Box::new(error)))?;

    let path = recipe.path.with_file_name(SOURCE_LOCK_FILE_NAME);
    let outcome =
        source_lock::write_source_lock(&path, &lock).map_err(|source| Error::WriteSourceLock { path, source })?;
    Ok(outcome)
}

/// Possible errors returned by functions in this module.
#[derive(Debug, Error)]
pub enum Error {
    #[error("plain")]
    /// An error occurred while dealing with an archive-based [Upstream].
    Plain(#[from] plain::Error),
    /// An error occurred while dealing with a Git-based [Upstream].
    #[error("git")]
    Git(#[from] git::Error),
    #[error("source order {0} cannot be represented in a source lock")]
    SourceOrderTooLarge(usize),
    #[error("resolved Git source {0} is missing after synchronization")]
    MissingStoredSource(usize),
    #[error("validate generated source lock")]
    GeneratedSourceLock(#[source] Box<source_lock::ValidationError>),
    #[error("write Gluon source lock {path:?}")]
    WriteSourceLock {
        path: std::path::PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("locked source {index} has an invalid URL")]
    LockedSourceUrl {
        index: usize,
        #[source]
        source: url::ParseError,
    },
    #[error("authored source {index} has an invalid URL")]
    AuthoredSourceUrl {
        index: usize,
        #[source]
        source: url::ParseError,
    },
    #[error("io")]
    // A generic I/O error occurred.
    Io(#[from] io::Error),
}

#[cfg(test)]
mod tests {
    use std::{path::Path, process::Command};

    use crate::upstream::StoredGit;
    use crate::upstream::plain::StoredPlain;

    use super::*;

    use fs_err as fs;
    const FULL_COMMIT: &str = "0123456789abcdef0123456789abcdef01234567";
    const AUTHORED_SOURCE_FIXTURE: &str = include_str!("../../../tests/fixtures/gluon/authored-source.glu");

    fn gluon_git_recipe(url: &str) -> String {
        format!(
            r#"let boulder = import! boulder.package.v3
let base = boulder.mk_package (boulder.meta {{
    pname = "example",
    version = "1.2.3",
    release = 1,
    homepage = "https://example.com",
    license = ["MPL-2.0"],
}})
{{
    sources = [boulder.source.git "{url}" "main"],
    .. base
}}"#
        )
    }

    fn git(repository: &Path, arguments: &[&str]) -> String {
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

    fn commit(repository: &Path, contents: &str, message: &str) -> String {
        fs::write(repository.join("source.txt"), contents).unwrap();
        git(repository, &["add", "source.txt"]);
        git(repository, &["commit", "-m", message]);
        git(repository, &["rev-parse", "HEAD"])
    }

    fn resolved_upstreams() -> Vec<Stored> {
        vec![
            Stored::Plain(StoredPlain {
                name: "source.tar.xz".to_owned(),
                path: "/tmp/source.tar.xz".into(),
                was_cached: false,
            }),
            Stored::Git(StoredGit {
                name: "source.git".to_owned(),
                repo: gitwrap::null_repository(),
                was_cached: false,
                resolved_hash: FULL_COMMIT.to_owned(),
                original_index: 1,
            }),
        ]
    }

    #[test]
    fn locked_sources_preserve_exact_materialization_names() {
        let sources = vec![
            LockedSource::Archive {
                order: 0,
                url: "https://example.invalid/source.tar.zst".to_owned(),
                sha256: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_owned(),
                filename: "renamed.tar.zst".to_owned(),
            },
            LockedSource::Git {
                order: 1,
                url: "https://example.invalid/source.git".to_owned(),
                requested_ref: "main".to_owned(),
                commit: FULL_COMMIT.to_owned(),
                directory: "frozen-source.git".to_owned(),
            },
        ];

        let upstreams = locked_upstreams(&sources).unwrap();
        assert_eq!(upstreams[0].name(), "renamed.tar.zst");
        assert_eq!(upstreams[1].name(), "frozen-source.git");
        let Upstream::Git(git) = &upstreams[1] else {
            panic!("expected locked Git source")
        };
        assert_eq!(git.commit, FULL_COMMIT);
    }

    #[test]
    fn source_root_mode_and_timestamp_are_reproducible() {
        use std::os::unix::fs::MetadataExt;

        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("sources");
        normalize_share_root(&root, 1_700_000_000).unwrap();

        let metadata = fs::metadata(root).unwrap();
        assert_eq!(metadata.mode() & 0o7777, 0o755);
        assert_eq!(metadata.mtime(), 1_700_000_000);
    }

    #[test]
    fn missing_lock_is_generated_without_mutating_source_and_then_consumed() {
        let directory = tempfile::tempdir().unwrap();
        let recipe_path = directory.path().join("stone.glu");
        let lock_path = directory.path().join(SOURCE_LOCK_FILE_NAME);
        let authored = AUTHORED_SOURCE_FIXTURE.to_owned();
        fs::write(&recipe_path, &authored).unwrap();

        let recipe = Recipe::load(directory.path()).unwrap();
        assert!(recipe.source_lock.is_none());
        let stored = resolved_upstreams();

        assert_eq!(
            write_resolved_source_lock(&recipe, &stored).unwrap(),
            WriteOutcome::Written
        );
        assert_eq!(fs::read_to_string(&recipe_path).unwrap(), authored);

        let lock_bytes = fs::read(&lock_path).unwrap();
        let lock = source_lock::decode_source_lock(SOURCE_LOCK_FILE_NAME, &lock_bytes).unwrap();
        assert_eq!(lock.sources.len(), 2);
        assert!(matches!(
            &lock.sources[1],
            SourceResolution::Git(source)
                if source.requested_ref == "main" && source.commit == FULL_COMMIT
        ));

        let loaded = Recipe::load(directory.path()).unwrap();
        let parsed = parse_recipe(&loaded).unwrap();
        assert!(matches!(
            &parsed[1],
            Upstream::Git(source)
                if source.commit == FULL_COMMIT
        ));

        let before = fs::metadata(&lock_path).unwrap();
        assert_eq!(
            write_resolved_source_lock(&loaded, &stored).unwrap(),
            WriteOutcome::Unchanged
        );
        let after = fs::metadata(&lock_path).unwrap();

        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            assert_eq!(before.ino(), after.ino());
        }
    }

    #[test]
    fn explicit_refresh_fetches_and_pins_the_latest_git_commit() {
        let directory = tempfile::tempdir().unwrap();
        let origin = directory.path().join("origin");
        fs::create_dir(&origin).unwrap();
        git(&origin, &["init", "--initial-branch=main"]);
        git(&origin, &["config", "user.name", "Boulder Test"]);
        git(&origin, &["config", "user.email", "boulder@example.invalid"]);
        let first_commit = commit(&origin, "first\n", "first");
        let url = url::Url::from_file_path(&origin).unwrap().to_string();
        let authored = gluon_git_recipe(&url);
        let recipe_path = directory.path().join("stone.glu");
        let lock_path = directory.path().join(SOURCE_LOCK_FILE_NAME);
        let storage = directory.path().join("cache/upstreams");
        fs::write(&recipe_path, &authored).unwrap();

        let recipe = Recipe::load_authored(&recipe_path).unwrap();
        assert_eq!(refresh_source_lock(&recipe, &storage).unwrap(), WriteOutcome::Written);
        let first = source_lock::decode_source_lock(SOURCE_LOCK_FILE_NAME, &fs::read(&lock_path).unwrap()).unwrap();
        assert!(matches!(
            &first.sources[0],
            SourceResolution::Git(source)
                if source.requested_ref == "main" && source.commit == first_commit
        ));

        let second_commit = commit(&origin, "second\n", "second");
        let recipe = Recipe::load_authored(&recipe_path).unwrap();
        assert_eq!(refresh_source_lock(&recipe, &storage).unwrap(), WriteOutcome::Written);
        let second = source_lock::decode_source_lock(SOURCE_LOCK_FILE_NAME, &fs::read(&lock_path).unwrap()).unwrap();
        assert!(matches!(
            &second.sources[0],
            SourceResolution::Git(source)
                if source.requested_ref == "main" && source.commit == second_commit
        ));
        assert_ne!(first_commit, second_commit);
        assert_eq!(fs::read_to_string(recipe_path).unwrap(), authored);
    }

    #[test]
    fn source_lock_refresh_rejects_implicit_git_submodules() {
        let directory = tempfile::tempdir().unwrap();
        let origin = directory.path().join("origin");
        fs::create_dir(&origin).unwrap();
        git(&origin, &["init", "--initial-branch=main"]);
        git(&origin, &["config", "user.name", "Boulder Test"]);
        git(&origin, &["config", "user.email", "boulder@example.invalid"]);
        let dependency_commit = commit(&origin, "source\n", "source");
        let cache_info = format!("160000,{dependency_commit},vendor/dependency");
        git(&origin, &["update-index", "--add", "--cacheinfo", &cache_info]);
        git(&origin, &["commit", "-m", "implicit submodule"]);
        let commit = git(&origin, &["rev-parse", "HEAD"]);

        let recipe_path = directory.path().join("stone.glu");
        let lock_path = directory.path().join(SOURCE_LOCK_FILE_NAME);
        let storage = directory.path().join("cache/upstreams");
        let url = url::Url::from_file_path(&origin).unwrap().to_string();
        fs::write(&recipe_path, gluon_git_recipe(&url)).unwrap();
        let recipe = Recipe::load_authored(&recipe_path).unwrap();

        let error = refresh_source_lock(&recipe, &storage).unwrap_err();

        assert!(matches!(
            error,
            Error::Git(git::Error::UnsupportedSubmodules { commit: rejected }) if rejected == commit
        ));
        assert!(!lock_path.exists());
    }
}
