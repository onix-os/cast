// SPDX-FileCopyrightText: 2023 AerynOS Developers
// SPDX-License-Identifier: MPL-2.0

//! Cache management for unpacking remote assets (`.stone`, etc.)

use std::collections::HashSet;
use std::path::Path;
use std::{
    io,
    path::PathBuf,
    sync::{Arc, Mutex},
};

use snafu::{OptionExt, ResultExt as _, Snafu, ensure};
use stone::{StoneDecodedPayload, StoneDigestWriter, StoneDigestWriterHasher, StoneReadError};
use tokio::io::AsyncReadExt as _;
use tracing::warn;
use url::Url;

use crate::{Installation, package, request, util};

/// Synchronized set of assets that are currently being
/// unpacked. Used to prevent unpacking the same asset
/// from different packages at the same time.
#[derive(Debug, Clone, Default)]
pub struct UnpackingInProgress(Arc<Mutex<HashSet<PathBuf>>>);

/// RAII guard representing exclusive ownership of an
/// in-progress asset unpack operation. When dropped
/// the asset is automatically removed from the
/// in-progress set.
pub struct InProgressGuard {
    owner: UnpackingInProgress,
    path: Option<PathBuf>,
}

impl UnpackingInProgress {
    /// Attempt to acquire exclusive unpack ownership for the asset.
    ///
    /// Returns `Some(InProgressGuard)` if the asset was successfully
    /// acquired, or `None` if another worker is currently unpacking it.
    pub fn acquire(&self, path: PathBuf) -> Option<InProgressGuard> {
        let mut lock = self.0.lock().unwrap_or_else(|e| e.into_inner());
        if lock.insert(path.clone()) {
            Some(InProgressGuard {
                owner: self.clone(),
                path: Some(path),
            })
        } else {
            None
        }
    }
}

/// Removes the asset from the in-progress set when the guard
/// goes out of scope.
impl Drop for InProgressGuard {
    fn drop(&mut self) {
        if let Some(path) = self.path.take() {
            let mut lock = self.owner.0.lock().unwrap_or_else(|e| e.into_inner());
            lock.remove(&path);
        }
    }
}

/// Per-package progress tracking for UI integration
#[derive(Debug, Clone, Copy)]
pub struct Progress {
    pub delta: u64,
    pub completed: u64,
    pub total: u64,
}

impl Progress {
    /// Return the completion as a percentage
    pub fn pct(&self) -> f32 {
        self.completed as f32 / self.total as f32
    }
}

/// Fetch a package with the provided [`package::Meta`] and [`Installation`] and return a [`Download`] on success.
pub async fn fetch(
    meta: &package::Meta,
    installation: &Installation,
    on_progress: impl Fn(Progress),
) -> Result<Download, FetchError> {
    use fs_err::tokio as fs;

    let url = meta.uri.as_deref().context(MissingUrlSnafu)?;
    let url = url.parse::<Url>().context(InvalidUrlSnafu { url })?;
    let hash = meta.hash.as_ref().context(MissingHashSnafu)?;
    let limits = package_download_limits(meta.download_size);

    let destination_path = download_path(installation, hash)?;

    if let Some(parent) = destination_path.parent() {
        fs::create_dir_all(parent).await?;
    }

    match cached_download_matches(&destination_path, hash, limits).await {
        Ok(true) => {
            return Ok(Download {
                id: meta.id().into(),
                path: destination_path,
                installation: installation.clone(),
                was_cached: true,
            });
        }
        Ok(false) => {}
        // Always force re-download on any error checking
        // cache validity
        Err(err) => {
            warn!(
                error = format!("{err:#}"),
                "Failed to verify cached download, will re-download"
            );
        }
    }

    if let Err(source) = request::download_with_progress_and_expected_sha256_and_limits(
        url,
        &destination_path,
        hash,
        limits,
        |progress| {
            (on_progress)(Progress {
                delta: progress.delta,
                completed: progress.completed,
                total: meta.download_size.unwrap_or(progress.completed),
            });
        },
    )
    .await
    {
        return match source {
            request::Error::HashMismatch { expected, actual } => Err(FetchError::BinaryStoneHashMismatch {
                package: meta.name.to_string(),
                expected,
                actual,
            }),
            source => Err(FetchError::Request { source }),
        };
    }

    Ok(Download {
        id: meta.id().into(),
        path: destination_path,
        installation: installation.clone(),
        was_cached: false,
    })
}

async fn cached_download_matches(
    path: &Path,
    expected_hash: &str,
    limits: request::DownloadLimits,
) -> io::Result<bool> {
    if !tokio::fs::try_exists(path).await? {
        return Ok(false);
    }

    // Never follow a cache-entry symlink or block on a FIFO/device. The
    // descriptor's type and size are checked after open before hashing.
    let file = tokio::fs::OpenOptions::new()
        .read(true)
        .custom_flags(nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW | nix::libc::O_NONBLOCK)
        .open(path)
        .await?;
    let metadata = file.metadata().await?;
    if !metadata.file_type().is_file() || metadata.len() > limits.max_bytes {
        return Ok(false);
    }

    let probe_limit = limits.max_bytes.saturating_add(1);
    let mut file = file.take(probe_limit);
    let actual_hash = util::sha256_hash_async(&mut file).await?;
    if probe_limit != u64::MAX && file.limit() == 0 {
        return Ok(false);
    }

    Ok(expected_hash == actual_hash)
}

