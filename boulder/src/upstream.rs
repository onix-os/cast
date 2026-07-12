// SPDX-FileCopyrightText: 2026 AerynOS Developers
// SPDX-License-Identifier: MPL-2.0

use std::{io, path::Path, time::Duration};

use crate::{
    recipe::Recipe,
    source_lock::{
        self, ArchiveResolution, GitResolution, SOURCE_LOCK_FILE_NAME, SourceLock, SourceResolution, WriteOutcome,
    },
};
use fs_err as fs;
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
                    requested_ref: git_ref,
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

    if recipe.is_yaml_compatibility() {
        if let Some(updated_yaml) = update_git_upstream_refs(&recipe.source, &stored) {
            fs::write(&recipe.path, updated_yaml)?;
            println!(
                "{} | Git references resolved to commit hashes and saved to stone.yaml. This ensures reproducible builds since tags and branches can move over time.",
                "Warning".yellow()
            );
        }
    } else {
        require_current_source_lock(recipe, &stored)?;
    }

    Ok(stored)
}

fn require_current_source_lock(recipe: &Recipe, stored: &[Stored]) -> Result<(), Error> {
    if write_resolved_source_lock(recipe, stored)? == Some(WriteOutcome::Written) {
        return Err(Error::SourceLockChanged {
            path: recipe.path.with_file_name(SOURCE_LOCK_FILE_NAME),
        });
    }
    Ok(())
}

