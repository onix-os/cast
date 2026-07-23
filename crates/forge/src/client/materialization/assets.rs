fn frozen_asset_path(digest: u128) -> PathBuf {
    let hash = format!("{digest:02x}");
    let directory = if hash.len() >= 10 {
        PathBuf::from(&hash[..2]).join(&hash[2..4]).join(&hash[4..6])
    } else {
        PathBuf::new()
    };
    directory.join(hash)
}

const MAX_BLIT_ASSET_BYTES: u64 = crate::request::DEFAULT_DOWNLOAD_LIMITS.max_bytes;
const ASSET_COPY_BUFFER_BYTES: usize = 64 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct AssetDirectoryIdentity {
    device: u64,
    inode: u64,
    mode: u32,
    owner: u32,
    group: u32,
}

impl AssetDirectoryIdentity {
    fn from_metadata(metadata: &std::fs::Metadata) -> io::Result<Self> {
        if metadata.mode() & nix::libc::S_IFMT != nix::libc::S_IFDIR {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "asset-cache anchor is not a directory",
            ));
        }
        Ok(Self {
            device: metadata.dev(),
            inode: metadata.ino(),
            mode: metadata.mode(),
            owner: metadata.uid(),
            group: metadata.gid(),
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct AssetFileWitness {
    device: u64,
    inode: u64,
    mode: u32,
    owner: u32,
    group: u32,
    links: u64,
    length: u64,
    modified_seconds: i64,
    modified_nanoseconds: i64,
    changed_seconds: i64,
    changed_nanoseconds: i64,
}

impl AssetFileWitness {
    fn from_metadata(metadata: &std::fs::Metadata) -> Self {
        Self {
            device: metadata.dev(),
            inode: metadata.ino(),
            mode: metadata.mode(),
            owner: metadata.uid(),
            group: metadata.gid(),
            links: metadata.nlink(),
            length: metadata.len(),
            modified_seconds: metadata.mtime(),
            modified_nanoseconds: metadata.mtime_nsec(),
            changed_seconds: metadata.ctime(),
            changed_nanoseconds: metadata.ctime_nsec(),
        }
    }
}

/// Retained descriptor chain from the installation root to `assets/v2`.
///
/// Acquisition may cross host mount points before reaching the configured
/// installation root. Every cache component below that explicit trust anchor
/// is opened with `RESOLVE_BENEATH | NO_SYMLINKS | NO_MAGICLINKS | NO_XDEV`.
/// Both named anchors are then re-opened and compared around each operation so
/// replacing either public pathname fails closed instead of silently changing
/// the source tree.
struct AssetPool {
    installation_path: PathBuf,
    installation_root: fs::File,
    installation_identity: AssetDirectoryIdentity,
    relative_path: PathBuf,
    root: fs::File,
    identity: AssetDirectoryIdentity,
}

impl AssetPool {
    fn open(installation: &Installation) -> Result<Self, Error> {
        Self::open_with_deadline(installation, None)
    }

    fn open_until(installation: &Installation, deadline: Instant) -> Result<Self, Error> {
        Self::open_with_deadline(installation, Some(deadline))
    }

    fn open_with_deadline(installation: &Installation, deadline: Option<Instant>) -> Result<Self, Error> {
        require_asset_deadline(deadline)?;
        let installation_path = complete_asset_io(lexical_absolute_path(&installation.root), deadline)?;
        let assets_path = complete_asset_io(lexical_absolute_path(&installation.assets_path("v2")), deadline)?;
        let relative_path = assets_path
            .strip_prefix(&installation_path)
            .ok()
            .filter(|path| !path.as_os_str().is_empty())
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "asset pool is outside installation root"))?
            .to_owned();
        require_beneath_path(&relative_path)?;

        let installation_root = open_absolute_directory_with_deadline(&installation_path, deadline)?;
        let installation_identity = asset_directory_identity_with_deadline(&installation_root, deadline)?;
        let root = open_asset_path(
            installation_root.as_raw_fd(),
            &relative_path,
            nix::libc::O_RDONLY
                | nix::libc::O_DIRECTORY
                | nix::libc::O_CLOEXEC
                | nix::libc::O_NOFOLLOW
                | nix::libc::O_NONBLOCK,
            asset_resolve_flags(),
            deadline,
        )?;
        let identity = asset_directory_identity_with_deadline(&root, deadline)?;
        let pool = Self {
            installation_path,
            installation_root,
            installation_identity,
            relative_path,
            root,
            identity,
        };
        pool.revalidate_with_deadline(deadline)?;
        Ok(pool)
    }

    fn revalidate(&self) -> Result<(), Error> {
        self.revalidate_with_deadline(None)
    }

    fn revalidate_until(&self, deadline: Instant) -> Result<(), Error> {
        self.revalidate_with_deadline(Some(deadline))
    }

    fn revalidate_with_deadline(&self, deadline: Option<Instant>) -> Result<(), Error> {
        require_asset_deadline(deadline)?;
        if asset_directory_identity_with_deadline(&self.installation_root, deadline)? != self.installation_identity
            || asset_directory_identity_with_deadline(&self.root, deadline)? != self.identity
        {
            return Err(asset_copy_error("retained asset-cache anchor changed"));
        }

        let named_installation = open_absolute_directory_with_deadline(&self.installation_path, deadline)?;
        if asset_directory_identity_with_deadline(&named_installation, deadline)? != self.installation_identity {
            return Err(asset_copy_error(
                "installation root was replaced while using asset cache",
            ));
        }
        let named_root = open_asset_path(
            named_installation.as_raw_fd(),
            &self.relative_path,
            nix::libc::O_RDONLY
                | nix::libc::O_DIRECTORY
                | nix::libc::O_CLOEXEC
                | nix::libc::O_NOFOLLOW
                | nix::libc::O_NONBLOCK,
            asset_resolve_flags(),
            deadline,
        )?;
        if asset_directory_identity_with_deadline(&named_root, deadline)? != self.identity {
            return Err(asset_copy_error("asset pool was replaced while materializing a root"));
        }
        require_asset_deadline(deadline)?;
        Ok(())
    }

    fn open_asset(&self, path: &Path) -> Result<OpenedAsset, Error> {
        self.open_asset_with_deadline(path, None)
    }

    fn open_asset_until(&self, path: &Path, deadline: Instant) -> Result<OpenedAsset, Error> {
        self.open_asset_with_deadline(path, Some(deadline))
    }

    fn open_asset_with_deadline(&self, path: &Path, deadline: Option<Instant>) -> Result<OpenedAsset, Error> {
        self.revalidate_with_deadline(deadline)?;
        require_beneath_path(path)?;
        let name = path
            .file_name()
            .ok_or_else(|| asset_copy_error("asset path has no final component"))?
            .to_owned();
        require_single_component(Path::new(&name))?;
        let parent = match path.parent().filter(|path| !path.as_os_str().is_empty()) {
            Some(parent_path) => open_asset_path(
                self.root.as_raw_fd(),
                parent_path,
                nix::libc::O_RDONLY
                    | nix::libc::O_DIRECTORY
                    | nix::libc::O_CLOEXEC
                    | nix::libc::O_NOFOLLOW
                    | nix::libc::O_NONBLOCK,
                asset_resolve_flags(),
                deadline,
            )?,
            // Cache hashes shorter than ten hexadecimal characters are stored
            // directly under assets/v2. Retain an independently owned clone of
            // the already authenticated pool descriptor as their parent; never
            // reopen the public path or relax the descendant resolution flags.
            None => {
                require_asset_deadline(deadline)?;
                complete_asset_io(self.root.try_clone(), deadline)?
            }
        };
        // Probe through O_PATH first so a hostile FIFO or device is rejected
        // without invoking its open handler. Only an exact regular inode is
        // then opened for bounded nonblocking reads.
        let probe = open_asset_path(
            parent.as_raw_fd(),
            Path::new(&name),
            nix::libc::O_PATH | nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW,
            asset_resolve_flags(),
            deadline,
        )?;
        let witness = asset_source_witness_with_deadline(&probe, deadline)?;
        let file = open_asset_path(
            parent.as_raw_fd(),
            Path::new(&name),
            nix::libc::O_RDONLY | nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW | nix::libc::O_NONBLOCK,
            asset_resolve_flags(),
            deadline,
        )?;
        if asset_source_witness_with_deadline(&file, deadline)? != witness {
            return Err(asset_copy_error(
                "cached asset was replaced between type probe and open",
            ));
        }
        self.revalidate_with_deadline(deadline)?;
        Ok(OpenedAsset {
            path: path.to_owned(),
            parent,
            name,
            file,
            witness,
        })
    }
}