pub(super) fn package_download_limits(declared_size: Option<u64>) -> request::DownloadLimits {
    request::DownloadLimits {
        // Repository metadata is an exact package-file size when present. It
        // may tighten, but never enlarge, the general Forge artifact ceiling.
        max_bytes: declared_size
            .unwrap_or(request::DEFAULT_DOWNLOAD_LIMITS.max_bytes)
            .min(request::DEFAULT_DOWNLOAD_LIMITS.max_bytes),
        total_timeout: request::DEFAULT_DOWNLOAD_LIMITS.total_timeout,
    }
}

/// A package that has been downloaded to the installation
pub struct Download {
    id: package::Id,
    path: PathBuf,
    installation: Installation,
    pub was_cached: bool,
}

/// Upon fetch completion we have this unpacked asset bound with
/// an open reader
pub struct UnpackedAsset {
    pub payloads: Vec<StoneDecodedPayload>,
}

impl Download {
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Unpack the downloaded package
    // TODO: Return an "Unpacked" struct which has a "blit" method on it?
    pub fn unpack(
        self,
        unpacking_in_progress: UnpackingInProgress,
        on_progress: impl Fn(Progress) + Send + 'static,
    ) -> Result<UnpackedAsset, UnpackError> {
        use fs_err::{self as fs, File};
        use std::io::{self, Read, Seek, SeekFrom, Write};

        struct ProgressWriter<'a, W> {
            writer: W,
            total: u64,
            written: u64,
            on_progress: &'a dyn Fn(Progress),
        }

        impl<'a, W> ProgressWriter<'a, W> {
            pub fn new(writer: W, total: u64, on_progress: &'a impl Fn(Progress)) -> Self {
                Self {
                    writer,
                    total,
                    written: 0,
                    on_progress,
                }
            }
        }

        impl<W: Write> Write for ProgressWriter<'_, W> {
            fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
                let bytes = self.writer.write(buf)?;

                self.written += bytes as u64;

                (self.on_progress)(Progress {
                    delta: bytes as u64,
                    completed: self.written,
                    total: self.total,
                });

                Ok(bytes)
            }

            fn flush(&mut self) -> io::Result<()> {
                self.writer.flush()
            }
        }

        let content_dir = self.installation.cache_path("content");
        let content_path = content_dir.join(self.id.as_str());

        fs::create_dir_all(&content_dir)?;

        let mut reader = stone::read(File::open(&self.path)?)?;

        let payloads = reader.payloads()?.collect::<Result<Vec<_>, _>>()?;
        let indices = payloads
            .iter()
            .filter_map(StoneDecodedPayload::index)
            .flat_map(|p| &p.body)
            .collect::<Vec<_>>();

        if indices.is_empty() {
            return Ok(UnpackedAsset { payloads });
        }

        let content = payloads
            .iter()
            .find_map(StoneDecodedPayload::content)
            .ok_or(UnpackError::MissingContent)?;

        let content_file = File::options()
            .read(true)
            .write(true)
            .create(true)
            .truncate(true)
            .open(&content_path)?;

        reader.unpack_content(
            content,
            &mut ProgressWriter::new(&content_file, content.header.plain_size, &on_progress),
        )?;

        indices
            .into_iter()
            .map(|idx| {
                let path = asset_path(&self.installation, &format!("{:02x}", idx.digest));
                let partial_path = path.with_added_extension("part");

                // Acquire in-progress guard.
                let _guard = match unpacking_in_progress.acquire(path.clone()) {
                    Some(guard) => guard,
                    None => return Ok(()),
                };

                let is_unpacked_already = || -> Result<bool, UnpackError> {
                    if fs::exists(&path)? {
                        let mut hasher = StoneDigestWriterHasher::new();
                        let mut file = File::open(&path)?;

                        io::copy(&mut file, &mut StoneDigestWriter::new(io::sink(), &mut hasher))?;

                        let actual_digest = hasher.digest128();

                        return Ok(idx.digest == actual_digest);
                    }

                    Ok(false)
                };

                match is_unpacked_already() {
                    Ok(true) => {
                        return Ok(());
                    }
                    Ok(false) => {}
                    // Always force unpack on any error checking cache validity
                    Err(err) => {
                        warn!(
                            error = format!("{err:#}"),
                            "Failed to verify if file is already unpacked, will re-unpack"
                        );
                    }
                }

                // Create parent dir
                if let Some(parent) = path.parent() {
                    fs::create_dir_all(parent)?;
                }

                // Split file reader over index range
                let mut file = &content_file;
                file.seek(SeekFrom::Start(idx.start))?;
                let mut split_file = (&mut file).take(idx.end - idx.start);

                let mut hasher = StoneDigestWriterHasher::new();
                let mut output = File::create(&partial_path)?;

                io::copy(&mut split_file, &mut StoneDigestWriter::new(&mut output, &mut hasher))?;

                let digest = hasher.digest128();

                ensure!(
                    digest == idx.digest,
                    FileUnpackHashMismatchSnafu {
                        path: path.clone(),
                        expected: idx.digest,
                        actual: digest
                    }
                );

                fs::rename(&partial_path, &path)?;

                Ok(())
            })
            .collect::<Result<Vec<_>, UnpackError>>()?;

        fs::remove_file(&content_path)?;

        Ok(UnpackedAsset { payloads })
    }
}

