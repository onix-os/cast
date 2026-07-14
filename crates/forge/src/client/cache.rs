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

const PRIVATE_FILE_MODE: u32 = 0o600;
const PRIVATE_DIRECTORY_MODE: u32 = 0o700;
const CACHE_DIRECTORY_MODE: u32 = 0o755;
const MAX_PUBLICATION_ATTEMPTS: usize = 8;
const MAX_PRIVATE_STAGE_ENTRIES: usize = 64;
const MAX_PRIVATE_STAGE_NAME_BYTES: usize = 16 * 1024;
const PUBLICATION_LOCK_TIMEOUT: Duration = Duration::from_secs(30);
const PUBLICATION_LOCK_RETRY: Duration = Duration::from_millis(10);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FileFingerprint {
    device: u64,
    inode: u64,
    length: u64,
    mode: u32,
    uid: u32,
    gid: u32,
    links: u64,
    modified_seconds: i64,
    modified_nanoseconds: i64,
    changed_seconds: i64,
    changed_nanoseconds: i64,
}

impl FileFingerprint {
    fn from_metadata(metadata: &std::fs::Metadata) -> Self {
        Self {
            device: metadata.dev(),
            inode: metadata.ino(),
            length: metadata.len(),
            mode: metadata.mode(),
            uid: metadata.uid(),
            gid: metadata.gid(),
            links: metadata.nlink(),
            modified_seconds: metadata.mtime(),
            modified_nanoseconds: metadata.mtime_nsec(),
            changed_seconds: metadata.ctime(),
            changed_nanoseconds: metadata.ctime_nsec(),
        }
    }
}

#[derive(Debug)]
struct Directory {
    file: std::fs::File,
    path: PathBuf,
}

impl Directory {
    fn open_absolute(path: &Path) -> io::Result<Self> {
        if !path.is_absolute() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("cache directory must be absolute: {}", path.display()),
            ));
        }
        let relative = path
            .strip_prefix(Path::new("/"))
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "cache directory is not absolute"))?;
        let root = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(nix::libc::O_DIRECTORY | nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW)
            .open("/")?;
        let file = if relative.as_os_str().is_empty() {
            root.try_clone()?
        } else {
            openat2(
                root.as_raw_fd(),
                relative.as_os_str(),
                nix::libc::O_RDONLY | nix::libc::O_DIRECTORY | nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW,
                0,
                nix::libc::RESOLVE_BENEATH | nix::libc::RESOLVE_NO_MAGICLINKS | nix::libc::RESOLVE_NO_SYMLINKS,
            )?
        };
        // A pre-existing capability root is evidence, not something this
        // boundary may launder with chmod. Callers must provision it safely.
        validate_directory(&file, None)?;
        Ok(Self {
            file,
            path: path.to_owned(),
        })
    }

    fn open_or_create_directory(&self, name: &OsStr, mode: u32) -> io::Result<Self> {
        validate_component(name)?;
        let name_c = cstring(name)?;
        // SAFETY: the parent and NUL-terminated component remain live.
        let mkdir_result = unsafe { nix::libc::mkdirat(self.file.as_raw_fd(), name_c.as_ptr(), mode) };
        let created = if mkdir_result == -1 {
            let error = io::Error::last_os_error();
            if error.kind() != io::ErrorKind::AlreadyExists {
                return Err(error);
            }
            false
        } else {
            true
        };
        let file = openat2(
            self.file.as_raw_fd(),
            name,
            nix::libc::O_RDONLY | nix::libc::O_DIRECTORY | nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW,
            0,
            beneath_no_links(),
        )?;
        if created {
            // Only an inode created by this call may be normalized against a
            // hostile umask. Existing entries must already satisfy policy.
            file.set_permissions(std::fs::Permissions::from_mode(mode))?;
        }
        validate_directory(&file, Some(mode))?;
        Ok(Self {
            file,
            path: self.path.join(name),
        })
    }

    fn open_regular(&self, name: &OsStr) -> io::Result<std::fs::File> {
        validate_component(name)?;
        openat2(
            self.file.as_raw_fd(),
            name,
            nix::libc::O_RDONLY | nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW | nix::libc::O_NONBLOCK,
            0,
            beneath_no_links(),
        )
    }

    fn sync(&self) -> io::Result<()> {
        self.file.sync_all()
    }
}

fn validate_directory(file: &std::fs::File, exact_mode: Option<u32>) -> io::Result<()> {
    let metadata = file.metadata()?;
    if !metadata.file_type().is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "cache component is not a directory",
        ));
    }
    if metadata.uid() != nix::unistd::Uid::effective().as_raw() {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "cache directory is not owned by the effective user",
        ));
    }
    let mode = metadata.mode() & 0o7777;
    if exact_mode.is_some_and(|expected| mode != expected) {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "cache directory mode is {mode:04o}, expected exactly {:04o}",
                exact_mode.unwrap()
            ),
        ));
    }
    if mode & 0o022 != 0 {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!("cache directory mode {mode:04o} permits group/other writes"),
        ));
    }
    Ok(())
}

fn beneath_no_links() -> u64 {
    nix::libc::RESOLVE_BENEATH
        | nix::libc::RESOLVE_NO_MAGICLINKS
        | nix::libc::RESOLVE_NO_SYMLINKS
        | nix::libc::RESOLVE_NO_XDEV
}

fn openat2(dirfd: RawFd, path: &OsStr, flags: i32, mode: u32, resolve: u64) -> io::Result<std::fs::File> {
    let path = cstring(path)?;
    // SAFETY: zero is valid for every `open_how` field.
    let mut how: nix::libc::open_how = unsafe { zeroed() };
    how.flags = u64::from(flags as u32);
    how.mode = u64::from(mode);
    how.resolve = resolve;
    // SAFETY: `dirfd`, the C string, and `open_how` remain live. Success
    // returns one fresh descriptor owned below.
    let descriptor = unsafe {
        nix::libc::syscall(
            nix::libc::SYS_openat2,
            dirfd,
            path.as_ptr(),
            &how,
            size_of::<nix::libc::open_how>(),
        )
    };
    if descriptor == -1 {
        return Err(io::Error::last_os_error());
    }
    let descriptor = i32::try_from(descriptor)
        .map_err(|_| io::Error::other(format!("openat2 returned invalid descriptor {descriptor}")))?;
    // SAFETY: successful openat2 returned this fresh owned descriptor.
    let descriptor = unsafe { OwnedFd::from_raw_fd(descriptor) };
    Ok(std::fs::File::from(descriptor))
}

