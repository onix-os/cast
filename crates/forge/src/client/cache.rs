// SPDX-FileCopyrightText: 2023 AerynOS Developers
// SPDX-License-Identifier: MPL-2.0

//! Cache management for unpacking remote assets (`.stone`, etc.)

use std::{
    collections::HashSet,
    ffi::{CStr, CString, OsStr, OsString},
    io,
    mem::{size_of, zeroed},
    os::{
        fd::{AsRawFd as _, FromRawFd as _, OwnedFd, RawFd},
        unix::{
            ffi::OsStrExt as _,
            fs::{MetadataExt as _, OpenOptionsExt as _, PermissionsExt as _},
        },
    },
    path::Path,
    path::PathBuf,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use sha2::{Digest as _, Sha256};
use snafu::{OptionExt, ResultExt as _, Snafu, ensure};
use stone::{StoneDecodedPayload, StoneDigestWriter, StoneDigestWriterHasher, StoneReadError};
use tokio::io::{AsyncReadExt as _, AsyncSeekExt as _};
use tracing::warn;
use url::Url;

use crate::{Installation, package, request};

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
    /// Opportunistically suppress duplicate work within this process. A busy
    /// entry never makes a worker wait indefinitely: it may unpack into its
    /// own private stage and converge through bounded cross-process
    /// publication instead.
    pub fn acquire(&self, path: PathBuf) -> Option<InProgressGuard> {
        let mut assets = self.0.lock().unwrap_or_else(|error| error.into_inner());
        assets.insert(path.clone()).then(|| InProgressGuard {
            owner: self.clone(),
            path: Some(path),
        })
    }
}

/// Removes the asset from the in-progress set when the guard
/// goes out of scope.
impl Drop for InProgressGuard {
    fn drop(&mut self) {
        if let Some(path) = self.path.take() {
            let mut assets = self.owner.0.lock().unwrap_or_else(|error| error.into_inner());
            assets.remove(&path);
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
    let url = meta.uri.as_deref().context(MissingUrlSnafu)?;
    let url = url.parse::<Url>().context(InvalidUrlSnafu { url })?;
    let hash = meta.hash.as_ref().context(MissingHashSnafu)?;
    let limits = package_download_limits(meta.download_size);

    let destination_path = download_path(installation, hash)?;
    let destination_name = destination_path.file_name().context(MalformedHashSnafu { hash })?;
    let destination_parent = open_download_parent(installation, hash)?;

    match authenticate_sha256_entry_async(
        &destination_parent,
        destination_name,
        hash,
        meta.download_size,
        limits.max_bytes,
        Some(PRIVATE_FILE_MODE),
    )
    .await
    {
        Ok(file) => {
            return Ok(Download {
                path: destination_path,
                file,
                expected_sha256: hash.clone(),
                expected_size: meta.download_size,
                max_size: limits.max_bytes,
                installation: installation.clone(),
                was_cached: true,
            });
        }
        // Always force re-download on any error checking
        // cache validity
        Err(err) => {
            warn!(
                error = format!("{err:#}"),
                "Failed to verify cached download, will re-download"
            );
        }
    }

    // Request writes to a fresh, mode-0700 directory rather than the shared
    // final name. The directory descriptor remains pinned across the await;
    // after request-level SHA-256 verification we authenticate the resulting
    // entry through that descriptor and publish it with `RENAME_NOREPLACE`.
    let stage = PrivateStageDirectory::create(&destination_parent, ".download-stage-")?;
    let staged_name = OsStr::new("package.stone");
    let staged_path = stage.path.join(staged_name);
    if let Err(source) =
        request::download_with_progress_and_expected_sha256_and_limits(url, &staged_path, hash, limits, |progress| {
            (on_progress)(Progress {
                delta: progress.delta,
                completed: progress.completed,
                total: meta.download_size.unwrap_or(progress.completed),
            });
        })
        .await
    {
        // Request owns a tempfile inside this private directory. Returning is
        // not enough: prove its TempPath has been removed before propagating
        // any transport/hash failure.
        stage.require_inventory(&[])?;
        return match source {
            request::Error::HashMismatch { expected, actual } => Err(FetchError::BinaryStoneHashMismatch {
                package: meta.name.to_string(),
                expected,
                actual,
            }),
            source => Err(FetchError::Request { source }),
        };
    }
    stage.require_inventory(&[staged_name])?;

    let staged_file = authenticate_sha256_entry_async(
        &stage.directory,
        staged_name,
        hash,
        meta.download_size,
        limits.max_bytes,
        None,
    )
    .await?;
    staged_file.set_permissions(std::fs::Permissions::from_mode(PRIVATE_FILE_MODE))?;
    staged_file.sync_all()?;
    let staged_fingerprint = authenticate_sha256_file_async(
        &staged_file,
        hash,
        meta.download_size,
        limits.max_bytes,
        Some(PRIVATE_FILE_MODE),
    )
    .await?;
    let file = publish_download_entry_async(
        &stage.directory,
        staged_name,
        &destination_parent,
        destination_name,
        staged_file,
        staged_fingerprint,
        hash,
        meta.download_size,
        limits.max_bytes,
    )
    .await?;

    Ok(Download {
        path: destination_path,
        file,
        expected_sha256: hash.clone(),
        expected_size: meta.download_size,
        max_size: limits.max_bytes,
        installation: installation.clone(),
        was_cached: false,
    })
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
    path: PathBuf,
    /// Exact descriptor authenticated at the download/cache boundary. The
    /// pathname is diagnostic only after construction.
    file: std::fs::File,
    expected_sha256: String,
    expected_size: Option<u64>,
    max_size: u64,
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
        use std::io::{Seek as _, SeekFrom, Write as _};

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

            fn finish(self) -> io::Result<()> {
                if self.written != self.total {
                    return Err(io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        format!(
                            "content expansion produced {} bytes, expected exactly {}",
                            self.written, self.total
                        ),
                    ));
                }
                Ok(())
            }
        }