struct OpenedAsset {
    path: PathBuf,
    parent: fs::File,
    name: OsString,
    file: fs::File,
    witness: AssetFileWitness,
}

fn asset_resolve_flags() -> u64 {
    (nix::libc::RESOLVE_BENEATH
        | nix::libc::RESOLVE_NO_SYMLINKS
        | nix::libc::RESOLVE_NO_MAGICLINKS
        | nix::libc::RESOLVE_NO_XDEV) as u64
}

fn require_asset_deadline(deadline: Option<Instant>) -> Result<(), Error> {
    if deadline.is_some_and(|deadline| Instant::now() > deadline) {
        Err(io::Error::new(
            io::ErrorKind::TimedOut,
            "authenticated asset operation exceeded its deadline",
        )
        .into())
    } else {
        Ok(())
    }
}

fn complete_asset_io<T>(result: io::Result<T>, deadline: Option<Instant>) -> Result<T, Error> {
    // Deadline failure wins even when the bounded syscall returned another
    // error after the deadline. This preserves the caller's temporal contract
    // instead of misclassifying a late operation as an ordinary path failure.
    require_asset_deadline(deadline)?;
    result.map_err(Error::from)
}

fn open_asset_path(
    parent: RawFd,
    path: &Path,
    flags: i32,
    resolve: u64,
    deadline: Option<Instant>,
) -> Result<fs::File, Error> {
    require_asset_deadline(deadline)?;
    let result = match deadline {
        Some(deadline) => openat2_frozen_until(parent, path, flags, resolve, deadline),
        None => openat2_frozen(parent, path, flags, resolve),
    };
    complete_asset_io(result, deadline)
}

