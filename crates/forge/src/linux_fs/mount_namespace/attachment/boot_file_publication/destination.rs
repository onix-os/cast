use std::{
    ffi::CStr,
    fs::File,
    io,
    os::{
        fd::AsRawFd as _,
        unix::fs::{MetadataExt as _, PermissionsExt as _},
    },
    time::Instant,
};

use sha2::{Digest as _, Sha256};
use xxhash_rust::xxh3::Xxh3;

use super::{
    AttachmentIdentity,
    effect::checkpoint,
    error::RetainedBootFilePublicationError,
    model::RetainedBootFilePublicationRequest,
};
use crate::linux_fs::{controlled_resolution, descriptor_mount_id_until, openat2_file_until};

const READ_BUFFER_BYTES: usize = 4 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::linux_fs::mount_namespace::attachment) struct FileIdentity {
    pub(in crate::linux_fs::mount_namespace::attachment) device: u64,
    pub(in crate::linux_fs::mount_namespace::attachment) inode: u64,
    pub(in crate::linux_fs::mount_namespace::attachment) mount_id: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum ReconciledMove {
    Applied,
    NotApplied,
    Ambiguous,
}

pub(in crate::linux_fs::mount_namespace::attachment) fn open_parent_io(
    retained_parent: &File,
    expected: AttachmentIdentity,
    deadline: Instant,
) -> Result<File, RetainedBootFilePublicationError> {
    checkpoint(deadline)?;
    let opened = openat2_file_until(
        retained_parent.as_raw_fd(),
        c".",
        nix::libc::O_RDONLY
            | nix::libc::O_DIRECTORY
            | nix::libc::O_CLOEXEC
            | nix::libc::O_NOFOLLOW
            | nix::libc::O_NOATIME,
        0,
        controlled_resolution(),
        deadline,
    )
    .map_err(|source| RetainedBootFilePublicationError::Filesystem {
        action: "opening a readable alias of the retained boot-file parent",
        source,
    })?;
    require_attachment_identity(&opened, expected, "binding readable boot-file parent", deadline)?;
    Ok(opened)
}

pub(in crate::linux_fs::mount_namespace::attachment) fn create_private_exclusive(
    parent: &File,
    name: &CStr,
    expected_parent: AttachmentIdentity,
    deadline: Instant,
) -> Result<File, io::Error> {
    let file = openat2_file_until(
        parent.as_raw_fd(),
        name,
        nix::libc::O_RDWR
            | nix::libc::O_CREAT
            | nix::libc::O_EXCL
            | nix::libc::O_CLOEXEC
            | nix::libc::O_NOFOLLOW,
        0o644,
        controlled_resolution(),
        deadline,
    )?;
    let opening = file.metadata()?;
    if !opening.file_type().is_file()
        || opening.len() != 0
        || opening.nlink() != 1
        || opening.dev() != expected_parent.device
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "exclusive private boot-file creation returned an invalid inode",
        ));
    }
    if opening.permissions().mode() & 0o7777 != 0o644 {
        // Ordinary Unix umask processing can remove requested mode bits. This
        // is the sole permitted normalization point: the inode is still empty
        // and has not crossed any streaming boundary. VFAT with `fmask=0133`
        // already reports 0644 and never enters this branch.
        // SAFETY: `file` is the fresh retained descriptor and the syscall
        // retains neither the descriptor nor the scalar mode.
        if unsafe { nix::libc::fchmod(file.as_raw_fd(), 0o644) } != 0 {
            return Err(io::Error::last_os_error());
        }
    }
    let admitted = file.metadata()?;
    if !admitted.file_type().is_file()
        || admitted.len() != 0
        || admitted.nlink() != 1
        || admitted.dev() != expected_parent.device
        || admitted.ino() != opening.ino()
        || admitted.permissions().mode() & 0o7777 != 0o644
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "exclusive private boot-file mode admission did not produce one empty 0644 inode",
        ));
    }
    let mount_id = descriptor_mount_id_until(&file, deadline)?;
    if mount_id != expected_parent.mount_id {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "exclusive private boot-file creation crossed the retained attachment",
        ));
    }
    Ok(file)
}