fn cstring(value: &OsStr) -> io::Result<CString> {
    CString::new(value.as_bytes()).map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "path contains NUL"))
}

fn validate_component(component: &OsStr) -> io::Result<()> {
    let bytes = component.as_bytes();
    if bytes.is_empty() || bytes == b"." || bytes == b".." || bytes.contains(&b'/') || bytes.contains(&0) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("invalid cache path component {component:?}"),
        ));
    }
    Ok(())
}

fn random_stage_name(prefix: &str) -> io::Result<String> {
    let mut random = [0_u8; 16];
    let mut filled = 0;
    while filled < random.len() {
        // SAFETY: the remaining slice is writable for the supplied length.
        let result = unsafe {
            nix::libc::syscall(
                nix::libc::SYS_getrandom,
                random[filled..].as_mut_ptr(),
                random.len() - filled,
                0,
            )
        };
        if result == -1 {
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(error);
        }
        let read = usize::try_from(result).map_err(|_| io::Error::other("getrandom returned an invalid length"))?;
        if read == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "getrandom returned no bytes",
            ));
        }
        filled += read;
    }
    let suffix = random.iter().map(|byte| format!("{byte:02x}")).collect::<String>();
    Ok(format!("{prefix}{suffix}"))
}

fn create_unique_file(parent: &Directory, prefix: &str) -> io::Result<(String, std::fs::File)> {
    for _ in 0..128 {
        let name = random_stage_name(prefix)?;
        match openat2(
            parent.file.as_raw_fd(),
            OsStr::new(&name),
            nix::libc::O_RDWR
                | nix::libc::O_CREAT
                | nix::libc::O_EXCL
                | nix::libc::O_CLOEXEC
                | nix::libc::O_NOFOLLOW
                | nix::libc::O_NONBLOCK,
            PRIVATE_FILE_MODE,
            beneath_no_links(),
        ) {
            Ok(file) => {
                if let Err(error) = file
                    .set_permissions(std::fs::Permissions::from_mode(PRIVATE_FILE_MODE))
                    .and_then(|()| {
                        validate_regular_metadata(&file.metadata()?, None, u64::MAX, Some(PRIVATE_FILE_MODE)).map(drop)
                    })
                {
                    let _ = unlink_entry(parent, OsStr::new(&name));
                    return Err(error);
                }
                return Ok((name, file));
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "could not allocate a unique cache stage",
    ))
}

struct AnonymousFile {
    file: std::fs::File,
}

impl AnonymousFile {
    fn create(parent: &Directory, prefix: &str) -> io::Result<Self> {
        let (name, file) = create_unique_file(parent, prefix)?;
        unlink_entry(parent, OsStr::new(&name))?;
        let metadata = file.metadata()?;
        if !metadata.file_type().is_file() || metadata.nlink() != 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "anonymous content stage did not detach from its pathname",
            ));
        }
        Ok(Self { file })
    }
}

struct NamedStageFile {
    parent: std::fs::File,
    name: String,
    file: std::fs::File,
    moved: bool,
}

impl NamedStageFile {
    fn create(parent: &Directory, prefix: &str) -> io::Result<Self> {
        let parent_file = parent.file.try_clone()?;
        let (name, file) = create_unique_file(parent, prefix)?;
        Ok(Self {
            parent: parent_file,
            name,
            file,
            moved: false,
        })
    }

    fn mark_moved(&mut self) {
        self.moved = true;
    }
}

impl Drop for NamedStageFile {
    fn drop(&mut self) {
        if !self.moved {
            let _ = unlinkat(self.parent.as_raw_fd(), OsStr::new(&self.name), 0);
        }
    }
}

/// Rolls back only the exact inode moved into a public name if any durability
/// or post-publication authentication step fails.
struct PublishedEntryGuard {
    parent: std::fs::File,
    name: OsString,
    device: u64,
    inode: u64,
    armed: bool,
}

impl PublishedEntryGuard {
    fn new(parent: &Directory, name: &OsStr, fingerprint: FileFingerprint) -> io::Result<Self> {
        Ok(Self {
            parent: parent.file.try_clone()?,
            name: name.to_owned(),
            device: fingerprint.device,
            inode: fingerprint.inode,
            armed: false,
        })
    }

    fn arm(&mut self) {
        self.armed = true;
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for PublishedEntryGuard {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        let parent = Directory {
            file: match self.parent.try_clone() {
                Ok(file) => file,
                Err(_) => return,
            },
            path: PathBuf::new(),
        };
        let Ok(file) = parent.open_regular(&self.name) else {
            return;
        };
        let Ok(metadata) = file.metadata() else {
            return;
        };
        if (metadata.dev(), metadata.ino()) == (self.device, self.inode) {
            let _ = unlinkat(self.parent.as_raw_fd(), &self.name, 0);
            let _ = self.parent.sync_all();
        }
    }
}

struct PrivateStageDirectory {
    parent: std::fs::File,
    name: String,
    path: PathBuf,
    directory: Directory,
}

impl PrivateStageDirectory {
    fn create(parent: &Directory, prefix: &str) -> io::Result<Self> {
        let parent_file = parent.file.try_clone()?;
        for _ in 0..128 {
            let name = random_stage_name(prefix)?;
            let name_c = cstring(OsStr::new(&name))?;
            // SAFETY: parent and component are valid and live.
            let result =
                unsafe { nix::libc::mkdirat(parent.file.as_raw_fd(), name_c.as_ptr(), PRIVATE_DIRECTORY_MODE) };
            if result == -1 {
                let error = io::Error::last_os_error();
                if error.kind() == io::ErrorKind::AlreadyExists {
                    continue;
                }
                return Err(error);
            }
            let directory = match parent.open_or_create_directory(OsStr::new(&name), PRIVATE_DIRECTORY_MODE) {
                Ok(directory) => directory,
                Err(error) => {
                    let _ = unlinkat(parent.file.as_raw_fd(), OsStr::new(&name), nix::libc::AT_REMOVEDIR);
                    return Err(error);
                }
            };
            return Ok(Self {
                parent: parent_file,
                name,
                path: directory.path.clone(),
                directory,
            });
        }
        Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "could not allocate a unique download stage directory",
        ))
    }

    fn require_inventory(&self, expected: &[&OsStr]) -> io::Result<()> {
        let mut actual = directory_entry_names(&self.directory)?;
        let mut expected = expected.iter().map(|name| (*name).to_owned()).collect::<Vec<_>>();
        actual.sort();
        expected.sort();
        if actual != expected {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("private download stage contains {actual:?}, expected exactly {expected:?}"),
            ));
        }
        Ok(())
    }

    fn cleanup_entries(&self) {
        if let Ok(entries) = directory_entry_names(&self.directory) {
            for entry in entries {
                let _ = unlinkat(self.directory.file.as_raw_fd(), &entry, 0);
            }
            let _ = self.directory.sync();
        }
    }
}