fn lexical_absolute_path(path: &Path) -> io::Result<PathBuf> {
    let path = if path.is_absolute() {
        path.to_owned()
    } else {
        std::env::current_dir()?.join(path)
    };
    let mut normalized = PathBuf::from("/");
    for component in path.components() {
        match component {
            PathComponent::RootDir => {}
            PathComponent::Normal(component) => normalized.push(component),
            PathComponent::CurDir => {}
            PathComponent::ParentDir | PathComponent::Prefix(_) => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "asset-cache path contains a parent or platform prefix component",
                ));
            }
        }
    }
    Ok(normalized)
}

fn open_absolute_directory(path: &Path) -> Result<fs::File, Error> {
    open_absolute_directory_with_deadline(path, None)
}

fn open_absolute_directory_with_deadline(path: &Path, deadline: Option<Instant>) -> Result<fs::File, Error> {
    require_asset_deadline(deadline)?;
    let relative = path
        .strip_prefix(Path::new("/"))
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "asset-cache anchor is not absolute"))?;
    let system_root = complete_asset_io(fs::File::open("/"), deadline)?;
    if relative.as_os_str().is_empty() {
        return Ok(system_root);
    }
    require_beneath_path(relative)?;
    open_asset_path(
        system_root.as_raw_fd(),
        relative,
        nix::libc::O_RDONLY
            | nix::libc::O_DIRECTORY
            | nix::libc::O_CLOEXEC
            | nix::libc::O_NOFOLLOW
            | nix::libc::O_NONBLOCK,
        (nix::libc::RESOLVE_BENEATH | nix::libc::RESOLVE_NO_SYMLINKS | nix::libc::RESOLVE_NO_MAGICLINKS) as u64,
        deadline,
    )
}

