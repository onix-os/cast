// SPDX-FileCopyrightText: 2026 AerynOS Developers
// SPDX-License-Identifier: MPL-2.0

use std::{io, path::Path, time::Duration};

use crate::{
    recipe::Recipe,
    source_lock::{
        self, ArchiveResolution, GitResolution, SOURCE_LOCK_FILE_NAME, SourceLock, SourceResolution, WriteOutcome,
    },
};
use futures_util::{StreamExt, TryStreamExt, stream};
use moss::runtime;
use stone_recipe::upstream;
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
    /// Constructs an [Upstream] based on the information provided
    /// in the `upstream` section of a Stone recipe.
    pub fn from_recipe_upstream(
        upstream: upstream::Upstream,
        original_index: usize,
        locked_commit: Option<&str>,
    ) -> Result<Self, Error> {
        match upstream.props {
            upstream::Props::Plain { hash, rename, .. } => Ok(Self::Plain(Plain {
                url: upstream.url,
                hash: hash.parse().map_err(plain::Error::from)?,
                rename,
            })),
            upstream::Props::Git { git_ref, .. } => {
                let commit = locked_commit.unwrap_or(&git_ref).to_owned();
                Ok(Self::Git(Git {
                    url: upstream.url,
                    commit,
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

    /// Unconditionally removes this Upstream's resources within the storage directory.
    /// If the resources do not exist, this function returns successfully
    /// (it is idempotent).
    ///
    /// Careful: this function does not validate the content!
    /// It will be removed even if it does not belong to this Upstream.
    fn remove(&self, storage_dir: &Path) -> Result<(), Error> {
        match self {
            Upstream::Plain(plain) => plain.remove(storage_dir).map_err(Error::from),
            Upstream::Git(git) => git.remove(storage_dir).map_err(Error::from),
        }
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
    /// This function tries to be as efficient as possible in terms
    /// of actual bytes written/copied, by linking files from the storage directory.
    async fn share(&self, dest_dir: &Path) -> Result<(), Error> {
        match self {
            Stored::Plain(plain) => plain.share(dest_dir)?,
            Stored::Git(git) => git.share(&dest_dir.join(&git.name)).await?,
        }
        Ok(())
    }
}

/// Returns a list of upstream from a Stone recipe.
pub fn parse_recipe(recipe: &Recipe) -> Result<Vec<Upstream>, Error> {
    recipe
        .parsed
        .upstreams
        .iter()
        .cloned()
        .enumerate()
        .map(|(index, upstream)| {
            let locked_commit = recipe
                .source_lock
                .as_ref()
                .and_then(|lock| lock.source(index))
                .and_then(|source| match source {
                    SourceResolution::Git(source) => Some(source.commit.as_str()),
                    SourceResolution::Archive(_) => None,
                });
            Upstream::from_recipe_upstream(upstream, index, locked_commit)
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

/// Helper that stores and shares a list of [Upstream]s.
pub fn sync(
    recipe: &Recipe,
    upstreams: &[Upstream],
    storage_dir: &Path,
    share_dir: &Path,
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

                stored.share(share_dir).await?;

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
    println!();

    require_current_source_lock(recipe, &stored)?;

    Ok(stored)
}

fn require_current_source_lock(recipe: &Recipe, stored: &[Stored]) -> Result<(), Error> {
    if write_resolved_source_lock(recipe, stored)? == WriteOutcome::Written {
        return Err(Error::SourceLockChanged {
            path: recipe.path.with_file_name(SOURCE_LOCK_FILE_NAME),
        });
    }
    Ok(())
}

pub(crate) fn write_resolved_source_lock(recipe: &Recipe, stored: &[Stored]) -> Result<WriteOutcome, Error> {
    let sources = recipe
        .parsed
        .upstreams
        .iter()
        .enumerate()
        .map(|(index, upstream)| {
            let order = u32::try_from(index).map_err(|_| Error::SourceOrderTooLarge(index))?;
            Ok(match &upstream.props {
                upstream::Props::Plain { hash, .. } => SourceResolution::Archive(ArchiveResolution {
                    order,
                    url: upstream.url.as_str().to_owned(),
                    sha256: hash.clone(),
                }),
                upstream::Props::Git { git_ref, .. } => {
                    let stored = stored
                        .iter()
                        .find_map(|stored| match stored {
                            Stored::Git(stored) if stored.original_index == index => Some(stored),
                            _ => None,
                        })
                        .ok_or(Error::MissingStoredSource(index))?;
                    SourceResolution::Git(GitResolution {
                        order,
                        url: upstream.url.as_str().to_owned(),
                        requested_ref: git_ref.clone(),
                        commit: stored.resolved_hash.clone(),
                    })
                }
            })
        })
        .collect::<Result<Vec<_>, Error>>()?;
    let lock = SourceLock::new(sources);
    lock.validate_against(&recipe.parsed)
        .map_err(|error| Error::GeneratedSourceLock(Box::new(error)))?;

    let path = recipe.path.with_file_name(SOURCE_LOCK_FILE_NAME);
    let outcome =
        source_lock::write_source_lock(&path, &lock).map_err(|source| Error::WriteSourceLock { path, source })?;
    Ok(outcome)
}

/// Helper that removes a list of [Upstream]s from the storage directory.
pub fn remove(storage_dir: &Path, upstreams: &[Upstream]) -> Result<(), Error> {
    for upstream in upstreams {
        upstream.remove(storage_dir)?;
    }
    Ok(())
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
    #[error("Gluon source lock {path:?} was created or updated; rerun the build to bind it into provenance")]
    SourceLockChanged { path: std::path::PathBuf },
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
    const AUTHORED_SOURCE_FIXTURE: &str = include_str!("../../test/fixtures/gluon/authored-source.glu");

    fn gluon_git_recipe(url: &str) -> String {
        format!(
            r#"let boulder = import! boulder.recipe.v1
let base = boulder.recipe (boulder.source {{
    name = "example",
    version = "1.2.3",
    release = 1,
    homepage = "https://example.com",
    license = ["MPL-2.0"],
}})
{{
    upstreams = [boulder.upstream.git "{url}" "main"],
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
    fn missing_lock_is_generated_without_mutating_source_and_then_consumed() {
        let directory = tempfile::tempdir().unwrap();
        let recipe_path = directory.path().join("stone.glu");
        let lock_path = directory.path().join(SOURCE_LOCK_FILE_NAME);
        let authored = AUTHORED_SOURCE_FIXTURE.to_owned();
        fs::write(&recipe_path, &authored).unwrap();

        let recipe = Recipe::load(directory.path()).unwrap();
        assert!(recipe.source_lock.is_none());
        let stored = resolved_upstreams();

        let error = require_current_source_lock(&recipe, &stored).unwrap_err();
        assert!(matches!(
            error,
            Error::SourceLockChanged { path } if path == lock_path
        ));
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
        require_current_source_lock(&loaded, &stored).unwrap();
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
}