impl Drop for PrivateStageDirectory {
    fn drop(&mut self) {
        self.cleanup_entries();
        let _ = unlinkat(self.parent.as_raw_fd(), OsStr::new(&self.name), nix::libc::AT_REMOVEDIR);
    }
}

fn directory_entry_names(directory: &Directory) -> io::Result<Vec<OsString>> {
    // fdopendir owns its descriptor, so duplicate the retained capability.
    // SAFETY: fcntl receives a live descriptor and returns a fresh one.
    let duplicate = unsafe { nix::libc::fcntl(directory.file.as_raw_fd(), nix::libc::F_DUPFD_CLOEXEC, 0) };
    if duplicate == -1 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: successful fcntl returned one fresh descriptor.
    let duplicate = unsafe { OwnedFd::from_raw_fd(duplicate) };
    // SAFETY: fdopendir consumes the raw descriptor on success.
    let stream = unsafe { nix::libc::fdopendir(duplicate.as_raw_fd()) };
    if stream.is_null() {
        return Err(io::Error::last_os_error());
    }
    std::mem::forget(duplicate);

    struct Stream(*mut nix::libc::DIR);
    impl Drop for Stream {
        fn drop(&mut self) {
            // SAFETY: this stream is uniquely owned and still open.
            let _ = unsafe { nix::libc::closedir(self.0) };
        }
    }
    let stream = Stream(stream);
    let mut entries = Vec::new();
    let mut total_name_bytes = 0_usize;
    loop {
        // SAFETY: Linux exposes thread-local errno through this pointer.
        unsafe { *nix::libc::__errno_location() = 0 };
        // SAFETY: the directory stream remains live and exclusively consumed.
        let entry = unsafe { nix::libc::readdir(stream.0) };
        if entry.is_null() {
            // SAFETY: read immediately after readdir on this thread.
            let errno = unsafe { *nix::libc::__errno_location() };
            if errno != 0 {
                return Err(io::Error::from_raw_os_error(errno));
            }
            break;
        }
        // SAFETY: d_name is NUL-terminated for a successful readdir result and
        // remains valid until the next call.
        let name = unsafe { CStr::from_ptr((*entry).d_name.as_ptr()) };
        if name.to_bytes() == b"." || name.to_bytes() == b".." {
            continue;
        }
        if entries.len() == MAX_PRIVATE_STAGE_ENTRIES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("private stage exceeds {MAX_PRIVATE_STAGE_ENTRIES} entries"),
            ));
        }
        total_name_bytes = total_name_bytes
            .checked_add(name.to_bytes().len())
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "private stage name budget overflow"))?;
        if total_name_bytes > MAX_PRIVATE_STAGE_NAME_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("private stage names exceed {MAX_PRIVATE_STAGE_NAME_BYTES} bytes"),
            ));
        }
        entries.push(OsStr::from_bytes(name.to_bytes()).to_owned());
    }
    Ok(entries)
}

struct DirectoryLock<'a> {
    directory: &'a std::fs::File,
}

impl<'a> DirectoryLock<'a> {
    fn try_exclusive(directory: &'a std::fs::File) -> io::Result<Option<Self>> {
        loop {
            // SAFETY: flock only borrows the live descriptor.
            if unsafe { nix::libc::flock(directory.as_raw_fd(), nix::libc::LOCK_EX | nix::libc::LOCK_NB) } == 0 {
                return Ok(Some(Self { directory }));
            }
            let error = io::Error::last_os_error();
            match error.kind() {
                io::ErrorKind::Interrupted => continue,
                io::ErrorKind::WouldBlock => return Ok(None),
                _ => return Err(error),
            }
        }
    }

    fn exclusive_until(directory: &'a std::fs::File, deadline: Instant) -> io::Result<Self> {
        loop {
            if let Some(lock) = Self::try_exclusive(directory)? {
                return Ok(lock);
            }
            let now = Instant::now();
            if now >= deadline {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    format!("cache publication lock exceeded {PUBLICATION_LOCK_TIMEOUT:?}"),
                ));
            }
            std::thread::sleep(PUBLICATION_LOCK_RETRY.min(deadline.saturating_duration_since(now)));
        }
    }
}

async fn lock_directory_async(directory: &std::fs::File) -> io::Result<DirectoryLock<'_>> {
    let deadline = Instant::now() + PUBLICATION_LOCK_TIMEOUT;
    loop {
        if let Some(lock) = DirectoryLock::try_exclusive(directory)? {
            return Ok(lock);
        }
        let now = Instant::now();
        if now >= deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                format!("cache publication lock exceeded {PUBLICATION_LOCK_TIMEOUT:?}"),
            ));
        }
        tokio::time::sleep(PUBLICATION_LOCK_RETRY.min(deadline.saturating_duration_since(now))).await;
    }
}

impl Drop for DirectoryLock<'_> {
    fn drop(&mut self) {
        // SAFETY: the descriptor remains live for this guard's lifetime.
        let _ = unsafe { nix::libc::flock(self.directory.as_raw_fd(), nix::libc::LOCK_UN) };
    }
}

fn validate_regular_metadata(
    metadata: &std::fs::Metadata,
    exact_size: Option<u64>,
    max_size: u64,
    exact_mode: Option<u32>,
) -> io::Result<FileFingerprint> {
    if !metadata.file_type().is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "cache entry is not a regular file",
        ));
    }
    if metadata.uid() != nix::unistd::Uid::effective().as_raw() {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "cache entry is not owned by the effective user",
        ));
    }
    if metadata.nlink() != 1 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("cache entry has {} links, expected exactly one", metadata.nlink()),
        ));
    }
    if metadata.len() > max_size {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("cache entry is {} bytes, exceeding limit {max_size}", metadata.len()),
        ));
    }
    if exact_size.is_some_and(|expected| metadata.len() != expected) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "cache entry is {} bytes, expected exactly {}",
                metadata.len(),
                exact_size.unwrap()
            ),
        ));
    }
    let mode = metadata.mode() & 0o7777;
    if exact_mode.is_some_and(|expected| mode != expected) {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "cache entry mode is {mode:04o}, expected exactly {:04o}",
                exact_mode.unwrap()
            ),
        ));
    }
    Ok(FileFingerprint::from_metadata(metadata))
}