        impl<W: io::Write> io::Write for ProgressWriter<'_, W> {
            fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
                let remaining = self.total.saturating_sub(self.written);
                if buf.len() as u64 > remaining {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!("content expansion exceeds declared size {}", self.total),
                    ));
                }
                let bytes = self.writer.write(buf)?;
                self.written = self
                    .written
                    .checked_add(bytes as u64)
                    .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "content byte count overflow"))?;

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

        let Download {
            path: _,
            mut file,
            expected_sha256,
            expected_size,
            max_size,
            installation,
            was_cached: _,
        } = self;
        authenticate_sha256_file_sync(
            &mut file,
            &expected_sha256,
            expected_size,
            max_size,
            Some(PRIVATE_FILE_MODE),
        )?;
        let mut download_proof = file.try_clone()?;
        let mut reader = stone::read(file)?;

        let payloads = reader.payloads()?.collect::<Result<Vec<_>, _>>()?;
        let indices = payloads
            .iter()
            .filter_map(StoneDecodedPayload::index)
            .flat_map(|p| &p.body)
            .collect::<Vec<_>>();

        if indices.is_empty() {
            authenticate_sha256_file_sync(
                &mut download_proof,
                &expected_sha256,
                expected_size,
                max_size,
                Some(PRIVATE_FILE_MODE),
            )?;
            return Ok(UnpackedAsset { payloads });
        }

        let content = payloads
            .iter()
            .find_map(StoneDecodedPayload::content)
            .ok_or(UnpackError::MissingContent)?;

        let cache_root = Directory::open_absolute(&installation.cache_path(""))?;
        let content_dir = cache_root.open_or_create_directory(OsStr::new("content"), PRIVATE_DIRECTORY_MODE)?;
        let mut content_file = AnonymousFile::create(&content_dir, ".content-stage-")?;
        let mut progress = ProgressWriter::new(&mut content_file.file, content.header.plain_size, &on_progress);
        reader.unpack_content(content, &mut progress)?;
        progress.finish()?;
        content_file.file.flush()?;
        content_file.file.sync_all()?;
        let content_metadata = content_file.file.metadata()?;
        ensure!(
            content_metadata.file_type().is_file() && content_metadata.len() == content.header.plain_size,
            ContentSizeMismatchSnafu {
                expected: content.header.plain_size,
                actual: content_metadata.len()
            }
        );

        let assets_root = Directory::open_absolute(&installation.assets_path(""))?;
        let assets_v2 = assets_root.open_or_create_directory(OsStr::new("v2"), CACHE_DIRECTORY_MODE)?;

        for idx in indices {
            let digest_name = format!("{:02x}", idx.digest);
            let path = asset_path(&installation, &digest_name);
            let expected_len = idx.end.checked_sub(idx.start).ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("asset index range is inverted: {}..{}", idx.start, idx.end),
                )
            })?;
            let _guard = unpacking_in_progress.acquire(path.clone());
            let (asset_parent, asset_name) = open_asset_parent(&assets_v2, &digest_name)?;

            match authenticate_asset_entry(&asset_parent, asset_name.as_os_str(), idx.digest, expected_len) {
                Ok(_) => continue,
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => warn!(
                    path = %path.display(),
                    error = format!("{error:#}"),
                    "Cached asset failed authentication and will be replaced"
                ),
            }

            content_file.file.seek(SeekFrom::Start(idx.start))?;
            let mut stage = NamedStageFile::create(&asset_parent, ".asset-stage-")?;
            let actual = copy_asset_exact(&mut content_file.file, &mut stage.file, expected_len)?;
            ensure!(
                actual == idx.digest,
                FileUnpackHashMismatchSnafu {
                    path: path.clone(),
                    expected: idx.digest,
                    actual
                }
            );
            stage.file.flush()?;
            stage.file.sync_all()?;
            let staged_fingerprint = authenticate_asset_file(&mut stage.file, idx.digest, expected_len)?;
            let published = publish_asset_entry(
                &mut stage,
                &asset_parent,
                asset_name.as_os_str(),
                staged_fingerprint,
                idx.digest,
                expected_len,
            )?;
            drop(published);
        }

        authenticate_sha256_file_sync(
            &mut download_proof,
            &expected_sha256,
            expected_size,
            max_size,
            Some(PRIVATE_FILE_MODE),
        )?;

        Ok(UnpackedAsset { payloads })
    }
}

include!("cache/directory_control.rs");
include!("cache/entry_authentication.rs");
include!("cache/entry_publication.rs");

/// Returns a fully qualified filesystem path to download the given hash ID into
pub fn download_path(installation: &Installation, hash: &str) -> Result<PathBuf, FetchError> {
    validate_sha256(hash).map_err(|_| FetchError::MalformedHash { hash: hash.to_owned() })?;

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
    #[snafu(display("Unpacked content size mismatch: expected {expected} bytes, got {actual}"))]
    ContentSizeMismatch { expected: u64, actual: u64 },
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
mod download_limit_tests;
