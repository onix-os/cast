use std::{
    io,
    os::{fd::AsRawFd as _, unix::fs::MetadataExt as _},
    path::Path,
};

use thiserror::Error;

use crate::linux_fs::{
    authenticated_current_thread_procfs, authenticated_procfs_root, controlled_resolution, descriptor_mount_id,
    mount_namespace::authenticate_mount_namespace_descriptor, openat2_file, read_to_end_bounded, require_procfs,
};

use super::{BootId, CodecError, MountNamespaceIdentity, RuntimeEpoch, RuntimeTreeIdentity};

const BOOT_ID_FILE_BYTES: usize = BootId::TEXT_LENGTH + 1;

impl RuntimeEpoch {
    /// Capture the boot and mount-namespace epoch through authenticated
    /// procfs capabilities. Runtime inode and mount witnesses are meaningful
    /// only while this complete value still matches.
    pub(crate) fn capture() -> Result<Self, RuntimeEvidenceError> {
        Ok(Self {
            boot_id: capture_boot_id()?,
            mount_namespace: capture_mount_namespace()?,
        })
    }
}

impl RuntimeTreeIdentity {
    /// Capture one exact retained `/usr` directory and its Linux 5.6 mount ID.
    pub(crate) fn capture_directory(directory: &std::fs::File) -> Result<Self, RuntimeEvidenceError> {
        let before = directory.metadata().map_err(RuntimeEvidenceError::InspectTree)?;
        if !before.file_type().is_dir() {
            return Err(RuntimeEvidenceError::TreeIsNotDirectory);
        }
        let mount_id = descriptor_mount_id(directory).map_err(RuntimeEvidenceError::ReadTreeMountId)?;
        let after = directory.metadata().map_err(RuntimeEvidenceError::InspectTree)?;
        if before.dev() != after.dev() || before.ino() != after.ino() {
            return Err(RuntimeEvidenceError::TreeChanged);
        }
        let identity = Self {
            st_dev: before.dev(),
            inode: before.ino(),
            mount_id,
        };
        if identity.st_dev == 0 || identity.inode == 0 || identity.mount_id == 0 {
            return Err(RuntimeEvidenceError::ZeroTreeIdentity);
        }
        Ok(identity)
    }
}

fn capture_boot_id() -> Result<BootId, RuntimeEvidenceError> {
    let proc = authenticated_procfs_root().map_err(RuntimeEvidenceError::OpenProcfs)?;
    let mut file = openat2_file(
        proc.as_raw_fd(),
        c"sys/kernel/random/boot_id",
        nix::libc::O_RDONLY | nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW | nix::libc::O_NONBLOCK,
        0,
        controlled_resolution(),
    )
    .map_err(RuntimeEvidenceError::OpenBootId)?;
    require_procfs(&file, Path::new("/proc/sys/kernel/random/boot_id"))
        .map_err(RuntimeEvidenceError::AuthenticateBootId)?;

    let bytes = read_to_end_bounded(&mut file, BOOT_ID_FILE_BYTES + 1).map_err(RuntimeEvidenceError::ReadBootId)?;
    parse_boot_id_bytes(&bytes)
}

pub(super) fn parse_boot_id_bytes(bytes: &[u8]) -> Result<BootId, RuntimeEvidenceError> {
    if bytes.len() != BOOT_ID_FILE_BYTES || bytes.last() != Some(&b'\n') {
        return Err(RuntimeEvidenceError::NoncanonicalBootIdFile);
    }
    let value =
        std::str::from_utf8(&bytes[..BootId::TEXT_LENGTH]).map_err(|_| RuntimeEvidenceError::NoncanonicalBootIdFile)?;
    BootId::parse(value).map_err(RuntimeEvidenceError::InvalidBootId)
}

fn capture_mount_namespace() -> Result<MountNamespaceIdentity, RuntimeEvidenceError> {
    let thread = authenticated_current_thread_procfs().map_err(RuntimeEvidenceError::OpenCurrentThreadProcfs)?;
    let namespace_directory = openat2_file(
        thread.as_raw_fd(),
        c"ns",
        nix::libc::O_PATH | nix::libc::O_DIRECTORY | nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW,
        0,
        controlled_resolution(),
    )
    .map_err(RuntimeEvidenceError::OpenNamespaceDirectory)?;
    require_procfs(&namespace_directory, Path::new("/proc/<pid>/task/<tid>/ns"))
        .map_err(RuntimeEvidenceError::AuthenticateNamespaceDirectory)?;

    // This is one intentional procfs magic-link traversal. The parent is the
    // exact authenticated current-thread namespace directory and `mnt` is a
    // fixed kernel-owned component. The resulting descriptor must be nsfs.
    let namespace = openat2_file(
        namespace_directory.as_raw_fd(),
        c"mnt",
        nix::libc::O_RDONLY | nix::libc::O_CLOEXEC,
        0,
        0,
    )
    .map_err(RuntimeEvidenceError::OpenMountNamespace)?;
    mount_namespace_identity(&namespace)
}

pub(super) fn mount_namespace_identity(
    namespace: &std::fs::File,
) -> Result<MountNamespaceIdentity, RuntimeEvidenceError> {
    let authenticated = authenticate_mount_namespace_descriptor(namespace, None)
        .map_err(RuntimeEvidenceError::AuthenticateMountNamespace)?;
    let identity = MountNamespaceIdentity {
        st_dev: authenticated.device,
        inode: authenticated.inode,
    };
    if identity.st_dev == 0 || identity.inode == 0 {
        return Err(RuntimeEvidenceError::ZeroMountNamespaceIdentity);
    }
    Ok(identity)
}

#[derive(Debug, Error)]
pub(crate) enum RuntimeEvidenceError {
    #[error("open authenticated procfs root for transition epoch capture")]
    OpenProcfs(#[source] io::Error),
    #[error("open kernel boot ID below authenticated procfs")]
    OpenBootId(#[source] io::Error),
    #[error("authenticate kernel boot ID as procfs")]
    AuthenticateBootId(#[source] io::Error),
    #[error("read bounded kernel boot ID")]
    ReadBootId(#[source] io::Error),
    #[error("kernel boot ID file is not exactly one canonical newline-terminated ID")]
    NoncanonicalBootIdFile,
    #[error("parse canonical kernel boot ID")]
    InvalidBootId(#[source] CodecError),
    #[error("open authenticated current-thread procfs directory")]
    OpenCurrentThreadProcfs(#[source] io::Error),
    #[error("open current-thread namespace directory")]
    OpenNamespaceDirectory(#[source] io::Error),
    #[error("authenticate current-thread namespace directory as procfs")]
    AuthenticateNamespaceDirectory(#[source] io::Error),
    #[error("open current mount namespace through authenticated procfs")]
    OpenMountNamespace(#[source] io::Error),
    #[error("authenticate current mount-namespace descriptor as nsfs")]
    AuthenticateMountNamespace(#[source] io::Error),
    #[error("current mount-namespace identity contains a zero field")]
    ZeroMountNamespaceIdentity,
    #[error("inspect retained transition tree")]
    InspectTree(#[source] io::Error),
    #[error("transition tree descriptor is not a directory")]
    TreeIsNotDirectory,
    #[error("read retained transition-tree mount ID")]
    ReadTreeMountId(#[source] io::Error),
    #[error("retained transition-tree descriptor changed during evidence capture")]
    TreeChanged,
    #[error("retained transition-tree runtime identity contains a zero field")]
    ZeroTreeIdentity,
}