async fn authenticate_sha256_entry_async(
    directory: &Directory,
    name: &OsStr,
    expected_hash: &str,
    exact_size: Option<u64>,
    max_size: u64,
    exact_mode: Option<u32>,
) -> io::Result<std::fs::File> {
    let file = directory.open_regular(name)?;
    let fingerprint = authenticate_sha256_file_async(&file, expected_hash, exact_size, max_size, exact_mode).await?;
    let reopened = directory.open_regular(name)?;
    let reopened_fingerprint = validate_regular_metadata(&reopened.metadata()?, exact_size, max_size, exact_mode)?;
    if reopened_fingerprint != fingerprint {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "cache entry identity changed during authentication",
        ));
    }
    Ok(file)
}

async fn authenticate_sha256_file_async(
    file: &std::fs::File,
    expected_hash: &str,
    exact_size: Option<u64>,
    max_size: u64,
    exact_mode: Option<u32>,
) -> io::Result<FileFingerprint> {
    let before = validate_regular_metadata(&file.metadata()?, exact_size, max_size, exact_mode)?;
    let mut reader = tokio::fs::File::from_std(file.try_clone()?);
    reader.seek(io::SeekFrom::Start(0)).await?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    let mut read_total = 0_u64;
    loop {
        let remaining = max_size.saturating_sub(read_total);
        let capacity = remaining.saturating_add(1).min(buffer.len() as u64) as usize;
        let read = reader.read(&mut buffer[..capacity]).await?;
        if read == 0 {
            break;
        }
        if read as u64 > remaining {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("cache entry stream exceeds limit {max_size}"),
            ));
        }
        hasher.update(&buffer[..read]);
        read_total += read as u64;
    }
    if exact_size.is_some_and(|expected| read_total != expected) {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            format!(
                "cache entry stream is {read_total} bytes, expected exactly {}",
                exact_size.unwrap()
            ),
        ));
    }
    let actual_hash = hex::encode(hasher.finalize());
    if actual_hash != expected_hash {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("cache entry SHA-256 mismatch: expected {expected_hash}, got {actual_hash}"),
        ));
    }
    let after = validate_regular_metadata(&file.metadata()?, exact_size, max_size, exact_mode)?;
    if after != before {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "cache entry changed while its SHA-256 was computed",
        ));
    }
    Ok(after)
}

fn authenticate_sha256_file_sync(
    file: &mut std::fs::File,
    expected_hash: &str,
    exact_size: Option<u64>,
    max_size: u64,
    exact_mode: Option<u32>,
) -> io::Result<FileFingerprint> {
    use std::io::{Read as _, Seek as _};

    let before = validate_regular_metadata(&file.metadata()?, exact_size, max_size, exact_mode)?;
    file.seek(io::SeekFrom::Start(0))?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    let mut read_total = 0_u64;
    loop {
        let remaining = max_size.saturating_sub(read_total);
        let capacity = remaining.saturating_add(1).min(buffer.len() as u64) as usize;
        let read = file.read(&mut buffer[..capacity])?;
        if read == 0 {
            break;
        }
        if read as u64 > remaining {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("cache entry stream exceeds limit {max_size}"),
            ));
        }
        hasher.update(&buffer[..read]);
        read_total += read as u64;
    }
    if exact_size.is_some_and(|expected| read_total != expected) {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            format!(
                "cache entry stream is {read_total} bytes, expected exactly {}",
                exact_size.unwrap()
            ),
        ));
    }
    let actual_hash = hex::encode(hasher.finalize());
    if actual_hash != expected_hash {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("cache entry SHA-256 mismatch: expected {expected_hash}, got {actual_hash}"),
        ));
    }
    let after = validate_regular_metadata(&file.metadata()?, exact_size, max_size, exact_mode)?;
    if after != before {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "cache entry changed while its SHA-256 was computed",
        ));
    }
    file.seek(io::SeekFrom::Start(0))?;
    Ok(after)
}

fn authenticate_asset_entry(
    directory: &Directory,
    name: &OsStr,
    expected_digest: u128,
    expected_size: u64,
) -> io::Result<std::fs::File> {
    let mut file = directory.open_regular(name)?;
    let fingerprint = authenticate_asset_file(&mut file, expected_digest, expected_size)?;
    let reopened = directory.open_regular(name)?;
    let reopened_fingerprint = validate_regular_metadata(
        &reopened.metadata()?,
        Some(expected_size),
        expected_size,
        Some(PRIVATE_FILE_MODE),
    )?;
    if reopened_fingerprint != fingerprint {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "asset identity changed during authentication",
        ));
    }
    Ok(file)
}

fn authenticate_asset_file(
    file: &mut std::fs::File,
    expected_digest: u128,
    expected_size: u64,
) -> io::Result<FileFingerprint> {
    use std::io::{Read as _, Seek as _};

    let before = validate_regular_metadata(
        &file.metadata()?,
        Some(expected_size),
        expected_size,
        Some(PRIVATE_FILE_MODE),
    )?;
    file.seek(io::SeekFrom::Start(0))?;
    let mut hasher = StoneDigestWriterHasher::new();
    let mut digest_writer = StoneDigestWriter::new(io::sink(), &mut hasher);
    let copied = io::copy(&mut file.take(expected_size.saturating_add(1)), &mut digest_writer)?;
    if copied != expected_size {
        return Err(io::Error::new(
            if copied < expected_size {
                io::ErrorKind::UnexpectedEof
            } else {
                io::ErrorKind::InvalidData
            },
            format!("asset stream is {copied} bytes, expected exactly {expected_size}"),
        ));
    }
    let actual = hasher.digest128();
    if actual != expected_digest {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("asset digest mismatch: expected {expected_digest:02x}, got {actual:02x}"),
        ));
    }
    let after = validate_regular_metadata(
        &file.metadata()?,
        Some(expected_size),
        expected_size,
        Some(PRIVATE_FILE_MODE),
    )?;
    if after != before {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "asset changed while its digest was computed",
        ));
    }
    file.seek(io::SeekFrom::Start(0))?;
    Ok(after)
}

