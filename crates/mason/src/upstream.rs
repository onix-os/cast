// SPDX-FileCopyrightText: 2026 AerynOS Developers

use std::{io, path::Path, time::Duration};

use crate::{
    recipe::Recipe,
    source_lock::{
        self, ArchiveResolution, GitResolution, SOURCE_LOCK_FILE_NAME, SourceLock, SourceResolution, WriteOutcome,
    },
};
use forge::runtime;
use futures_util::{StreamExt, TryStreamExt, stream};
use stone_recipe::{
    UpstreamSpec,
    derivation::LockedSource,
    spec::{SourceUrlKind, SourceUrlValidationError, UpstreamValidationError, validate_source_url},
};
use thiserror::Error;
use tui::{MultiProgress, ProgressBar, ProgressStyle, Styled};

use crate::upstream::{
    git::{Git, StoredGit},
    plain::{Plain, StoredPlain},
    share_root::ShareRoot,
};

mod git;
mod plain;
mod share_root;

pub(crate) use plain::ARCHIVE_DOWNLOAD_LIMITS;

#[cfg(feature = "delegated-fixture-test-support")]
const _: [(); 1] = [(); gitwrap::FIXTURE_TEST_SUPPORT_ENABLED as usize];

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
        locked_git: Option<(&str, &str)>,
    ) -> Result<Self, Error> {
        let validated_url = source.validated_url().map_err(|source| Error::InvalidAuthoredSource {
            index: original_index,
            source,
        })?;
        let materialization_name = source
            .materialization_name()
            .map_err(|source| Error::InvalidAuthoredSource {
                index: original_index,
                source,
            })?;
        match source {
            UpstreamSpec::Archive { hash, .. } => Ok(Self::Plain(Plain {
                url: validated_url,
                hash: hash.parse().map_err(plain::Error::from)?,
                // Resolve URL-derived defaults exactly once at the validated
                // package boundary.
                rename: Some(materialization_name),
            })),
            UpstreamSpec::Git { git_ref, .. } => {
                let (commit, materialization_sha256) = locked_git
                    .map(|(commit, digest)| (commit.to_owned(), Some(digest.to_owned())))
                    .unwrap_or_else(|| (git_ref.clone(), None));
                Ok(Self::Git(Git {
                    url: validated_url,
                    commit,
                    name: materialization_name,
                    original_index,
                    materialization_sha256,
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
    async fn share(&self, share_root: &ShareRoot, source_date_epoch: i64) -> Result<(), Error> {
        match self {
            Stored::Plain(plain) => plain.share(share_root.descriptor_path(), source_date_epoch)?,
            Stored::Git(git) => {
                git.share_into_root(share_root.directory(), share_root.descriptor_path(), source_date_epoch)
                    .await?;
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
            let locked_git = recipe
                .source_lock
                .as_ref()
                .and_then(|lock| lock.source(index))
                .and_then(|source| match source {
                    SourceResolution::Git(source) => {
                        Some((source.commit.as_str(), source.materialization_sha256.as_str()))
                    }
                    SourceResolution::Archive(_) => None,
                });
            Upstream::from_package_source(source, index, locked_git)
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
                    let stored: Result<Stored, Error> = async {
                        let stored = upstream.resolve(storage_dir, &bar).await?;
                        match stored {
                            Stored::Git(mut git) => {
                                let temporary = tempfile::tempdir()?;
                                let destination = temporary.path().join(&git.name);
                                git.materialization_sha256 = Some(git.export_normalized(&destination, 0).await?);
                                Ok(Stored::Git(git))
                            }
                            stored @ Stored::Plain(_) => Ok(stored),
                        }
                    }
                    .await;
                    bar.finish_and_clear();
                    progress.remove(&bar);
                    stored
                }
            })
            .buffer_unordered(forge::environment::MAX_NETWORK_CONCURRENCY)
            .try_collect::<Vec<_>>(),
    )?;
    progress.clear()?;

    write_resolved_source_lock(recipe, &stored)
}

/// Fetch and share only the exact source identities frozen into a derivation.
///
/// This path does not load or rewrite authored recipe/source-lock state.
#[cfg(test)]
pub fn sync_locked(
    sources: &[LockedSource],
    storage_dir: &Path,
    share_dir: &Path,
    source_date_epoch: i64,
) -> Result<Vec<Stored>, Error> {
    let upstreams = locked_upstreams(sources)?;
    sync_upstreams(&upstreams, storage_dir, share_dir, source_date_epoch)
}

/// Fetch and share the exact locked sources beneath the descriptor retained by
/// one Forge materialization.  The configured global root pathname is never
/// reopened to create or populate the build-visible source directory.
pub fn sync_locked_into_root(
    sources: &[LockedSource],
    storage_dir: &Path,
    root: &forge::MaterializedFrozenRoot,
    share_dir: &Path,
    source_date_epoch: i64,
) -> Result<Vec<Stored>, Error> {
    let upstreams = locked_upstreams(sources)?;
    root.revalidate().map_err(share_root::Error::MaterializedRoot)?;
    let share_root = ShareRoot::prepare_in(root, share_dir)?;
    let stored = sync_upstreams_into(&upstreams, storage_dir, &share_root, source_date_epoch)?;
    root.revalidate().map_err(share_root::Error::MaterializedRoot)?;
    Ok(stored)
}

#[cfg(any(test, feature = "delegated-fixture-test-support"))]
include!("upstream/fixture_import.rs");

fn locked_upstreams(sources: &[LockedSource]) -> Result<Vec<Upstream>, Error> {
    sources
        .iter()
        .enumerate()
        .map(|(index, source)| {
            Ok(match source {
                LockedSource::Archive {
                    url, sha256, filename, ..
                } => Upstream::Plain(Plain {
                    url: validate_source_url(SourceUrlKind::Archive, url)
                        .map_err(|source| Error::InvalidLockedSourceUrl { index, source })?,
                    hash: sha256.parse().map_err(plain::Error::from)?,
                    rename: Some(filename.clone()),
                }),
                LockedSource::Git {
                    url,
                    commit,
                    materialization_sha256,
                    directory,
                    ..
                } => Upstream::Git(Git {
                    url: validate_source_url(SourceUrlKind::Git, url)
                        .map_err(|source| Error::InvalidLockedSourceUrl { index, source })?,
                    commit: commit.clone(),
                    name: directory.clone(),
                    original_index: index,
                    materialization_sha256: Some(materialization_sha256.clone()),
                }),
            })
        })
        .collect()
}

#[cfg(test)]
fn sync_upstreams(
    upstreams: &[Upstream],
    storage_dir: &Path,
    share_dir: &Path,
    source_date_epoch: i64,
) -> Result<Vec<Stored>, Error> {
    let share_root = ShareRoot::prepare(share_dir)?;
    sync_upstreams_into(upstreams, storage_dir, &share_root, source_date_epoch)
}

fn sync_upstreams_into(
    upstreams: &[Upstream],
    storage_dir: &Path,
    share_root: &ShareRoot,
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

                stored.share(share_root, source_date_epoch).await?;

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
            .buffer_unordered(forge::environment::MAX_NETWORK_CONCURRENCY)
            .try_collect::<Vec<_>>(),
    )?;

    mp.clear()?;
    share_root.normalize_and_verify(source_date_epoch)?;
    println!();

    Ok(stored)
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
                        materialization_sha256: stored
                            .materialization_sha256
                            .clone()
                            .ok_or(Error::MissingMaterializationDigest(index))?,
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
    #[error("resolved Git source {0} has no canonical materialization digest")]
    MissingMaterializationDigest(usize),
    #[error("validate generated source lock")]
    GeneratedSourceLock(#[source] Box<source_lock::ValidationError>),
    #[error("write Gluon source lock {path:?}")]
    WriteSourceLock {
        path: std::path::PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("locked source {index}.url is invalid: {source}")]
    InvalidLockedSourceUrl {
        index: usize,
        #[source]
        source: SourceUrlValidationError,
    },
    #[error("authored source {index} is invalid: {source}")]
    InvalidAuthoredSource {
        index: usize,
        #[source]
        source: UpstreamValidationError,
    },
    #[cfg(any(test, feature = "delegated-fixture-test-support"))]
    #[error("offline fixture import requires one locked archive source")]
    FixtureImportRequiresArchive,
    #[cfg(any(test, feature = "delegated-fixture-test-support"))]
    #[error("offline Git fixture import requires one locked Git source")]
    FixtureImportRequiresGit,
    #[error("prepare build-visible source root")]
    ShareRoot(#[from] share_root::Error),
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
    const SECOND_COMMIT: &str = "89abcdef0123456789abcdef0123456789abcdef";
    const MATERIALIZATION_SHA256: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
    const SECOND_MATERIALIZATION_SHA256: &str = "cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc";
    const AUTHORED_SOURCE_FIXTURE: &str = include_str!("../../../tests/fixtures/gluon/authored-source.glu");

    fn gluon_two_git_recipe(first_url: &str, second_url: &str) -> String {
        format!(
            r#"let cast = import! cast.package.v3
let base = cast.mk_package (cast.meta {{
    pname = "example",
    version = "1.2.3",
    release = 1,
    homepage = "https://example.com",
    license = ["MPL-2.0"],
}})
{{
    sources = [
        cast.source.git "{first_url}" "main",
        cast.source.git "{second_url}" "stable",
    ],
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
                hash: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
                    .parse()
                    .unwrap(),
                was_cached: false,
            }),
            Stored::Git(StoredGit {
                name: "source.git".to_owned(),
                repo: gitwrap::null_repository(),
                was_cached: false,
                resolved_hash: FULL_COMMIT.to_owned(),
                original_index: 1,
                materialization_sha256: Some(MATERIALIZATION_SHA256.to_owned()),
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
                materialization_sha256: MATERIALIZATION_SHA256.to_owned(),
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
        assert_eq!(git.materialization_sha256.as_deref(), Some(MATERIALIZATION_SHA256));
    }

    #[test]
    fn offline_fixture_import_preserves_https_identity_and_rejects_git() {
        use sha2::{Digest, Sha256};

        let directory = tempfile::tempdir().unwrap();
        let fixture = directory.path().join("source.tar");
        let storage = directory.path().join("storage");
        fs::write(&fixture, b"offline archive").unwrap();
        let archive = LockedSource::Archive {
            order: 0,
            url: "https://fixtures.invalid/source.tar".to_owned(),
            sha256: hex::encode(Sha256::digest(b"offline archive")),
            filename: "source.tar".to_owned(),
        };

        import_locked_archive_fixture(&archive, &storage, &fixture).unwrap();
        let upstreams = locked_upstreams(std::slice::from_ref(&archive)).unwrap();
        let [Upstream::Plain(plain)] = upstreams.as_slice() else {
            panic!("locked archive did not remain a plain HTTPS upstream");
        };
        assert_eq!(plain.url.scheme(), "https");
        assert!(plain.stored(&storage).is_ok());

        let git = LockedSource::Git {
            order: 0,
            url: "https://fixtures.invalid/source.git".to_owned(),
            requested_ref: "main".to_owned(),
            commit: FULL_COMMIT.to_owned(),
            materialization_sha256: MATERIALIZATION_SHA256.to_owned(),
            directory: "source.git".to_owned(),
        };
        assert!(matches!(
            import_locked_archive_fixture(&git, &storage, &fixture),
            Err(Error::FixtureImportRequiresArchive)
        ));
    }

    #[test]
    fn locked_source_url_policy_fails_before_any_storage_or_share_access() {
        let cases = [
            LockedSource::Archive {
                order: 0,
                url: "file:///tmp/source.tar.zst".to_owned(),
                sha256: "a".repeat(64),
                filename: "source.tar.zst".to_owned(),
            },
            LockedSource::Archive {
                order: 0,
                url: "http://example.invalid/source.tar.zst".to_owned(),
                sha256: "a".repeat(64),
                filename: "source.tar.zst".to_owned(),
            },
            LockedSource::Git {
                order: 0,
                url: "file:///tmp/source.git".to_owned(),
                requested_ref: "main".to_owned(),
                commit: FULL_COMMIT.to_owned(),
                materialization_sha256: MATERIALIZATION_SHA256.to_owned(),
                directory: "source.git".to_owned(),
            },
            LockedSource::Git {
                order: 0,
                url: "git://example.invalid/source.git".to_owned(),
                requested_ref: "main".to_owned(),
                commit: FULL_COMMIT.to_owned(),
                materialization_sha256: MATERIALIZATION_SHA256.to_owned(),
                directory: "source.git".to_owned(),
            },
        ];

        for source in cases {
            let directory = tempfile::tempdir().unwrap();
            let storage = directory.path().join("storage");
            let shared = directory.path().join("shared");
            let error = match sync_locked(&[source], &storage, &shared, 0) {
                Ok(_) => panic!("insecure locked source URL was accepted"),
                Err(error) => error,
            };
            assert!(matches!(
                error,
                Error::InvalidLockedSourceUrl {
                    index: 0,
                    source: SourceUrlValidationError::UnsupportedScheme { .. },
                }
            ));
            assert!(!storage.exists());
            assert!(!shared.exists());
        }
    }

    #[test]
    fn fetch_boundary_url_errors_are_field_specific_and_secret_free() {
        for value in [
            "https://user:do-not-print@example.invalid/source.tar.zst",
            "https://example.invalid/source.tar.zst#do-not-print",
        ] {
            let source = UpstreamSpec::Archive {
                url: value.to_owned(),
                hash: "a".repeat(64),
                rename: None,
                strip_dirs: None,
                unpack: true,
                unpack_dir: None,
            };
            let error = Upstream::from_package_source(&source, 7, None).unwrap_err();
            let message = error.to_string();
            assert!(matches!(error, Error::InvalidAuthoredSource { index: 7, .. }));
            assert!(message.starts_with("authored source 7 is invalid:"));
            assert!(!message.contains("user"));
            assert!(!message.contains("do-not-print"));
        }
    }

    #[test]
    fn authored_git_clone_dir_is_the_materialization_name() {
        let source = UpstreamSpec::Git {
            url: "https://example.invalid/source.git".to_owned(),
            git_ref: "main".to_owned(),
            clone_dir: Some("chosen-source".to_owned()),
        };

        let upstream = Upstream::from_package_source(&source, 0, None).unwrap();

        assert_eq!(upstream.name(), "chosen-source");
        let Upstream::Git(git) = upstream else {
            panic!("expected Git source")
        };
        assert_eq!(git.materialization_sha256, None);
    }

    #[test]
    fn source_root_mode_and_timestamp_are_reproducible() {
        use std::os::unix::fs::MetadataExt;

        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("sources");
        let share_root = ShareRoot::prepare(&root).unwrap();
        share_root.normalize_and_verify(1_700_000_000).unwrap();

        let metadata = fs::metadata(root).unwrap();
        assert_eq!(metadata.mode() & 0o7777, 0o755);
        assert_eq!(metadata.mtime(), 1_700_000_000);
    }

    #[test]
    fn source_sync_rejects_share_root_and_destination_symlink_escapes() {
        use std::os::unix::fs::symlink;

        let temporary = tempfile::tempdir().unwrap();
        let outside = temporary.path().join("outside");
        fs::create_dir(&outside).unwrap();
        let linked_root = temporary.path().join("linked-root");
        symlink(&outside, &linked_root).unwrap();

        let error = match sync_upstreams(&[], &temporary.path().join("cache"), &linked_root, 0) {
            Ok(_) => panic!("symlink source root was accepted"),
            Err(error) => error,
        };
        assert!(matches!(error, Error::ShareRoot(share_root::Error::Open { .. })));
        assert!(fs::read_dir(&outside).unwrap().next().is_none());

        let root = temporary.path().join("root");
        fs::create_dir(&root).unwrap();
        symlink(&outside, root.join("source.git")).unwrap();
        let error = match sync_upstreams(&[], &temporary.path().join("cache"), &root, 0) {
            Ok(_) => panic!("pre-existing source destination was accepted"),
            Err(error) => error,
        };
        assert!(matches!(
            error,
            Error::ShareRoot(share_root::Error::NotEmpty(path)) if path == root
        ));
        assert!(fs::read_dir(&outside).unwrap().next().is_none());
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
                if source.requested_ref == "main"
                    && source.commit == FULL_COMMIT
                    && source.materialization_sha256 == MATERIALIZATION_SHA256
        ));

        let loaded = Recipe::load(directory.path()).unwrap();
        let parsed = parse_recipe(&loaded).unwrap();
        assert!(matches!(
            &parsed[1],
            Upstream::Git(source)
                if source.commit == FULL_COMMIT
                    && source.materialization_sha256.as_deref() == Some(MATERIALIZATION_SHA256)
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
    fn resolved_git_materializations_follow_authored_indices_not_completion_order() {
        let directory = tempfile::tempdir().unwrap();
        let recipe_path = directory.path().join("stone.glu");
        let first_url = "https://example.invalid/first.git";
        let second_url = "https://example.invalid/second.git";
        fs::write(&recipe_path, gluon_two_git_recipe(first_url, second_url)).unwrap();
        let recipe = Recipe::load_authored(&recipe_path).unwrap();

        let stored = vec![
            Stored::Git(StoredGit {
                name: "second.git".to_owned(),
                repo: gitwrap::null_repository(),
                was_cached: false,
                resolved_hash: SECOND_COMMIT.to_owned(),
                original_index: 1,
                materialization_sha256: Some(SECOND_MATERIALIZATION_SHA256.to_owned()),
            }),
            Stored::Git(StoredGit {
                name: "first.git".to_owned(),
                repo: gitwrap::null_repository(),
                was_cached: false,
                resolved_hash: FULL_COMMIT.to_owned(),
                original_index: 0,
                materialization_sha256: Some(MATERIALIZATION_SHA256.to_owned()),
            }),
        ];

        write_resolved_source_lock(&recipe, &stored).unwrap();
        let lock = source_lock::decode_source_lock(
            SOURCE_LOCK_FILE_NAME,
            &fs::read(directory.path().join(SOURCE_LOCK_FILE_NAME)).unwrap(),
        )
        .unwrap();

        assert!(matches!(
            &lock.sources[0],
            SourceResolution::Git(source)
                if source.url == first_url
                    && source.commit == FULL_COMMIT
                    && source.materialization_sha256 == MATERIALIZATION_SHA256
        ));
        assert!(matches!(
            &lock.sources[1],
            SourceResolution::Git(source)
                if source.url == second_url
                    && source.commit == SECOND_COMMIT
                    && source.materialization_sha256 == SECOND_MATERIALIZATION_SHA256
        ));
    }

    #[test]
    fn local_git_lower_layer_resolves_and_verifies_moving_refs() {
        let directory = tempfile::tempdir().unwrap();
        let origin = directory.path().join("origin");
        fs::create_dir(&origin).unwrap();
        git(&origin, &["init", "--initial-branch=main"]);
        git(&origin, &["config", "user.name", "Cast Test"]);
        git(&origin, &["config", "user.email", "cast@example.invalid"]);
        let first_commit = commit(&origin, "first\n", "first");
        let url = url::Url::from_file_path(&origin).unwrap();
        let storage = directory.path().join("cache/upstreams");
        let upstream = Git {
            url,
            commit: "main".to_owned(),
            name: "locked-source".to_owned(),
            original_index: 0,
            materialization_sha256: None,
        };
        let mut first = runtime::block_on(upstream.resolve(&storage, &ProgressBar::new(100))).unwrap();
        assert_eq!(first.resolved_hash, first_commit);
        let first_digest =
            runtime::block_on(first.export_normalized(&directory.path().join("first-export"), 0)).unwrap();
        assert_eq!(first_digest.len(), 64);

        let repeated = runtime::block_on(upstream.resolve(&storage, &ProgressBar::new(100))).unwrap();
        assert_eq!(repeated.resolved_hash, first_commit);
        let repeated_digest =
            runtime::block_on(repeated.export_normalized(&directory.path().join("repeated-export"), 0)).unwrap();
        assert_eq!(repeated_digest, first_digest);

        first.materialization_sha256 = Some(first_digest.clone());
        let shared = directory.path().join("shared");
        fs::create_dir(&shared).unwrap();
        runtime::block_on(first.share(&shared.join("locked-source"), 1_700_000_000)).unwrap();
        assert_eq!(
            fs::read_to_string(shared.join("locked-source/source.txt")).unwrap(),
            "first\n"
        );

        first.materialization_sha256 = Some("c".repeat(64));
        let rejected = directory.path().join("rejected");
        fs::create_dir(&rejected).unwrap();
        let error = runtime::block_on(first.share(&rejected.join("locked-source"), 1_700_000_000)).unwrap_err();
        assert!(matches!(
            error,
            git::Error::MaterializationDigestMismatch {
                index: 0,
                commit,
                expected,
                found,
            } if commit == first_commit
                && expected == "c".repeat(64)
                && found == first_digest
        ));
        assert!(!rejected.join("locked-source").exists());

        let second_commit = commit(&origin, "second\n", "second");
        let second = runtime::block_on(upstream.resolve(&storage, &ProgressBar::new(100))).unwrap();
        assert_eq!(second.resolved_hash, second_commit);
        let second_digest =
            runtime::block_on(second.export_normalized(&directory.path().join("second-export"), 0)).unwrap();
        assert_ne!(second_digest, first_digest);
        assert_ne!(first_commit, second_commit);
    }

    #[test]
    fn local_git_lower_layer_rejects_implicit_submodules() {
        let directory = tempfile::tempdir().unwrap();
        let origin = directory.path().join("origin");
        fs::create_dir(&origin).unwrap();
        git(&origin, &["init", "--initial-branch=main"]);
        git(&origin, &["config", "user.name", "Cast Test"]);
        git(&origin, &["config", "user.email", "cast@example.invalid"]);
        let dependency_commit = commit(&origin, "source\n", "source");
        let cache_info = format!("160000,{dependency_commit},vendor/dependency");
        git(&origin, &["update-index", "--add", "--cacheinfo", &cache_info]);
        git(&origin, &["commit", "-m", "implicit submodule"]);
        let commit = git(&origin, &["rev-parse", "HEAD"]);

        let storage = directory.path().join("cache/upstreams");
        let upstream = Git {
            url: url::Url::from_file_path(&origin).unwrap(),
            commit: "main".to_owned(),
            name: "source".to_owned(),
            original_index: 0,
            materialization_sha256: None,
        };
        let error = match runtime::block_on(upstream.resolve(&storage, &ProgressBar::new(100))) {
            Ok(_) => panic!("Git source with an implicit submodule was accepted"),
            Err(error) => error,
        };

        assert!(matches!(
            error,
            git::Error::UnsupportedSubmodules { commit: rejected } if rejected == commit
        ));
    }
}