/// Returns a fully qualified filesystem path to download the given hash ID into
pub fn download_path(installation: &Installation, hash: &str) -> Result<PathBuf, FetchError> {
    ensure!(hash.len() >= 5, MalformedHashSnafu { hash });

    let directory = installation
        .cache_path("downloads")
        .join("v1")
        .join(&hash[..5])
        .join(&hash[hash.len() - 5..]);

    Ok(directory.join(hash))
}

/// Returns a fully qualified filesystem path to promote the final asset into
pub fn asset_path(installation: &Installation, hash: &str) -> PathBuf {
    let directory = if hash.len() >= 10 {
        installation
            .assets_path("v2")
            .join(&hash[..2])
            .join(&hash[2..4])
            .join(&hash[4..6])
    } else {
        installation.assets_path("v2")
    };

    directory.join(hash)
}

#[derive(Debug, Snafu)]
pub enum UnpackError {
    #[snafu(display("Missing content payload"))]
    MissingContent,
    #[snafu(context(false), display("read stone"))]
    ReadStone { source: StoneReadError },
    #[snafu(context(false), display("io"))]
    Io { source: io::Error },
    #[snafu(display("File unpack hash mismatch for {path:?}: expected {expected:02x}, got {actual:02x}"))]
    FileUnpackHashMismatch {
        path: PathBuf,
        expected: u128,
        actual: u128,
    },
}

#[derive(Debug, Snafu)]
pub enum FetchError {
    #[snafu(display("missing hash"))]
    MissingHash,
    #[snafu(display("malformed hash `{hash}`"))]
    MalformedHash { hash: String },
    #[snafu(display("missing URL"))]
    MissingUrl,
    #[snafu(display("invalid URL `{url}`"))]
    InvalidUrl { source: url::ParseError, url: Box<str> },
    #[snafu(transparent)]
    Request { source: request::Error },
    #[snafu(context(false), display("io"))]
    Io { source: io::Error },
    #[snafu(display("Binary stone hash mismatch for {package}: expected {expected}, got {actual}"))]
    BinaryStoneHashMismatch {
        package: String,
        expected: String,
        actual: String,
    },
}

#[cfg(test)]
mod download_limit_tests {
    use std::time::{Duration, Instant};

    use super::*;

    #[test]
    fn declared_package_size_can_only_tighten_the_global_ceiling() {
        assert_eq!(package_download_limits(Some(42)).max_bytes, 42);
        assert_eq!(package_download_limits(None), request::DEFAULT_DOWNLOAD_LIMITS);
        assert_eq!(
            package_download_limits(Some(request::DEFAULT_DOWNLOAD_LIMITS.max_bytes + 1)).max_bytes,
            request::DEFAULT_DOWNLOAD_LIMITS.max_bytes
        );
    }

    #[tokio::test]
    async fn cached_package_symlink_is_rejected_without_reading_target() {
        use std::os::unix::fs::symlink;

        let directory = tempfile::tempdir().unwrap();
        let outside = directory.path().join("outside");
        let cached = directory.path().join("cached");
        std::fs::write(&outside, b"outside bytes").unwrap();
        symlink(&outside, &cached).unwrap();

        assert!(
            cached_download_matches(&cached, "irrelevant", request::DEFAULT_DOWNLOAD_LIMITS)
                .await
                .is_err()
        );
        assert_eq!(std::fs::read(outside).unwrap(), b"outside bytes");
    }

    #[tokio::test]
    async fn cached_package_fifo_is_rejected_without_blocking() {
        use nix::{sys::stat::Mode, unistd::mkfifo};

        let directory = tempfile::tempdir().unwrap();
        let cached = directory.path().join("cached");
        mkfifo(&cached, Mode::S_IRUSR | Mode::S_IWUSR).unwrap();
        let started = Instant::now();

        assert!(
            !cached_download_matches(&cached, "irrelevant", request::DEFAULT_DOWNLOAD_LIMITS)
                .await
                .unwrap()
        );
        assert!(started.elapsed() < Duration::from_secs(2));
    }
}