fn open_download_parent(installation: &Installation, hash: &str) -> io::Result<Directory> {
    validate_sha256(hash)?;
    let root = Directory::open_absolute(&installation.cache_path(""))?;
    let downloads = root.open_or_create_directory(OsStr::new("downloads"), CACHE_DIRECTORY_MODE)?;
    let version = downloads.open_or_create_directory(OsStr::new("v1"), CACHE_DIRECTORY_MODE)?;
    let prefix = version.open_or_create_directory(OsStr::new(&hash[..5]), CACHE_DIRECTORY_MODE)?;
    prefix.open_or_create_directory(OsStr::new(&hash[hash.len() - 5..]), CACHE_DIRECTORY_MODE)
}

fn open_asset_parent(root: &Directory, hash: &str) -> io::Result<(Directory, OsString)> {
    if hash.is_empty()
        || !hash
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "asset digest must be lowercase ASCII hexadecimal",
        ));
    }
    let mut parent = Directory {
        file: root.file.try_clone()?,
        path: root.path.clone(),
    };
    if hash.len() >= 10 {
        for component in [&hash[..2], &hash[2..4], &hash[4..6]] {
            parent = parent.open_or_create_directory(OsStr::new(component), CACHE_DIRECTORY_MODE)?;
        }
    }
    Ok((parent, OsString::from(hash)))
}

fn validate_sha256(hash: &str) -> io::Result<()> {
    if hash.len() != 64
        || !hash
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "package hash must be exactly 64 lowercase ASCII hexadecimal characters",
        ));
    }
    Ok(())
}

fn copy_asset_exact(input: &mut std::fs::File, output: &mut std::fs::File, expected: u64) -> io::Result<u128> {
    use std::io::Read as _;

    struct ExactWriter<'a> {
        inner: &'a mut std::fs::File,
        expected: u64,
        written: u64,
    }

    impl io::Write for ExactWriter<'_> {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            let remaining = self.expected.saturating_sub(self.written);
            if bytes.len() as u64 > remaining {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("asset output exceeds exact bound {}", self.expected),
                ));
            }
            let written = self.inner.write(bytes)?;
            self.written += written as u64;
            Ok(written)
        }

        fn flush(&mut self) -> io::Result<()> {
            self.inner.flush()
        }
    }

    let mut exact = ExactWriter {
        inner: output,
        expected,
        written: 0,
    };
    let mut hasher = StoneDigestWriterHasher::new();
    let copied = io::copy(
        &mut input.take(expected),
        &mut StoneDigestWriter::new(&mut exact, &mut hasher),
    )?;
    if copied != expected || exact.written != expected {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            format!("asset source supplied {copied} bytes, expected exactly {expected}"),
        ));
    }
    Ok(hasher.digest128())
}

async fn publish_download_entry_async(
    staged_directory: &Directory,
    staged_name: &OsStr,
    destination_directory: &Directory,
    destination_name: &OsStr,
    staged_file: std::fs::File,
    staged_fingerprint: FileFingerprint,
    expected_hash: &str,
    exact_size: Option<u64>,
    max_size: u64,
) -> io::Result<std::fs::File> {
    let _lock = lock_directory_async(&destination_directory.file).await?;
    for _ in 0..MAX_PUBLICATION_ATTEMPTS {
        match authenticate_sha256_entry_async(
            destination_directory,
            destination_name,
            expected_hash,
            exact_size,
            max_size,
            Some(PRIVATE_FILE_MODE),
        )
        .await
        {
            Ok(winner) => return Ok(winner),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => {
                warn!(
                    error = format!("{error:#}"),
                    "Removing invalid cached package before publication"
                );
                unlink_entry(destination_directory, destination_name)?;
            }
        }

        let current_staged = authenticate_sha256_entry_async(
            staged_directory,
            staged_name,
            expected_hash,
            exact_size,
            max_size,
            Some(PRIVATE_FILE_MODE),
        )
        .await?;
        let current_fingerprint = validate_regular_metadata(
            &current_staged.metadata()?,
            exact_size,
            max_size,
            Some(PRIVATE_FILE_MODE),
        )?;
        if current_fingerprint != staged_fingerprint {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "download stage identity changed before publication",
            ));
        }

        let mut rollback = PublishedEntryGuard::new(destination_directory, destination_name, staged_fingerprint)?;
        match rename_noreplace(staged_directory, staged_name, destination_directory, destination_name) {
            Ok(()) => {
                rollback.arm();
                destination_directory.sync()?;
                let final_file = authenticate_sha256_entry_async(
                    destination_directory,
                    destination_name,
                    expected_hash,
                    exact_size,
                    max_size,
                    Some(PRIVATE_FILE_MODE),
                )
                .await?;
                let final_fingerprint =
                    validate_regular_metadata(&final_file.metadata()?, exact_size, max_size, Some(PRIVATE_FILE_MODE))?;
                let retained_fingerprint = FileFingerprint::from_metadata(&staged_file.metadata()?);
                if final_fingerprint != retained_fingerprint
                    || (retained_fingerprint.device, retained_fingerprint.inode)
                        != (staged_fingerprint.device, staged_fingerprint.inode)
                {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "published package is not the authenticated staged inode",
                    ));
                }
                rollback.disarm();
                return Ok(staged_file);
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::WouldBlock,
        "package cache publication did not converge",
    ))
}