fn require_beneath_path(path: &Path) -> Result<(), Error> {
    if path.is_absolute()
        || path.as_os_str().is_empty()
        || !path
            .components()
            .all(|component| matches!(component, PathComponent::Normal(_)))
    {
        return Err(asset_copy_error(
            "asset-cache path is not a non-empty normalized relative path",
        ));
    }
    Ok(())
}

fn require_single_component(path: &Path) -> Result<(), Error> {
    require_beneath_path(path)?;
    if path.components().count() != 1 {
        return Err(asset_copy_error("asset-cache leaf is not one path component"));
    }
    Ok(())
}

fn asset_directory_identity(file: &fs::File) -> Result<AssetDirectoryIdentity, Error> {
    asset_directory_identity_with_deadline(file, None)
}

fn asset_directory_identity_with_deadline(
    file: &fs::File,
    deadline: Option<Instant>,
) -> Result<AssetDirectoryIdentity, Error> {
    require_asset_deadline(deadline)?;
    let metadata = complete_asset_io(file.metadata(), deadline)?;
    complete_asset_io(AssetDirectoryIdentity::from_metadata(&metadata), deadline)
}

fn asset_source_witness(file: &fs::File) -> Result<AssetFileWitness, Error> {
    asset_source_witness_with_deadline(file, None)
}

fn asset_source_witness_with_deadline(
    file: &fs::File,
    deadline: Option<Instant>,
) -> Result<AssetFileWitness, Error> {
    require_asset_deadline(deadline)?;
    let metadata = complete_asset_io(file.metadata(), deadline)?;
    let witness = AssetFileWitness::from_metadata(&metadata);
    if witness.mode & nix::libc::S_IFMT != nix::libc::S_IFREG {
        return Err(asset_copy_error("cached asset is not a regular file"));
    }
    if witness.links == 0 {
        return Err(asset_copy_error("cached asset has no filesystem links"));
    }
    if witness.length > MAX_BLIT_ASSET_BYTES {
        return Err(asset_copy_error(format!(
            "cached asset is {} bytes, exceeding the {}-byte copy limit",
            witness.length, MAX_BLIT_ASSET_BYTES
        )));
    }
    require_asset_deadline(deadline)?;
    Ok(witness)
}

fn open_named_asset(asset: &OpenedAsset) -> Result<fs::File, Error> {
    open_named_asset_with_deadline(asset, None)
}

fn open_named_asset_with_deadline(
    asset: &OpenedAsset,
    deadline: Option<Instant>,
) -> Result<fs::File, Error> {
    open_asset_path(
        asset.parent.as_raw_fd(),
        Path::new(&asset.name),
        nix::libc::O_RDONLY | nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW | nix::libc::O_NONBLOCK,
        asset_resolve_flags(),
        deadline,
    )
}

fn require_asset_unchanged(pool: &AssetPool, asset: &OpenedAsset) -> Result<(), Error> {
    require_asset_unchanged_with_deadline(pool, asset, None)
}

fn require_asset_unchanged_until(pool: &AssetPool, asset: &OpenedAsset, deadline: Instant) -> Result<(), Error> {
    require_asset_unchanged_with_deadline(pool, asset, Some(deadline))
}

fn require_asset_unchanged_with_deadline(
    pool: &AssetPool,
    asset: &OpenedAsset,
    deadline: Option<Instant>,
) -> Result<(), Error> {
    let descriptor = asset_source_witness_with_deadline(&asset.file, deadline)?;
    let reopened = open_named_asset_with_deadline(asset, deadline)?;
    let named = asset_source_witness_with_deadline(&reopened, deadline)?;
    let full_reopened = pool.open_asset_with_deadline(&asset.path, deadline)?;
    let final_descriptor = asset_source_witness_with_deadline(&asset.file, deadline)?;
    pool.revalidate_with_deadline(deadline)?;
    if descriptor != asset.witness
        || named != asset.witness
        || full_reopened.witness != asset.witness
        || final_descriptor != asset.witness
    {
        return Err(asset_copy_error(
            "cached asset changed or was replaced while being materialized",
        ));
    }
    require_asset_deadline(deadline)?;
    Ok(())
}