pub(in crate::linux_fs::mount_namespace::attachment) fn open_and_verify(
    parent: &File,
    name: &CStr,
    request: RetainedBootFilePublicationRequest<'_>,
    expected_parent: AttachmentIdentity,
    deadline: Instant,
) -> Result<(File, FileIdentity), RetainedBootFilePublicationError> {
    checkpoint(deadline)?;
    let file = openat2_file_until(
        parent.as_raw_fd(),
        name,
        nix::libc::O_RDONLY | nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW | nix::libc::O_NOATIME,
        0,
        controlled_resolution(),
        deadline,
    )
    .map_err(|source| RetainedBootFilePublicationError::Filesystem {
        action: "opening one exact boot-file leaf",
        source,
    })?;
    let identity = verify_open_file(&file, request, expected_parent, deadline)?;
    Ok((file, identity))
}

pub(in crate::linux_fs::mount_namespace::attachment) fn verify_open_file(
    file: &File,
    request: RetainedBootFilePublicationRequest<'_>,
    expected_parent: AttachmentIdentity,
    deadline: Instant,
) -> Result<FileIdentity, RetainedBootFilePublicationError> {
    checkpoint(deadline)?;
    let opening = file.metadata().map_err(|source| RetainedBootFilePublicationError::Filesystem {
        action: "observing opening boot-file metadata",
        source,
    })?;
    require_regular_metadata(&opening, request, expected_parent)?;
    let mount_id = descriptor_mount_id_until(file, deadline).map_err(|source| {
        RetainedBootFilePublicationError::Filesystem {
            action: "observing boot-file attachment mount ID",
            source,
        }
    })?;
    if mount_id != expected_parent.mount_id {
        return Err(RetainedBootFilePublicationError::DestinationIdentityChanged {
            action: "matching boot-file mount ID",
        });
    }

    let mut xxh3 = Xxh3::new();
    let mut sha256 = Sha256::new();
    let mut buffer = [0u8; READ_BUFFER_BYTES];
    let mut offset = 0u64;
    while offset < request.expected_length() {
        checkpoint(deadline)?;
        let offered = usize::try_from((request.expected_length() - offset).min(READ_BUFFER_BYTES as u64))
            .expect("fixed boot-file read buffer fits usize");
        let found = pread_once(file, offset, &mut buffer[..offered]).map_err(|source| {
            RetainedBootFilePublicationError::Filesystem {
                action: "verifying boot-file content",
                source,
            }
        })?;
        if found == 0 || found > offered {
            return Err(RetainedBootFilePublicationError::ContentIdentityMismatch {
                field: "destination length",
            });
        }
        xxh3.update(&buffer[..found]);
        sha256.update(&buffer[..found]);
        offset = offset.checked_add(found as u64).ok_or(
            RetainedBootFilePublicationError::ContentIdentityMismatch {
                field: "destination offset",
            },
        )?;
    }
    let mut probe = [0u8; 1];
    if pread_once(file, request.expected_length(), &mut probe).map_err(|source| {
        RetainedBootFilePublicationError::Filesystem {
            action: "probing terminal boot-file length",
            source,
        }
    })? != 0
    {
        return Err(RetainedBootFilePublicationError::ContentIdentityMismatch {
            field: "destination terminal length",
        });
    }
    if xxh3.digest128() != request.expected_xxh3() {
        return Err(RetainedBootFilePublicationError::ContentIdentityMismatch {
            field: "destination XXH3",
        });
    }
    let actual_sha256: [u8; 32] = sha256.finalize().into();
    if actual_sha256 != request.expected_sha256() {
        return Err(RetainedBootFilePublicationError::ContentIdentityMismatch {
            field: "destination SHA-256",
        });
    }
    let closing = file.metadata().map_err(|source| RetainedBootFilePublicationError::Filesystem {
        action: "observing closing boot-file metadata",
        source,
    })?;
    if metadata_identity(&opening) != metadata_identity(&closing)
        || opening.len() != closing.len()
        || opening.permissions().mode() != closing.permissions().mode()
        || opening.nlink() != closing.nlink()
    {
        return Err(RetainedBootFilePublicationError::DestinationIdentityChanged {
            action: "sandwiching boot-file content verification",
        });
    }
    checkpoint(deadline)?;
    Ok(FileIdentity {
        device: opening.dev(),
        inode: opening.ino(),
        mount_id,
    })
}

pub(in crate::linux_fs::mount_namespace::attachment) fn require_named_identity(
    parent: &File,
    name: &CStr,
    expected: FileIdentity,
    deadline: Instant,
) -> Result<(), RetainedBootFilePublicationError> {
    match observe_named_identity(parent, name, deadline)? {
        Some(found) if found == expected => Ok(()),
        _ => Err(RetainedBootFilePublicationError::DestinationIdentityChanged {
            action: "rebinding one retained boot-file name",
        }),
    }
}