fn publish_asset_entry(
    stage: &mut NamedStageFile,
    destination_directory: &Directory,
    destination_name: &OsStr,
    staged_fingerprint: FileFingerprint,
    expected_digest: u128,
    expected_size: u64,
) -> io::Result<std::fs::File> {
    let _lock = DirectoryLock::exclusive_until(&destination_directory.file, Instant::now() + PUBLICATION_LOCK_TIMEOUT)?;
    for _ in 0..MAX_PUBLICATION_ATTEMPTS {
        match authenticate_asset_entry(destination_directory, destination_name, expected_digest, expected_size) {
            Ok(winner) => return Ok(winner),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => {
                warn!(
                    error = format!("{error:#}"),
                    "Removing invalid asset before publication"
                );
                unlink_entry(destination_directory, destination_name)?;
            }
        }

        let current_stage = authenticate_asset_entry(
            &Directory {
                file: stage.parent.try_clone()?,
                path: destination_directory.path.clone(),
            },
            OsStr::new(&stage.name),
            expected_digest,
            expected_size,
        )?;
        if FileFingerprint::from_metadata(&current_stage.metadata()?) != staged_fingerprint {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "asset stage identity changed before publication",
            ));
        }

        let mut rollback = PublishedEntryGuard::new(destination_directory, destination_name, staged_fingerprint)?;
        match rename_noreplace_raw(
            stage.parent.as_raw_fd(),
            OsStr::new(&stage.name),
            destination_directory.file.as_raw_fd(),
            destination_name,
        ) {
            Ok(()) => {
                rollback.arm();
                stage.mark_moved();
                destination_directory.sync()?;
                let final_file =
                    authenticate_asset_entry(destination_directory, destination_name, expected_digest, expected_size)?;
                let final_fingerprint = FileFingerprint::from_metadata(&final_file.metadata()?);
                let retained_fingerprint = FileFingerprint::from_metadata(&stage.file.metadata()?);
                if final_fingerprint != retained_fingerprint
                    || (retained_fingerprint.device, retained_fingerprint.inode)
                        != (staged_fingerprint.device, staged_fingerprint.inode)
                {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "published asset is not the authenticated staged inode",
                    ));
                }
                rollback.disarm();
                return Ok(final_file);
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::WouldBlock,
        "asset publication did not converge",
    ))
}

fn rename_noreplace(
    source_directory: &Directory,
    source_name: &OsStr,
    destination_directory: &Directory,
    destination_name: &OsStr,
) -> io::Result<()> {
    rename_noreplace_raw(
        source_directory.file.as_raw_fd(),
        source_name,
        destination_directory.file.as_raw_fd(),
        destination_name,
    )
}

