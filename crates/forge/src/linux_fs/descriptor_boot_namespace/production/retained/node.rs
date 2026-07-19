use std::{ffi::CStr, fs::File, io, os::fd::AsFd as _};

use super::super::super::observer::{BootNamespaceNodeIdentity, BootNamespaceNodeKind};
use super::{
    error::RetainedBootNamespaceAssessmentError,
    limits::LiveLedger,
    syscall::{descriptor_flags_once, descriptor_mount_id_once, fstat_once, openat2_once},
};

pub(super) const RETAINED_LOOKUP_RESOLUTION: u64 =
    (nix::libc::RESOLVE_BENEATH | nix::libc::RESOLVE_NO_MAGICLINKS | nix::libc::RESOLVE_NO_SYMLINKS) as u64;
const _: () = assert!(RETAINED_LOOKUP_RESOLUTION & nix::libc::RESOLVE_NO_XDEV as u64 == 0);

const PATH_FLAGS: i32 = nix::libc::O_PATH | nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW;
const READ_FLAGS: i32 = nix::libc::O_RDONLY | nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW | nix::libc::O_NOATIME;
const DIRECTORY_READ_FLAGS: i32 = READ_FLAGS | nix::libc::O_DIRECTORY;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct NodeStat {
    pub(super) device: u64,
    pub(super) inode: u64,
    pub(super) mode: u32,
    pub(super) links: u64,
    pub(super) uid: u32,
    pub(super) gid: u32,
    pub(super) special_device: u64,
    pub(super) size: u64,
    pub(super) block_size: i64,
    pub(super) blocks: i64,
    pub(super) access_seconds: i64,
    pub(super) access_nanoseconds: i64,
    pub(super) modify_seconds: i64,
    pub(super) modify_nanoseconds: i64,
    pub(super) change_seconds: i64,
    pub(super) change_nanoseconds: i64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct NodeObservation {
    pub(super) identity: BootNamespaceNodeIdentity,
    pub(super) kind: BootNamespaceNodeKind,
    pub(super) stat: NodeStat,
}

/// One owned descriptor whose conservative pre-open admission slot is live.
/// Callers must consume it through `close` or transfer it into a retained node
/// whose LIFO release path does so.
pub(super) struct AccountedFile(File);

impl AccountedFile {
    pub(super) const fn file(&self) -> &File {
        &self.0
    }

    pub(super) fn close(self, ledger: &mut LiveLedger) {
        drop(self.0);
        ledger.release_descriptor_slot();
    }
}

pub(super) fn observe_retained_path(
    file: &File,
    ledger: &mut LiveLedger,
    action: &'static str,
) -> Result<NodeObservation, RetainedBootNamespaceAssessmentError> {
    let flags = descriptor_flags(file, ledger, action)?;
    if flags & nix::libc::O_PATH == 0 || flags & nix::libc::O_ACCMODE != nix::libc::O_RDONLY {
        return Err(invalid_data(action, "descriptor is not a retained O_PATH capability"));
    }
    observe_sandwiched(file, ledger, action)
}

pub(super) fn observe_readable(
    file: &File,
    expected: NodeObservation,
    require_directory: bool,
    ledger: &mut LiveLedger,
    action: &'static str,
) -> Result<NodeObservation, RetainedBootNamespaceAssessmentError> {
    let flags = descriptor_flags(file, ledger, action)?;
    if flags & nix::libc::O_PATH != 0
        || flags & nix::libc::O_ACCMODE != nix::libc::O_RDONLY
        || flags & nix::libc::O_NOATIME == 0
        || (require_directory && flags & nix::libc::O_DIRECTORY == 0)
    {
        return Err(invalid_data(
            action,
            "descriptor is not the required read-only O_NOATIME description",
        ));
    }
    let found = observe_sandwiched(file, ledger, action)?;
    if found != expected {
        return Err(invalid_data(
            action,
            "readable description does not match the retained O_PATH node",
        ));
    }
    Ok(found)
}

pub(super) fn open_path_component(
    parent: &File,
    raw_name: &[u8],
    ledger: &mut LiveLedger,
    action: &'static str,
) -> Result<Option<AccountedFile>, RetainedBootNamespaceAssessmentError> {
    let name = component(raw_name, action)?;
    match open_relative(parent, name.as_c_str(), PATH_FLAGS, ledger, action) {
        Ok(file) => Ok(Some(file)),
        Err(RetainedBootNamespaceAssessmentError::Filesystem { source, .. })
            if source.raw_os_error() == Some(nix::libc::ENOENT) =>
        {
            Ok(None)
        }
        Err(error) => Err(error),
    }
}

pub(super) fn open_regular_reader(
    parent: &File,
    raw_name: &[u8],
    expected: NodeObservation,
    ledger: &mut LiveLedger,
) -> Result<AccountedFile, RetainedBootNamespaceAssessmentError> {
    let action = "opening one exact regular O_RDONLY O_NOATIME description";
    let name = component(raw_name, action)?;
    let reader = open_relative(parent, name.as_c_str(), READ_FLAGS, ledger, action)?;
    match observe_readable(reader.file(), expected, false, ledger, action) {
        Ok(_) => Ok(reader),
        Err(error) => {
            reader.close(ledger);
            Err(error)
        }
    }
}

pub(super) fn open_fresh_directory_reader(
    directory: &File,
    expected: NodeObservation,
    ledger: &mut LiveLedger,
) -> Result<AccountedFile, RetainedBootNamespaceAssessmentError> {
    let action = "opening one fresh offset-zero directory description";
    let reader = open_relative(directory, c".", DIRECTORY_READ_FLAGS, ledger, action)?;
    match observe_readable(reader.file(), expected, true, ledger, action) {
        Ok(_) => Ok(reader),
        Err(error) => {
            reader.close(ledger);
            Err(error)
        }
    }
}

fn open_relative(
    parent: &File,
    name: &CStr,
    flags: i32,
    ledger: &mut LiveLedger,
    action: &'static str,
) -> Result<AccountedFile, RetainedBootNamespaceAssessmentError> {
    ledger.reserve_descriptor_slot(action)?;
    let opened = (|| {
        ledger.admit_observation_io_attempt(action)?;
        let opened = openat2_once(parent.as_fd(), name, flags, 0, RETAINED_LOOKUP_RESOLUTION)
            .map_err(|source| RetainedBootNamespaceAssessmentError::Filesystem { action, source });
        ledger.complete_observation_io_attempt()?;
        Ok(AccountedFile(opened?))
    })();
    if opened.is_err() {
        ledger.release_descriptor_slot();
    }
    opened
}

fn observe_sandwiched(
    file: &File,
    ledger: &mut LiveLedger,
    action: &'static str,
) -> Result<NodeObservation, RetainedBootNamespaceAssessmentError> {
    let opening = file_stat(file, ledger, action)?;
    ledger.admit_observation_io_attempt("capturing one retained descriptor mount ID")?;
    let mount_id =
        descriptor_mount_id_once(file.as_fd()).map_err(|source| RetainedBootNamespaceAssessmentError::Filesystem {
            action: "capturing one retained descriptor mount ID",
            source,
        });
    ledger.complete_observation_io_attempt()?;
    let mount_id = mount_id?;
    let closing = file_stat(file, ledger, action)?;
    if opening != closing {
        return Err(invalid_data(
            action,
            "descriptor metadata changed around mount-ID capture",
        ));
    }
    if opening.device == 0 || opening.inode == 0 || mount_id == 0 {
        return Err(invalid_data(action, "descriptor identity contains a zero scalar"));
    }
    Ok(NodeObservation {
        identity: BootNamespaceNodeIdentity::new(opening.device, opening.inode, mount_id),
        kind: node_kind(opening.mode).ok_or_else(|| invalid_data(action, "descriptor has an unknown node kind"))?,
        stat: opening,
    })
}

fn descriptor_flags(
    file: &File,
    ledger: &mut LiveLedger,
    action: &'static str,
) -> Result<i32, RetainedBootNamespaceAssessmentError> {
    ledger.admit_observation_io_attempt(action)?;
    let flags = descriptor_flags_once(file.as_fd())
        .map_err(|source| RetainedBootNamespaceAssessmentError::Filesystem { action, source });
    ledger.complete_observation_io_attempt()?;
    flags
}

pub(super) fn file_stat(
    file: &File,
    ledger: &mut LiveLedger,
    action: &'static str,
) -> Result<NodeStat, RetainedBootNamespaceAssessmentError> {
    ledger.admit_observation_io_attempt(action)?;
    let status =
        fstat_once(file.as_fd()).map_err(|source| RetainedBootNamespaceAssessmentError::Filesystem { action, source });
    ledger.complete_observation_io_attempt()?;
    convert_stat(status?, action)
}

fn convert_stat(
    status: nix::libc::stat,
    action: &'static str,
) -> Result<NodeStat, RetainedBootNamespaceAssessmentError> {
    Ok(NodeStat {
        device: status.st_dev,
        inode: status.st_ino,
        mode: status.st_mode,
        links: status.st_nlink,
        uid: status.st_uid,
        gid: status.st_gid,
        special_device: status.st_rdev,
        size: u64::try_from(status.st_size).map_err(|_| invalid_data(action, "descriptor has negative size"))?,
        block_size: status.st_blksize,
        blocks: status.st_blocks,
        access_seconds: status.st_atime,
        access_nanoseconds: status.st_atime_nsec,
        modify_seconds: status.st_mtime,
        modify_nanoseconds: status.st_mtime_nsec,
        change_seconds: status.st_ctime,
        change_nanoseconds: status.st_ctime_nsec,
    })
}

fn node_kind(mode: u32) -> Option<BootNamespaceNodeKind> {
    match mode & nix::libc::S_IFMT {
        nix::libc::S_IFDIR => Some(BootNamespaceNodeKind::Directory),
        nix::libc::S_IFREG => Some(BootNamespaceNodeKind::Regular),
        nix::libc::S_IFLNK => Some(BootNamespaceNodeKind::Symlink),
        nix::libc::S_IFIFO => Some(BootNamespaceNodeKind::Fifo),
        nix::libc::S_IFSOCK => Some(BootNamespaceNodeKind::Socket),
        nix::libc::S_IFBLK => Some(BootNamespaceNodeKind::BlockDevice),
        nix::libc::S_IFCHR => Some(BootNamespaceNodeKind::CharacterDevice),
        _ => None,
    }
}

struct StackComponent {
    bytes: [u8; 256],
    length: usize,
}

impl StackComponent {
    fn as_c_str(&self) -> &CStr {
        CStr::from_bytes_with_nul(&self.bytes[..=self.length]).expect("validated stack component has one terminal NUL")
    }
}

fn component(raw_name: &[u8], action: &'static str) -> Result<StackComponent, RetainedBootNamespaceAssessmentError> {
    if raw_name.is_empty()
        || raw_name.len() > 255
        || raw_name == b"."
        || raw_name == b".."
        || raw_name.contains(&b'/')
        || raw_name.contains(&0)
    {
        return Err(invalid_data(action, "name is not one canonical raw component"));
    }
    let mut bytes = [0u8; 256];
    bytes[..raw_name.len()].copy_from_slice(raw_name);
    Ok(StackComponent {
        bytes,
        length: raw_name.len(),
    })
}

pub(super) fn invalid_data(action: &'static str, message: &'static str) -> RetainedBootNamespaceAssessmentError {
    RetainedBootNamespaceAssessmentError::Filesystem {
        action,
        source: io::Error::new(io::ErrorKind::InvalidData, message),
    }
}