pub(super) fn reconcile_move(
    parent: &File,
    private_name: &CStr,
    canonical_name: &CStr,
    retained: FileIdentity,
    deadline: Instant,
) -> Result<ReconciledMove, RetainedBootFilePublicationError> {
    let private = observe_named_identity(parent, private_name, deadline)?;
    let canonical = observe_named_identity(parent, canonical_name, deadline)?;
    Ok(match (private, canonical) {
        (None, Some(found)) if found == retained => ReconciledMove::Applied,
        (Some(found), None) if found == retained => ReconciledMove::NotApplied,
        _ => ReconciledMove::Ambiguous,
    })
}

pub(in crate::linux_fs::mount_namespace::attachment) fn observe_named_identity(
    parent: &File,
    name: &CStr,
    deadline: Instant,
) -> Result<Option<FileIdentity>, RetainedBootFilePublicationError> {
    checkpoint(deadline)?;
    let file = match openat2_file_until(
        parent.as_raw_fd(),
        name,
        nix::libc::O_PATH | nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW,
        0,
        controlled_resolution(),
        deadline,
    ) {
        Ok(file) => file,
        Err(source) if source.raw_os_error() == Some(nix::libc::ENOENT) => return Ok(None),
        Err(source) => {
            return Err(RetainedBootFilePublicationError::Filesystem {
                action: "observing one boot-file publication name",
                source,
            });
        }
    };
    let metadata = file.metadata().map_err(|source| RetainedBootFilePublicationError::Filesystem {
        action: "observing one boot-file publication inode",
        source,
    })?;
    if !metadata.file_type().is_file() {
        return Err(RetainedBootFilePublicationError::RenameAmbiguous);
    }
    let mount_id = descriptor_mount_id_until(&file, deadline).map_err(|source| {
        RetainedBootFilePublicationError::Filesystem {
            action: "observing one boot-file publication mount ID",
            source,
        }
    })?;
    Ok(Some(FileIdentity {
        device: metadata.dev(),
        inode: metadata.ino(),
        mount_id,
    }))
}

fn require_attachment_identity(
    file: &File,
    expected: AttachmentIdentity,
    action: &'static str,
    deadline: Instant,
) -> Result<(), RetainedBootFilePublicationError> {
    let metadata = file.metadata().map_err(|source| RetainedBootFilePublicationError::Filesystem { action, source })?;
    let mount_id = descriptor_mount_id_until(file, deadline)
        .map_err(|source| RetainedBootFilePublicationError::Filesystem { action, source })?;
    if !metadata.file_type().is_dir()
        || metadata.dev() != expected.device
        || metadata.ino() != expected.inode
        || mount_id != expected.mount_id
    {
        return Err(RetainedBootFilePublicationError::DestinationIdentityChanged { action });
    }
    Ok(())
}

fn require_regular_metadata(
    metadata: &std::fs::Metadata,
    request: RetainedBootFilePublicationRequest<'_>,
    expected_parent: AttachmentIdentity,
) -> Result<(), RetainedBootFilePublicationError> {
    if !metadata.file_type().is_file()
        || metadata.nlink() != 1
        || metadata.len() != request.expected_length()
        || metadata.permissions().mode() & 0o7777 != 0o644
        || metadata.dev() != expected_parent.device
        || metadata.ino() == 0
    {
        return Err(RetainedBootFilePublicationError::ContentIdentityMismatch {
            field: "destination metadata or effective mode",
        });
    }
    Ok(())
}

fn metadata_identity(metadata: &std::fs::Metadata) -> (u64, u64) {
    (metadata.dev(), metadata.ino())
}

fn pread_once(file: &File, offset: u64, output: &mut [u8]) -> io::Result<usize> {
    let offset = nix::libc::off_t::try_from(offset)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "boot-file read offset exceeds off_t"))?;
    // SAFETY: output and the retained readable descriptor remain live for the
    // one positional read. The syscall retains neither argument.
    let found = unsafe { nix::libc::pread(file.as_raw_fd(), output.as_mut_ptr().cast(), output.len(), offset) };
    if found < 0 {
        Err(io::Error::last_os_error())
    } else {
        usize::try_from(found).map_err(|_| io::Error::other("pread returned an oversized byte count"))
    }
}