fn rename_noreplace_raw(
    source_directory: RawFd,
    source_name: &OsStr,
    destination_directory: RawFd,
    destination_name: &OsStr,
) -> io::Result<()> {
    let source_name = cstring(source_name)?;
    let destination_name = cstring(destination_name)?;
    // SAFETY: both descriptors and C strings remain live for the syscall.
    let result = unsafe {
        nix::libc::syscall(
            nix::libc::SYS_renameat2,
            source_directory,
            source_name.as_ptr(),
            destination_directory,
            destination_name.as_ptr(),
            nix::libc::RENAME_NOREPLACE,
        )
    };
    if result == -1 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

fn unlink_entry(directory: &Directory, name: &OsStr) -> io::Result<()> {
    unlinkat(directory.file.as_raw_fd(), name, 0)
}

fn unlinkat(directory: RawFd, name: &OsStr, flags: i32) -> io::Result<()> {
    validate_component(name)?;
    let name = cstring(name)?;
    // SAFETY: descriptor and component remain live for the syscall.
    let result = unsafe { nix::libc::unlinkat(directory, name.as_ptr(), flags) };
    if result == -1 {
        let error = io::Error::last_os_error();
        if error.kind() == io::ErrorKind::NotFound {
            Ok(())
        } else {
            Err(error)
        }
    } else {
        Ok(())
    }
}

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
mod download_limit_tests {
    use std::{
        io::{Cursor, Write as _},
        os::unix::fs::{PermissionsExt as _, symlink},
        sync::{Arc, Barrier},
        time::{Duration, Instant},
    };

    use stone::{StoneHeaderV1FileType, StoneWriter};

    use super::*;

    fn digest(bytes: &[u8]) -> u128 {
        let mut hasher = StoneDigestWriterHasher::new();
        {
            let mut writer = StoneDigestWriter::new(io::sink(), &mut hasher);
            writer.write_all(bytes).unwrap();
        }
        hasher.digest128()
    }

    fn private_tempdir() -> tempfile::TempDir {
        let temporary = tempfile::tempdir().unwrap();
        std::fs::set_permissions(
            temporary.path(),
            std::fs::Permissions::from_mode(PRIVATE_DIRECTORY_MODE),
        )
        .unwrap();
        temporary
    }

    fn sha256(bytes: &[u8]) -> String {
        hex::encode(Sha256::digest(bytes))
    }

    fn stage_asset(directory: &Directory, bytes: &[u8]) -> (NamedStageFile, FileFingerprint, u128) {
        let expected = digest(bytes);
        let mut stage = NamedStageFile::create(directory, ".asset-stage-").unwrap();
        stage.file.write_all(bytes).unwrap();
        stage.file.flush().unwrap();
        stage.file.sync_all().unwrap();
        let fingerprint = authenticate_asset_file(&mut stage.file, expected, bytes.len() as u64).unwrap();
        (stage, fingerprint, expected)
    }

    fn stone_with_content(bytes: &[u8]) -> Vec<u8> {
        let mut archive = Vec::new();
        let mut writer = StoneWriter::new(&mut archive, StoneHeaderV1FileType::Binary)
            .unwrap()
            .with_content(Cursor::new(Vec::new()), Some(bytes.len() as u64), 1)
            .unwrap();
        writer.add_content(&mut Cursor::new(bytes)).unwrap();
        writer.finalize().unwrap();
        archive
    }

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
        let directory = private_tempdir();
        let outside = directory.path().join("outside");
        let cached = directory.path().join("cached");
        std::fs::write(&outside, b"outside bytes").unwrap();
        symlink(&outside, &cached).unwrap();
        let directory = Directory::open_absolute(directory.path()).unwrap();

        assert!(
            authenticate_sha256_entry_async(
                &directory,
                OsStr::new("cached"),
                "irrelevant",
                None,
                request::DEFAULT_DOWNLOAD_LIMITS.max_bytes,
                None,
            )
            .await
            .is_err()
        );
        assert_eq!(std::fs::read(outside).unwrap(), b"outside bytes");
    }

    #[tokio::test]
    async fn cached_package_fifo_is_rejected_without_blocking() {
        use nix::{sys::stat::Mode, unistd::mkfifo};

        let directory = private_tempdir();
        let cached = directory.path().join("cached");
        mkfifo(&cached, Mode::S_IRUSR | Mode::S_IWUSR).unwrap();
        let started = Instant::now();
        let directory = Directory::open_absolute(directory.path()).unwrap();

        assert!(
            authenticate_sha256_entry_async(
                &directory,
                OsStr::new("cached"),
                "irrelevant",
                None,
                request::DEFAULT_DOWNLOAD_LIMITS.max_bytes,
                None,
            )
            .await
            .is_err()
        );
        assert!(started.elapsed() < Duration::from_secs(2));
    }

    #[tokio::test]
    async fn cached_package_requires_the_exact_declared_size_at_n_and_n_plus_one() {
        let temporary = private_tempdir();
        for (name, bytes) in [("exact", b"abc".as_slice()), ("short", b"ab"), ("long", b"abcd")] {
            let path = temporary.path().join(name);
            std::fs::write(&path, bytes).unwrap();
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(PRIVATE_FILE_MODE)).unwrap();
        }
        let directory = Directory::open_absolute(temporary.path()).unwrap();
        let expected_hash = sha256(b"abc");

        authenticate_sha256_entry_async(
            &directory,
            OsStr::new("exact"),
            &expected_hash,
            Some(3),
            3,
            Some(PRIVATE_FILE_MODE),
        )
        .await
        .unwrap();
        for name in ["short", "long"] {
            assert!(
                authenticate_sha256_entry_async(
                    &directory,
                    OsStr::new(name),
                    &expected_hash,
                    Some(3),
                    3,
                    Some(PRIVATE_FILE_MODE),
                )
                .await
                .is_err()
            );
        }
    }

    #[test]
    fn retained_download_descriptor_defeats_path_substitution_before_unpack() {
        let temporary = private_tempdir();
        let installation_root = temporary.path().join("root");
        std::fs::create_dir(&installation_root).unwrap();
        crate::test_support::prepare_private_installation_root(&installation_root);
        let installation = Installation::open(&installation_root, None).unwrap();
        std::fs::set_permissions(
            installation.cache_path(""),
            std::fs::Permissions::from_mode(PRIVATE_DIRECTORY_MODE),
        )
        .unwrap();
        std::fs::set_permissions(
            installation.assets_path(""),
            std::fs::Permissions::from_mode(PRIVATE_DIRECTORY_MODE),
        )
        .unwrap();
        let original_bytes = b"descriptor-authenticated package content";
        let archive = stone_with_content(original_bytes);
        let expected_sha256 = sha256(&archive);
        let download_path = installation.cache_path("retained-download.stone");
        std::fs::write(&download_path, &archive).unwrap();
        std::fs::set_permissions(&download_path, std::fs::Permissions::from_mode(PRIVATE_FILE_MODE)).unwrap();
        let mut descriptor = std::fs::OpenOptions::new().read(true).open(&download_path).unwrap();
        authenticate_sha256_file_sync(
            &mut descriptor,
            &expected_sha256,
            Some(archive.len() as u64),
            archive.len() as u64,
            Some(PRIVATE_FILE_MODE),
        )
        .unwrap();

        std::fs::rename(&download_path, installation.cache_path("detached-download.stone")).unwrap();
        std::fs::write(&download_path, b"hostile pathname replacement").unwrap();
        std::fs::set_permissions(&download_path, std::fs::Permissions::from_mode(PRIVATE_FILE_MODE)).unwrap();

        Download {
            path: download_path.clone(),
            file: descriptor,
            expected_sha256,
            expected_size: Some(archive.len() as u64),
            max_size: archive.len() as u64,
            installation: installation.clone(),
            was_cached: true,
        }
        .unpack(UnpackingInProgress::default(), |_| {})
        .unwrap();

        let asset = asset_path(&installation, &format!("{:02x}", digest(original_bytes)));
        assert_eq!(std::fs::read(asset).unwrap(), original_bytes);
        assert_eq!(std::fs::read(download_path).unwrap(), b"hostile pathname replacement");
    }

    #[test]
    fn asset_publication_replaces_fifo_and_symlink_without_blocking_or_touching_target() {
        use nix::{sys::stat::Mode, unistd::mkfifo};

        let temporary = private_tempdir();
        let directory = Directory::open_absolute(temporary.path()).unwrap();
        let bytes = b"authenticated asset";
        let name = format!("{:02x}", digest(bytes));
        let final_path = temporary.path().join(&name);
        mkfifo(&final_path, Mode::S_IRUSR | Mode::S_IWUSR).unwrap();
        let (mut stage, fingerprint, expected) = stage_asset(&directory, bytes);
        let started = Instant::now();
        publish_asset_entry(
            &mut stage,
            &directory,
            OsStr::new(&name),
            fingerprint,
            expected,
            bytes.len() as u64,
        )
        .unwrap();
        assert!(started.elapsed() < Duration::from_secs(2));
        assert_eq!(std::fs::read(&final_path).unwrap(), bytes);

        std::fs::remove_file(&final_path).unwrap();
        let outside = temporary.path().join("outside");
        std::fs::write(&outside, b"outside stays unchanged").unwrap();
        symlink(&outside, &final_path).unwrap();
        let (mut stage, fingerprint, expected) = stage_asset(&directory, bytes);
        publish_asset_entry(
            &mut stage,
            &directory,
            OsStr::new(&name),
            fingerprint,
            expected,
            bytes.len() as u64,
        )
        .unwrap();
        assert_eq!(std::fs::read(&final_path).unwrap(), bytes);
        assert_eq!(std::fs::read(outside).unwrap(), b"outside stays unchanged");
    }

    #[test]
    fn asset_authentication_rejects_truncated_and_n_plus_one_entries() {
        let temporary = private_tempdir();
        let expected_bytes = b"abcd";
        let expected_digest = digest(expected_bytes);
        for (name, bytes) in [
            ("exact", expected_bytes.as_slice()),
            ("short", b"abc".as_slice()),
            ("long", b"abcde".as_slice()),
        ] {
            let path = temporary.path().join(name);
            std::fs::write(&path, bytes).unwrap();
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(PRIVATE_FILE_MODE)).unwrap();
        }
        let mut exact = std::fs::OpenOptions::new()
            .read(true)
            .open(temporary.path().join("exact"))
            .unwrap();
        authenticate_asset_file(&mut exact, expected_digest, 4).unwrap();
        for name in ["short", "long"] {
            let mut file = std::fs::OpenOptions::new()
                .read(true)
                .open(temporary.path().join(name))
                .unwrap();
            assert!(authenticate_asset_file(&mut file, expected_digest, 4).is_err());
        }
    }

    #[test]
    fn competing_asset_publishers_reuse_one_verified_winner() {
        let temporary = private_tempdir();
        let directory_path = temporary.path().to_owned();
        let bytes = b"same content from competing publishers".to_vec();
        let name = format!("{:02x}", digest(&bytes));
        let barrier = Arc::new(Barrier::new(2));
        let mut workers = Vec::new();
        for _ in 0..2 {
            let directory_path = directory_path.clone();
            let bytes = bytes.clone();
            let name = name.clone();
            let barrier = Arc::clone(&barrier);
            workers.push(std::thread::spawn(move || {
                let directory = Directory::open_absolute(&directory_path).unwrap();
                let (mut stage, fingerprint, expected) = stage_asset(&directory, &bytes);
                barrier.wait();
                let mut winner = publish_asset_entry(
                    &mut stage,
                    &directory,
                    OsStr::new(&name),
                    fingerprint,
                    expected,
                    bytes.len() as u64,
                )
                .unwrap();
                let mut published = Vec::new();
                use std::io::Read as _;
                winner.read_to_end(&mut published).unwrap();
                published
            }));
        }
        for worker in workers {
            assert_eq!(worker.join().unwrap(), bytes);
        }
        let entries = std::fs::read_dir(&directory_path)
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect::<Vec<_>>();
        assert_eq!(entries, [OsString::from(&name)]);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn competing_download_publishers_reuse_one_verified_winner() {
        let temporary = private_tempdir();
        let destination = Directory::open_absolute(temporary.path()).unwrap();
        let bytes = b"one authenticated package download";
        let expected_hash = sha256(bytes);
        let first_stage = PrivateStageDirectory::create(&destination, ".download-stage-").unwrap();
        let second_stage = PrivateStageDirectory::create(&destination, ".download-stage-").unwrap();
        let staged_name = OsStr::new("package.stone");
        for stage in [&first_stage, &second_stage] {
            std::fs::write(stage.path.join(staged_name), bytes).unwrap();
            std::fs::set_permissions(
                stage.path.join(staged_name),
                std::fs::Permissions::from_mode(PRIVATE_FILE_MODE),
            )
            .unwrap();
            stage.require_inventory(&[staged_name]).unwrap();
        }
        let first_file = authenticate_sha256_entry_async(
            &first_stage.directory,
            staged_name,
            &expected_hash,
            Some(bytes.len() as u64),
            bytes.len() as u64,
            Some(PRIVATE_FILE_MODE),
        )
        .await
        .unwrap();
        let first_fingerprint = FileFingerprint::from_metadata(&first_file.metadata().unwrap());
        let second_file = authenticate_sha256_entry_async(
            &second_stage.directory,
            staged_name,
            &expected_hash,
            Some(bytes.len() as u64),
            bytes.len() as u64,
            Some(PRIVATE_FILE_MODE),
        )
        .await
        .unwrap();
        let second_fingerprint = FileFingerprint::from_metadata(&second_file.metadata().unwrap());

        let first = publish_download_entry_async(
            &first_stage.directory,
            staged_name,
            &destination,
            OsStr::new("winner.stone"),
            first_file,
            first_fingerprint,
            &expected_hash,
            Some(bytes.len() as u64),
            bytes.len() as u64,
        );
        let second = publish_download_entry_async(
            &second_stage.directory,
            staged_name,
            &destination,
            OsStr::new("winner.stone"),
            second_file,
            second_fingerprint,
            &expected_hash,
            Some(bytes.len() as u64),
            bytes.len() as u64,
        );
        let (first, second) = tokio::join!(first, second);
        for mut winner in [first.unwrap(), second.unwrap()] {
            use std::io::{Read as _, Seek as _};
            let mut actual = Vec::new();
            winner.seek(io::SeekFrom::Start(0)).unwrap();
            winner.read_to_end(&mut actual).unwrap();
            assert_eq!(actual, bytes);
        }
        assert_eq!(std::fs::read(temporary.path().join("winner.stone")).unwrap(), bytes);
    }

    #[test]
    fn armed_publication_cleanup_removes_only_the_exact_moved_inode() {
        let temporary = private_tempdir();
        let directory = Directory::open_absolute(temporary.path()).unwrap();
        let path = temporary.path().join("published");
        std::fs::write(&path, b"first").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(PRIVATE_FILE_MODE)).unwrap();
        let fingerprint = FileFingerprint::from_metadata(&std::fs::metadata(&path).unwrap());
        let mut guard = PublishedEntryGuard::new(&directory, OsStr::new("published"), fingerprint).unwrap();
        guard.arm();
        drop(guard);
        assert!(!path.exists());

        std::fs::write(&path, b"old").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(PRIVATE_FILE_MODE)).unwrap();
        let fingerprint = FileFingerprint::from_metadata(&std::fs::metadata(&path).unwrap());
        let mut guard = PublishedEntryGuard::new(&directory, OsStr::new("published"), fingerprint).unwrap();
        guard.arm();
        std::fs::rename(&path, temporary.path().join("detached")).unwrap();
        std::fs::write(&path, b"replacement").unwrap();
        drop(guard);
        assert_eq!(std::fs::read(path).unwrap(), b"replacement");
    }

    #[test]
    fn random_stages_clean_failure_without_truncating_legacy_part_file() {
        let temporary = private_tempdir();
        let directory = Directory::open_absolute(temporary.path()).unwrap();
        let legacy = temporary.path().join("asset.part");
        std::fs::write(&legacy, b"stale sentinel").unwrap();
        let stage = NamedStageFile::create(&directory, ".asset-stage-").unwrap();
        let staged_path = temporary.path().join(&stage.name);
        assert!(staged_path.exists());
        drop(stage);
        assert!(!staged_path.exists());
        assert_eq!(std::fs::read(legacy).unwrap(), b"stale sentinel");

        let anonymous = AnonymousFile::create(&directory, ".content-stage-").unwrap();
        let metadata = anonymous.file.metadata().unwrap();
        assert_eq!(metadata.nlink(), 0);
        assert_eq!(metadata.mode() & 0o7777, PRIVATE_FILE_MODE);

        let download_stage = PrivateStageDirectory::create(&directory, ".download-stage-").unwrap();
        let download_stage_path = download_stage.path.clone();
        std::fs::write(download_stage.path.join(".cast-download-stale"), b"stale").unwrap();
        drop(download_stage);
        assert!(!download_stage_path.exists());
    }
}