fn asset_copy_error(message: impl Into<String>) -> Error {
    io::Error::new(io::ErrorKind::InvalidData, message.into()).into()
}

fn cleanup_failed_materialization(
    parent: RawFd,
    target: &str,
    created: &fs::File,
    expected_links_after: u64,
    primary: Error,
) -> Error {
    let created_identity = match created.metadata() {
        Ok(metadata) => AssetFileWitness::from_metadata(&metadata),
        Err(cleanup) => {
            return asset_copy_error(format!(
                "asset materialization failed: {primary}; stat during cleanup also failed: {cleanup}"
            ));
        }
    };
    let named = openat2_frozen(
        parent,
        Path::new(target),
        nix::libc::O_PATH | nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW,
        (nix::libc::RESOLVE_BENEATH | nix::libc::RESOLVE_NO_MAGICLINKS | nix::libc::RESOLVE_NO_XDEV) as u64,
    );
    match named {
        Ok(named) => {
            let named_identity = match named.metadata() {
                Ok(metadata) => AssetFileWitness::from_metadata(&metadata),
                Err(cleanup) => {
                    return asset_copy_error(format!(
                        "asset materialization failed: {primary}; stat named cleanup target also failed: {cleanup}"
                    ));
                }
            };
            if (named_identity.device, named_identity.inode) != (created_identity.device, created_identity.inode) {
                return asset_copy_error(format!(
                    "asset materialization failed: {primary}; refusing to unlink a replacement cleanup target"
                ));
            }
            if let Err(cleanup) = unlinkat(Some(parent), target, UnlinkatFlags::NoRemoveDir) {
                return asset_copy_error(format!(
                    "asset materialization failed: {primary}; unlink cleanup also failed: {cleanup}"
                ));
            }
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(cleanup) => {
            return asset_copy_error(format!(
                "asset materialization failed: {primary}; reopen during cleanup also failed: {cleanup}"
            ));
        }
    }

    match created.metadata() {
        Ok(metadata) if metadata.nlink() == expected_links_after => primary,
        Ok(metadata) => asset_copy_error(format!(
            "asset materialization failed: {primary}; cleanup left {} links, expected {expected_links_after}",
            metadata.nlink()
        )),
        Err(cleanup) => asset_copy_error(format!(
            "asset materialization failed: {primary}; final cleanup stat also failed: {cleanup}"
        )),
    }
}

fn link_asset(pool: &AssetPool, source: &Path, parent: RawFd, target: &str) -> Result<(), Error> {
    require_single_component(Path::new(target))?;
    let asset = pool.open_asset(source)?;
    linkat(
        Some(asset.parent.as_raw_fd()),
        Path::new(&asset.name),
        Some(parent),
        Path::new(target),
        nix::unistd::LinkatFlags::NoSymlinkFollow,
    )?;

    let result = (|| -> Result<(), Error> {
        let target_file = openat2_frozen(
            parent,
            Path::new(target),
            nix::libc::O_RDONLY | nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW | nix::libc::O_NONBLOCK,
            asset_resolve_flags(),
        )?;
        let target_witness = asset_source_witness(&target_file)?;
        let source_after = asset_source_witness(&asset.file)?;
        if (target_witness.device, target_witness.inode) != (asset.witness.device, asset.witness.inode)
            || source_after.links != asset.witness.links.saturating_add(1)
            || source_after.length != asset.witness.length
            || source_after.mode != asset.witness.mode
        {
            return Err(asset_copy_error(
                "hardlinked asset changed or target names a different inode",
            ));
        }
        let reopened = open_named_asset(&asset)?;
        let named = asset_source_witness(&reopened)?;
        let full_reopened = pool.open_asset(&asset.path)?;
        if (named.device, named.inode) != (asset.witness.device, asset.witness.inode)
            || (full_reopened.witness.device, full_reopened.witness.inode)
                != (asset.witness.device, asset.witness.inode)
            || full_reopened.witness.links != source_after.links
        {
            return Err(asset_copy_error(
                "hardlinked cached asset was replaced during publication",
            ));
        }
        pool.revalidate()
    })();
    if let Err(error) = result {
        return Err(cleanup_failed_materialization(
            parent,
            target,
            &asset.file,
            asset.witness.links,
            error,
        ));
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AssetCopyCheckpoint {
    SourceOpened,
    BytesCopied,
}

/// Copy one cached asset into a fresh inode under `parent`.
///
/// Writable package roots are modified by transaction triggers or build steps,
/// so aliasing them to the persistent content store would let a write or chmod
/// corrupt cached assets. Keep descriptor-relative traversal while giving each
/// destination independent digest-verified bytes and metadata.
fn copy_asset(
    pool: &AssetPool,
    source: &Path,
    expected_digest: u128,
    parent: RawFd,
    target: &str,
    mode: u32,
    copy_manifest: Option<&FrozenCopyManifest>,
    deadline: Option<Instant>,
) -> Result<(), Error> {
    copy_asset_with_checkpoint(
        pool,
        source,
        expected_digest,
        parent,
        target,
        mode,
        copy_manifest,
        deadline,
        |_| {},
    )
}

fn copy_asset_with_checkpoint<F>(
    pool: &AssetPool,
    source: &Path,
    expected_digest: u128,
    parent: RawFd,
    target: &str,
    mode: u32,
    copy_manifest: Option<&FrozenCopyManifest>,
    deadline: Option<Instant>,
    mut checkpoint: F,
) -> Result<(), Error>
where
    F: FnMut(AssetCopyCheckpoint),
{
    require_blit_deadline(deadline)?;
    require_single_component(Path::new(target))?;
    let asset = pool.open_asset(source)?;
    if let Some(copy_manifest) = copy_manifest {
        copy_manifest.require_length(expected_digest, asset.witness.length)?;
    }
    checkpoint(AssetCopyCheckpoint::SourceOpened);
    pool.revalidate()?;
    let target_fd = openat2_frozen(
        parent,
        Path::new(target),
        nix::libc::O_CLOEXEC
            | nix::libc::O_CREAT
            | nix::libc::O_EXCL
            | nix::libc::O_NOFOLLOW
            | nix::libc::O_NONBLOCK
            | nix::libc::O_WRONLY,
        asset_resolve_flags(),
    )?;

    let result = (|| -> Result<(), Error> {
        fchmod(target_fd.as_raw_fd(), Mode::from_bits_truncate(0o600))?;
        require_private_copy_target(&target_fd, 0, 0o600)?;
        copy_fd_exact(
            asset.file.as_raw_fd(),
            target_fd.as_raw_fd(),
            asset.witness.length,
            expected_digest,
            deadline,
        )?;
        checkpoint(AssetCopyCheckpoint::BytesCopied);
        require_asset_unchanged(pool, &asset)?;
        fchmod(target_fd.as_raw_fd(), Mode::from_bits_truncate(mode))?;
        target_fd.sync_data()?;
        let expected_target = require_copy_target(&target_fd, asset.witness.length, mode)?;
        let named_target = openat2_frozen(
            parent,
            Path::new(target),
            nix::libc::O_RDONLY | nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW | nix::libc::O_NONBLOCK,
            asset_resolve_flags(),
        )?;
        let named_witness = require_copy_target(&named_target, asset.witness.length, mode)?;
        let final_target = require_copy_target(&target_fd, asset.witness.length, mode)?;
        if named_witness != expected_target || final_target != expected_target {
            return Err(asset_copy_error(
                "copied asset target changed or was replaced before publication",
            ));
        }
        Ok(())
    })();
    if let Err(error) = result {
        return Err(cleanup_failed_materialization(parent, target, &target_fd, 0, error));
    }

    Ok(())
}

fn require_private_copy_target(file: &fs::File, expected_length: u64, expected_mode: u32) -> Result<(), Error> {
    let witness = require_copy_target(file, expected_length, expected_mode)?;
    // SAFETY: `geteuid` has no preconditions and cannot fail.
    let effective_owner = unsafe { nix::libc::geteuid() };
    if witness.owner != effective_owner {
        return Err(asset_copy_error(
            "fresh asset-copy target is not owned by the effective user",
        ));
    }
    Ok(())
}

fn require_copy_target(file: &fs::File, expected_length: u64, expected_mode: u32) -> Result<AssetFileWitness, Error> {
    let witness = AssetFileWitness::from_metadata(&file.metadata()?);
    let expected_permissions = expected_mode & 0o7777;
    if witness.mode & nix::libc::S_IFMT != nix::libc::S_IFREG
        || witness.links != 1
        || witness.length != expected_length
        || witness.mode & 0o7777 != expected_permissions
    {
        return Err(asset_copy_error(format!(
            "asset-copy target metadata mismatch: mode {:#o}, links {}, length {}; expected permissions {:#o}, one link, length {}",
            witness.mode, witness.links, witness.length, expected_permissions, expected_length
        )));
    }
    Ok(witness)
}

fn openat_owned<P: ?Sized + nix::NixPath>(parent: RawFd, path: &P, flags: OFlag, mode: Mode) -> Result<OwnedFd, Errno> {
    fcntl::openat(parent, path, flags, mode).map(raw_fd_into_owned)
}

fn raw_fd_into_owned(fd: RawFd) -> OwnedFd {
    // SAFETY: every successful nix open/openat call returns one newly owned
    // descriptor. Ownership is transferred exactly once to OwnedFd here.
    unsafe { OwnedFd::from_raw_fd(fd) }
}

fn copy_fd_exact(
    source: RawFd,
    target: RawFd,
    expected_length: u64,
    expected_digest: u128,
    deadline: Option<Instant>,
) -> Result<boot_content_identity::BootContentIdentity, Error> {
    if expected_length > MAX_BLIT_ASSET_BYTES {
        return Err(asset_copy_error(format!(
            "asset-copy length {expected_length} exceeds {MAX_BLIT_ASSET_BYTES} bytes"
        )));
    }
    let mut buffer = [0_u8; ASSET_COPY_BUFFER_BYTES];
    let mut remaining = expected_length;
    let mut hasher = StoneDigestWriterHasher::new();
    let mut content_hasher = <sha2::Sha256 as sha2::Digest>::new();

    while remaining != 0 {
        require_blit_deadline(deadline)?;
        let requested = usize::try_from(remaining.min(buffer.len() as u64))
            .map_err(|_| asset_copy_error("asset-copy chunk length is not representable"))?;
        let read_count = match read(source, &mut buffer[..requested]) {
            Ok(count) => count,
            Err(Errno::EINTR) => continue,
            Err(error) => return Err(error.into()),
        };
        if read_count == 0 {
            return Err(asset_copy_error(format!(
                "cached asset ended early with {remaining} bytes still required"
            )));
        }
        hasher.update(&buffer[..read_count]);
        sha2::Digest::update(&mut content_hasher, &buffer[..read_count]);
        remaining = remaining
            .checked_sub(read_count as u64)
            .ok_or_else(|| asset_copy_error("asset-copy byte count underflow"))?;

        let mut written = 0;
        while written < read_count {
            require_blit_deadline(deadline)?;
            match write(target, &buffer[written..read_count]) {
                Ok(0) => return Err(Errno::EIO.into()),
                Ok(count) => written += count,
                Err(Errno::EINTR) => {}
                Err(error) => return Err(error.into()),
            }
        }
    }

    require_blit_deadline(deadline)?;
    let trailing = loop {
        match read(source, &mut buffer[..1]) {
            Ok(count) => break count,
            Err(Errno::EINTR) => require_blit_deadline(deadline)?,
            Err(error) => return Err(error.into()),
        }
    };
    if trailing != 0 {
        return Err(asset_copy_error(format!(
            "cached asset exceeds its pinned {expected_length}-byte length"
        )));
    }

    let actual_digest = hasher.digest128();
    if actual_digest != expected_digest {
        return Err(asset_copy_error(format!(
            "cached asset digest mismatch: expected {expected_digest:032x}, got {actual_digest:032x}"
        )));
    }
    Ok(boot_content_identity::BootContentIdentity::from_sha256(
        sha2::Digest::finalize(content_hasher).into(),
    ))
}