pub(crate) fn write_resolved_source_lock(recipe: &Recipe, stored: &[Stored]) -> Result<Option<WriteOutcome>, Error> {
    if recipe.is_yaml_compatibility() {
        return Ok(None);
    }

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
    Ok(Some(outcome))
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

/// Process git upstreams after cloning and return updated YAML if refs differ from resolved hashes.
pub(crate) fn update_git_upstream_refs(recipe_source: &str, stored_upstreams: &[Stored]) -> Option<String> {
    let mut yaml_updater = yaml::Updater::new();
    let mut refs_updated = false;

    for stored in stored_upstreams.iter() {
        if let Stored::Git(git) = stored
            && git.resolved_hash != git.original_ref
        {
            update_git_upstream_ref_in_yaml(
                &mut yaml_updater,
                git.original_index,
                git.url.as_str(),
                &git.resolved_hash,
                &git.original_ref,
            );
            println!(
                "{} | Updated ref '{}' to commit {} for {}",
                "Warning".yellow(),
                &git.resolved_hash[..8],
                git.original_ref,
                git.url.as_str()
            );
            refs_updated = true;
        }
    }

    if refs_updated {
        Some(yaml_updater.apply(recipe_source))
    } else {
        None
    }
}

/// Replaces the non-hash refs for git upstreams with the hash for the given ref
/// and includes a comment showing the original ref.
fn update_git_upstream_ref_in_yaml(
    updater: &mut yaml::Updater,
    upstream_index: usize,
    url: &str,
    new_ref: &str,
    original_ref: &str,
) {
    let git_key = format!("git|{url}");
    let new_value_with_comment = format!("{new_ref} # {original_ref}");

    // git|url: <ref>
    updater.update_value(&new_value_with_comment, |p| {
        p / "upstreams" / upstream_index / git_key.as_str()
    });

    // git|url:
    // - ref: <ref>
    // ...
    updater.update_value(&new_value_with_comment, |p| {
        p / "upstreams" / upstream_index / git_key.as_str() / "ref"
    });
}

#[cfg(test)]
mod tests {
    use crate::upstream::StoredGit;
    use crate::upstream::plain::StoredPlain;

    use super::*;

    use url::Url;

    const ARCHIVE_URL: &str = "https://example.com/source.tar.xz";
    const ARCHIVE_HASH: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const GIT_URL: &str = "https://example.com/source.git";
    const FULL_COMMIT: &str = "0123456789abcdef0123456789abcdef01234567";

    fn gluon_recipe() -> String {
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
    upstreams = [
        boulder.upstream.archive "{ARCHIVE_URL}" "{ARCHIVE_HASH}",
        boulder.upstream.git "{GIT_URL}" "main",
    ],
    .. base
}}"#
        )
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
                url: Url::parse(GIT_URL).unwrap(),
                original_ref: "main".to_owned(),
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
        let authored = gluon_recipe();
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
                if source.requested_ref == "main" && source.commit == FULL_COMMIT
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
    fn yaml_compatibility_never_writes_a_source_lock() {
        let directory = tempfile::tempdir().unwrap();
        fs::write(
            directory.path().join("stone.yaml"),
            r#"
name: example
version: "1.2.3"
release: 1
homepage: https://example.com
license: MPL-2.0
"#,
        )
        .unwrap();
        let recipe = Recipe::load(directory.path()).unwrap();

        assert_eq!(write_resolved_source_lock(&recipe, &[]).unwrap(), None);
        assert!(!directory.path().join(SOURCE_LOCK_FILE_NAME).exists());
    }

    #[test]
    fn test_update_git_upstream_refs() {
        let recipe_source = r#"
upstreams:
  - git|https://github.com/example/repo1.git: main
  - git|https://github.com/example/repo2.git:
      ref: main
  - git|https://github.com/example/repo3.git: abcd1234567890abcdef1234567890abcdef1234
  - git|https://github.com/example/repo4.git: abc123d
  - https://example.com/file.tar.gz: some-hash
"#;

        let stored = vec![
            Stored::Git(StoredGit {
                name: "repo1.git".to_owned(),
                repo: gitwrap::null_repository(),
                was_cached: false,
                url: Url::parse("https://github.com/example/repo1.git").unwrap(),
                original_ref: "main".to_owned(),
                resolved_hash: "1111222233334444555566667777888899990000".to_owned(),
                original_index: 0,
            }),
            Stored::Git(StoredGit {
                name: "repo2.git".to_owned(),
                repo: gitwrap::null_repository(),
                was_cached: false,
                url: Url::parse("https://github.com/example/repo2.git").unwrap(),
                original_ref: "main".to_owned(),
                resolved_hash: "aaaa1111bbbb2222cccc3333dddd4444eeee5555".to_owned(),
                original_index: 1,
            }),
            Stored::Git(StoredGit {
                name: "repo3.git".to_owned(),
                repo: gitwrap::null_repository(),
                was_cached: false,
                url: Url::parse("https://github.com/example/repo3.git").unwrap(),
                original_ref: "abcd1234567890abcdef1234567890abcdef1234".to_owned(),
                resolved_hash: "abcd1234567890abcdef1234567890abcdef1234".to_owned(),
                original_index: 2,
            }),
            Stored::Git(StoredGit {
                name: "repo4.git".to_owned(),
                repo: gitwrap::null_repository(),
                was_cached: false,
                url: Url::parse("https://github.com/example/repo4.git").unwrap(),
                original_ref: "abc123d".to_owned(),
                resolved_hash: "abc123d567890abcdef1234567890abcdef12345".to_owned(),
                original_index: 3,
            }),
            Stored::Git(StoredGit {
                name: "file.tar.gz".to_owned(),
                repo: gitwrap::null_repository(),
                was_cached: false,
                // We don't care about the values below.
                url: "http://example.com".try_into().unwrap(),
                original_ref: String::new(),
                resolved_hash: String::new(),
                original_index: 0,
            }),
        ];

        let result = update_git_upstream_refs(recipe_source, &stored);

        assert!(result.is_some());
        let updated_yaml = result.unwrap();

        // Should update short form ref to hash with comment
        assert!(updated_yaml.contains("1111222233334444555566667777888899990000 # main"));

        // Should update long form ref to hash with comment
        assert!(updated_yaml.contains("aaaa1111bbbb2222cccc3333dddd4444eeee5555 # main"));

        // Should not change hash that's already a hash
        assert!(updated_yaml.contains("abcd1234567890abcdef1234567890abcdef1234"));
        assert!(
            !updated_yaml
                .contains("abcd1234567890abcdef1234567890abcdef1234 # abcd1234567890abcdef1234567890abcdef1234")
        );

        // Should update short hash to long hash
        assert!(updated_yaml.contains("abc123d567890abcdef1234567890abcdef12345 # abc123d"));

        // Should preserve non-git upstreams unchanged
        assert!(updated_yaml.contains("https://example.com/file.tar.gz: some-hash"));
    }

    #[test]
    fn test_update_git_upstream_refs_no_updates() {
        let recipe_source = r#"
upstreams:
  - git|https://github.com/example/repo3.git: abcd1234567890abcdef1234567890abcdef1234
  - https://example.com/file.tar.gz: some-hash
"#;

        let stored = vec![
            Stored::Git(StoredGit {
                name: "repo3.git".to_owned(),
                repo: gitwrap::null_repository(),
                was_cached: false,
                url: Url::parse("https://github.com/example/repo3.git").unwrap(),
                original_ref: "abcd1234567890abcdef1234567890abcdef1234".to_owned(),
                resolved_hash: "abcd1234567890abcdef1234567890abcdef1234".to_owned(),
                original_index: 0,
            }),
            Stored::Plain(StoredPlain {
                name: "file.tar.gz".to_owned(),
                path: "/tmp/file.tar.gz".into(),
                was_cached: false,
            }),
        ];

        let result = update_git_upstream_refs(recipe_source, &stored);

        assert!(result.is_none());
    }
}
