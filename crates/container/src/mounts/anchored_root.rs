use std::io;
use std::os::fd::{AsRawFd as _, FromRawFd as _, OwnedFd, RawFd};
use std::os::unix::ffi::OsStrExt as _;
use std::path::{Path, PathBuf};

use fs_err::{self as fs, PathExt as _};
use nc::{AT_EMPTY_PATH, MOVE_MOUNT_F_EMPTY_PATH, OPEN_TREE_CLOEXEC, OPEN_TREE_CLONE, move_mount, open_tree};
use nix::errno::Errno;
use nix::libc::syscall;
use nix::mount::{MntFlags, MsFlags, umount2};
use nix::sys::stat::{Mode, umask};
use nix::unistd::{fchdir, pivot_root, sethostname};
use snafu::ResultExt as _;

use super::pseudo_filesystems::{
    apply_pseudo_mount, apply_root_mount_policy, mount_binds, prepare_anchored_pseudo_mount,
    prepare_anchored_resolver_mount, prepare_pseudo_mount_targets, pseudo_mount_decisions, set_mount_access_fd,
    setup_networking,
};
use super::syscalls::{add_mount, ensure_directory, set_current_dir, setup_localhost};
use super::{Bind, BindSource};
use crate::{
    ActivateAnchoredRootSnafu, Container, ContainerError, FsErrSnafu, LoopbackPolicy, MountSnafu, PivotRootSnafu,
    PseudoFilesystemPolicy, RootFilesystemPolicy, SetHostnameSnafu, UnmountOldRootSnafu, duplicate_cloexec,
};

// linux/mount.h. nc 0.9 exposes only the source-empty-path flag.
const MOVE_MOUNT_T_EMPTY_PATH: u32 = 0x0000_0040;

/// Setup the container
pub(crate) fn setup(
    container: &Container,
    anchored_bind_sources: &[PinnedAnchoredBindSource],
) -> Result<(), ContainerError> {
    if container.networking && container.root_anchor.is_none() {
        setup_networking(&container.root)?;
    }

    if matches!(container.loopback, LoopbackPolicy::HostIpIfAvailable) {
        setup_localhost()?;
    }

    if let Some(anchor) = &container.root_anchor {
        pivot_anchored(
            &container.root,
            anchor.as_raw_fd(),
            anchored_bind_sources,
            container.networking,
            container.pseudo_filesystems,
            container.root_filesystem,
        )?;
    } else {
        pivot(
            &container.root,
            &container.binds,
            container.pseudo_filesystems,
            container.root_filesystem,
        )?;
    }

    if let Some(hostname) = &container.hostname {
        sethostname(hostname).context(SetHostnameSnafu)?;
    }

    if let Some(dir) = &container.work_dir {
        set_current_dir(dir)?;
    }

    Ok(())
}

/// Pivot the process into the rootfs
fn pivot(
    root: &Path,
    binds: &[Bind],
    pseudo_filesystems: PseudoFilesystemPolicy,
    root_filesystem: RootFilesystemPolicy,
) -> Result<(), ContainerError> {
    add_mount(None, "/", None, MsFlags::MS_REC | MsFlags::MS_PRIVATE)?;
    add_mount(Some(root), root, None, MsFlags::MS_BIND)?;

    pivot_mounted_root(root, binds, pseudo_filesystems, root_filesystem)
}

/// Clone and attach the exact mount referenced by `anchor`, then pivot through
/// the retained mount descriptor. `label` is diagnostic-only: even if another
/// process removes or replaces it, both sides of activation remain anchored by
/// descriptor-empty paths.
fn pivot_anchored(
    label: &Path,
    anchor: RawFd,
    bind_sources: &[PinnedAnchoredBindSource],
    networking: bool,
    pseudo_filesystems: PseudoFilesystemPolicy,
    root_filesystem: RootFilesystemPolicy,
) -> Result<(), ContainerError> {
    add_mount(None, "/", None, MsFlags::MS_REC | MsFlags::MS_PRIVATE)?;

    // Prepare every source as a detached mount before entering the root. This
    // does not modify the authenticated tree and ensures later setup never
    // reopens a source pathname.
    let mut prepared_mounts = prepare_anchored_binds(bind_sources)?;
    if networking {
        prepared_mounts.push(prepare_anchored_resolver_mount()?);
    }
    for decision in pseudo_mount_decisions(pseudo_filesystems) {
        prepared_mounts.push(prepare_anchored_pseudo_mount(decision)?);
    }
    validate_anchored_mount_topology(&prepared_mounts)?;

    let root_mount = clone_anchored_root(label, anchor)?;
    attach_anchored_root(label, anchor, &root_mount)?;
    fchdir(root_mount.as_raw_fd()).with_context(|_| ActivateAnchoredRootSnafu {
        label: label.to_owned(),
        operation: "enter attached root mount",
    })?;

    // Pin every target against the untouched cloned root before the first
    // submount is attached. Earlier mounts can therefore never provide a later
    // target, even if a caller accidentally declares overlapping paths.
    let ready_mounts = pin_anchored_mount_targets(root_mount.as_raw_fd(), prepared_mounts)?;

    if matches!(root_filesystem, RootFilesystemPolicy::ReadOnly) {
        set_mount_access_fd(root_mount.as_raw_fd(), true, true).with_context(|_| MountSnafu {
            target: label.to_owned(),
        })?;
    }
    for prepared in &ready_mounts {
        attach_ready_anchored_mount(prepared)?;
    }

    let root = Path::new(".");
    // The same-path pivot idiom stacks the old namespace root over the new
    // descriptor-mounted root without creating a put_old directory in the
    // authenticated backing tree. cwd denotes the old root immediately after
    // pivot_root; detach it before entering the new `/`.
    pivot_root(root, root).context(PivotRootSnafu)?;
    umount2(root, MntFlags::MNT_DETACH).context(UnmountOldRootSnafu)?;
    set_current_dir("/")?;
    umask(Mode::S_IWGRP | Mode::S_IWOTH);
    Ok(())
}

fn clone_anchored_root(label: &Path, anchor: RawFd) -> Result<OwnedFd, ContainerError> {
    // SAFETY: `anchor` is the live duplicate owned by Container and an empty
    // path is explicitly admitted by AT_EMPTY_PATH. Deliberately omitting
    // AT_RECURSIVE clones only the authenticated root mount, so undeclared
    // nested mounts cannot enter the frozen root. A successful call returns a
    // fresh detached mount descriptor.
    let descriptor = unsafe {
        open_tree(
            anchor,
            Path::new(""),
            OPEN_TREE_CLONE | OPEN_TREE_CLOEXEC | AT_EMPTY_PATH as u32,
        )
    }
    .map_err(Errno::from_i32)
    .with_context(|_| ActivateAnchoredRootSnafu {
        label: label.to_owned(),
        operation: "clone descriptor-backed root mount",
    })?;
    // SAFETY: successful open_tree returned a fresh owned descriptor.
    Ok(unsafe { OwnedFd::from_raw_fd(descriptor) })
}

fn attach_anchored_root(label: &Path, anchor: RawFd, root_mount: &OwnedFd) -> Result<(), ContainerError> {
    // SAFETY: root_mount is a live detached mount descriptor, anchor is the
    // duplicated O_PATH directory descriptor, and both remain owned until
    // after pivot_root. The flags explicitly admit both empty paths.
    unsafe {
        move_mount(
            root_mount.as_raw_fd(),
            Path::new(""),
            anchor,
            Path::new(""),
            MOVE_MOUNT_F_EMPTY_PATH | MOVE_MOUNT_T_EMPTY_PATH,
        )
    }
    .map_err(Errno::from_i32)
    .with_context(|_| ActivateAnchoredRootSnafu {
        label: label.to_owned(),
        operation: "attach descriptor-backed root mount",
    })
    .map(|_| ())
}

// Linux PATH_MAX includes the terminating NUL byte.
const MAX_ANCHORED_MOUNT_TARGET_BYTES: usize = 4095;
const MAX_ANCHORED_MOUNT_TARGET_COMPONENTS: usize = 256;
const MAX_ANCHORED_MOUNT_COMPONENT_BYTES: usize = 255;
const MAX_ANCHORED_MOUNTS: usize = 64;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AnchoredMountTargetKind {
    Directory,
    RegularFile,
}

pub(crate) struct PreparedAnchoredMount {
    pub(crate) source_mount: OwnedFd,
    pub(crate) target: PathBuf,
    pub(crate) target_kind: AnchoredMountTargetKind,
}

struct ReadyAnchoredMount {
    source_mount: OwnedFd,
    target: PathBuf,
    target_descriptor: OwnedFd,
}

pub(crate) struct PinnedAnchoredBindSource {
    pub(crate) source: OwnedFd,
    pub(crate) source_label: PathBuf,
    pub(crate) target: PathBuf,
    pub(crate) target_kind: AnchoredMountTargetKind,
    pub(crate) read_only: bool,
}

pub(crate) fn pin_anchored_bind_sources(
    root: RawFd,
    binds: &[Bind],
) -> Result<Vec<PinnedAnchoredBindSource>, ContainerError> {
    if binds.len() > MAX_ANCHORED_MOUNTS {
        return Err(ContainerError::TooManyAnchoredMounts {
            actual: binds.len(),
            limit: MAX_ANCHORED_MOUNTS,
        });
    }
    binds
        .iter()
        .map(|bind| {
            let (source, source_label) = match &bind.source {
                BindSource::Path(path) => {
                    return Err(ContainerError::UnpinnedAnchoredMountSource { path: path.clone() });
                }
                BindSource::RootRelative(path) => {
                    let source = openat2_anchored(
                        root,
                        path,
                        nix::libc::O_PATH | nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW,
                        0,
                        nix::libc::RESOLVE_BENEATH
                            | nix::libc::RESOLVE_NO_XDEV
                            | nix::libc::RESOLVE_NO_MAGICLINKS
                            | nix::libc::RESOLVE_NO_SYMLINKS,
                    )
                    .map_err(|source| ContainerError::OpenAnchoredMountSource {
                        path: path.clone(),
                        source,
                    })?;
                    (source, path.clone())
                }
                BindSource::Pinned { descriptor, label } => {
                    let source = duplicate_cloexec(descriptor.as_raw_fd()).map_err(|source| {
                        ContainerError::OpenAnchoredMountSource {
                            path: label.clone(),
                            source,
                        }
                    })?;
                    (source, label.clone())
                }
            };
            let target_kind = descriptor_target_kind(source.as_raw_fd(), &source_label)?;
            Ok(PinnedAnchoredBindSource {
                source,
                source_label,
                target: normalized_anchored_mount_target(&bind.target)?,
                target_kind,
                read_only: bind.read_only,
            })
        })
        .collect()
}

fn prepare_anchored_binds(
    bind_sources: &[PinnedAnchoredBindSource],
) -> Result<Vec<PreparedAnchoredMount>, ContainerError> {
    bind_sources
        .iter()
        .map(|bind| {
            // Clone exactly the pinned object. In particular, a directory
            // source must not recursively import mounts that appeared below
            // it on the host; pseudo-filesystem trees are the only explicitly
            // recursive anchored imports.
            let flags = OPEN_TREE_CLONE | OPEN_TREE_CLOEXEC | AT_EMPTY_PATH as u32;
            // SAFETY: source is the live O_PATH descriptor pinned before
            // clone(2), the empty path is admitted explicitly, and a
            // successful open_tree returns a fresh detached mount descriptor.
            let descriptor = unsafe { open_tree(bind.source.as_raw_fd(), Path::new(""), flags) }
                .map_err(Errno::from_i32)
                .with_context(|_| MountSnafu {
                    target: bind.source_label.clone(),
                })?;
            // SAFETY: successful open_tree returned a fresh owned descriptor.
            let source_mount = unsafe { OwnedFd::from_raw_fd(descriptor) };
            if bind.read_only {
                set_mount_access_fd(source_mount.as_raw_fd(), true, false).with_context(|_| MountSnafu {
                    target: bind.source_label.clone(),
                })?;
            }
            Ok(PreparedAnchoredMount {
                source_mount,
                target: bind.target.clone(),
                target_kind: bind.target_kind,
            })
        })
        .collect()
}

pub(crate) fn descriptor_target_kind(fd: RawFd, label: &Path) -> Result<AnchoredMountTargetKind, ContainerError> {
    let stat = descriptor_stat(fd).context(FsErrSnafu)?;
    match stat.st_mode & nix::libc::S_IFMT {
        nix::libc::S_IFDIR => Ok(AnchoredMountTargetKind::Directory),
        nix::libc::S_IFREG => Ok(AnchoredMountTargetKind::RegularFile),
        mode => Err(ContainerError::UnsupportedAnchoredMountSource {
            path: label.to_owned(),
            mode,
        }),
    }
}

pub(crate) fn normalized_anchored_mount_target(target: &Path) -> Result<PathBuf, ContainerError> {
    let bytes = target.as_os_str().as_bytes();
    if !target.is_absolute() || bytes.is_empty() || bytes.len() > MAX_ANCHORED_MOUNT_TARGET_BYTES || bytes.contains(&0)
    {
        return Err(ContainerError::InvalidAnchoredMountTarget {
            path: target.to_owned(),
        });
    }
    if bytes
        .split(|byte| *byte == b'/')
        .any(|component| component == b"." || component == b"..")
    {
        return Err(ContainerError::InvalidAnchoredMountTarget {
            path: target.to_owned(),
        });
    }
    let mut normalized = PathBuf::new();
    let mut components = 0usize;
    for component in target.components() {
        match component {
            std::path::Component::RootDir => {}
            std::path::Component::Normal(component) => {
                components = components.saturating_add(1);
                if components > MAX_ANCHORED_MOUNT_TARGET_COMPONENTS
                    || component.as_bytes().len() > MAX_ANCHORED_MOUNT_COMPONENT_BYTES
                {
                    return Err(ContainerError::InvalidAnchoredMountTarget {
                        path: target.to_owned(),
                    });
                }
                normalized.push(component);
            }
            std::path::Component::CurDir | std::path::Component::ParentDir | std::path::Component::Prefix(_) => {
                return Err(ContainerError::InvalidAnchoredMountTarget {
                    path: target.to_owned(),
                });
            }
        }
    }
    if normalized.as_os_str().is_empty() {
        return Err(ContainerError::InvalidAnchoredMountTarget {
            path: target.to_owned(),
        });
    }
    Ok(normalized)
}

pub(crate) fn validate_anchored_mount_topology(mounts: &[PreparedAnchoredMount]) -> Result<(), ContainerError> {
    if mounts.len() > MAX_ANCHORED_MOUNTS {
        return Err(ContainerError::TooManyAnchoredMounts {
            actual: mounts.len(),
            limit: MAX_ANCHORED_MOUNTS,
        });
    }
    for (index, mount) in mounts.iter().enumerate() {
        for other in &mounts[index + 1..] {
            if mount.target == other.target
                || mount.target.starts_with(&other.target)
                || other.target.starts_with(&mount.target)
            {
                return Err(ContainerError::OverlappingAnchoredMountTargets {
                    first: mount.target.clone(),
                    second: other.target.clone(),
                });
            }
        }
    }
    Ok(())
}

fn pin_anchored_mount_targets(
    root: RawFd,
    mounts: Vec<PreparedAnchoredMount>,
) -> Result<Vec<ReadyAnchoredMount>, ContainerError> {
    mounts
        .into_iter()
        .map(|mount| {
            let target_descriptor = open_anchored_mount_target(root, &mount.target, mount.target_kind)?;
            Ok(ReadyAnchoredMount {
                source_mount: mount.source_mount,
                target: mount.target,
                target_descriptor,
            })
        })
        .collect()
}

fn attach_ready_anchored_mount(prepared: &ReadyAnchoredMount) -> Result<(), ContainerError> {
    move_mount_empty(
        prepared.source_mount.as_raw_fd(),
        prepared.target_descriptor.as_raw_fd(),
    )
    .with_context(|_| MountSnafu {
        target: prepared.target.clone(),
    })
}

pub(crate) fn open_anchored_mount_target(
    root: RawFd,
    target: &Path,
    kind: AnchoredMountTargetKind,
) -> Result<OwnedFd, ContainerError> {
    let flags = nix::libc::O_PATH
        | nix::libc::O_CLOEXEC
        | nix::libc::O_NOFOLLOW
        | if matches!(kind, AnchoredMountTargetKind::Directory) {
            nix::libc::O_DIRECTORY
        } else {
            0
        };
    let descriptor = openat2_anchored(
        root,
        target,
        flags,
        0,
        nix::libc::RESOLVE_BENEATH
            | nix::libc::RESOLVE_NO_XDEV
            | nix::libc::RESOLVE_NO_MAGICLINKS
            | nix::libc::RESOLVE_NO_SYMLINKS,
    )
    .map_err(|source| ContainerError::OpenAnchoredMountTarget {
        path: target.to_owned(),
        source,
    })?;
    let stat = descriptor_stat(descriptor.as_raw_fd()).map_err(|source| ContainerError::OpenAnchoredMountTarget {
        path: target.to_owned(),
        source,
    })?;
    let actual = match stat.st_mode & nix::libc::S_IFMT {
        nix::libc::S_IFDIR => AnchoredMountTargetKind::Directory,
        nix::libc::S_IFREG => AnchoredMountTargetKind::RegularFile,
        mode => {
            return Err(ContainerError::UnsafeAnchoredMountTarget {
                path: target.to_owned(),
                mode,
            });
        }
    };
    if actual != kind {
        return Err(ContainerError::AnchoredMountTargetType {
            path: target.to_owned(),
            expected: kind,
            actual,
        });
    }
    Ok(descriptor)
}

pub(crate) fn move_mount_empty(source: RawFd, target: RawFd) -> Result<(), Errno> {
    // SAFETY: both descriptors are live mount/source target references and the
    // explicit flags admit both empty paths.
    unsafe {
        move_mount(
            source,
            Path::new(""),
            target,
            Path::new(""),
            MOVE_MOUNT_F_EMPTY_PATH | MOVE_MOUNT_T_EMPTY_PATH,
        )
    }
    .map_err(Errno::from_i32)
    .map(|_| ())
}

pub(crate) fn descriptor_stat(fd: RawFd) -> io::Result<nix::libc::stat> {
    // SAFETY: zero is valid initialization for stat, fd is live, and the
    // output object remains exclusively borrowed for the call.
    let mut stat: nix::libc::stat = unsafe { std::mem::zeroed() };
    if unsafe { nix::libc::fstat(fd, &mut stat) } == -1 {
        return Err(io::Error::last_os_error());
    }
    Ok(stat)
}

pub(crate) fn openat2_anchored(
    parent: RawFd,
    path: &Path,
    flags: nix::libc::c_int,
    mode: nix::libc::mode_t,
    resolve: u64,
) -> io::Result<OwnedFd> {
    let path = std::ffi::CString::new(path.as_os_str().as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "path contains NUL"))?;
    // SAFETY: zero is valid for every open_how field.
    let mut how: nix::libc::open_how = unsafe { std::mem::zeroed() };
    how.flags = u64::from(flags as u32);
    how.mode = u64::from(mode);
    how.resolve = resolve;
    // SAFETY: every pointer remains live for the call and a successful syscall
    // returns a fresh descriptor.
    let descriptor = unsafe {
        syscall(
            nix::libc::SYS_openat2,
            parent,
            path.as_ptr(),
            &how,
            size_of::<nix::libc::open_how>(),
        )
    };
    if descriptor == -1 {
        return Err(io::Error::last_os_error());
    }
    let descriptor = RawFd::try_from(descriptor)
        .map_err(|_| io::Error::other(format!("openat2 returned invalid descriptor {descriptor}")))?;
    // SAFETY: successful openat2 returned a fresh owned descriptor.
    Ok(unsafe { OwnedFd::from_raw_fd(descriptor) })
}

pub(crate) fn canonical_bind_sources(binds: &[Bind]) -> Result<Vec<PathBuf>, ContainerError> {
    binds
        .iter()
        .map(|bind| match &bind.source {
            BindSource::Path(path) => path.fs_err_canonicalize().context(FsErrSnafu),
            BindSource::RootRelative(path) | BindSource::Pinned { label: path, .. } => {
                Err(ContainerError::AnchoredBindOnPathContainer { path: path.clone() })
            }
        })
        .collect()
}

fn pivot_mounted_root(
    root: &Path,
    binds: &[Bind],
    pseudo_filesystems: PseudoFilesystemPolicy,
    root_filesystem: RootFilesystemPolicy,
) -> Result<(), ContainerError> {
    let sources = canonical_bind_sources(binds)?;
    pivot_mounted_root_with_sources(root, binds, &sources, pseudo_filesystems, root_filesystem)
}

fn pivot_mounted_root_with_sources(
    root: &Path,
    binds: &[Bind],
    sources: &[PathBuf],
    pseudo_filesystems: PseudoFilesystemPolicy,
    root_filesystem: RootFilesystemPolicy,
) -> Result<(), ContainerError> {
    const OLD_PATH: &str = "old_root";

    let old_root = root.join(OLD_PATH);
    let pseudo_mounts = pseudo_mount_decisions(pseudo_filesystems);

    // A read-only root cannot acquire missing mountpoint directories after
    // its recursive mount policy is applied. Prepare every setup-owned target
    // in the backing root first; pseudo-filesystem mounts are attached only
    // after pivot, so payload-visible contents still come exclusively from
    // the selected policy.
    ensure_directory(&old_root)?;
    prepare_pseudo_mount_targets(root, &pseudo_mounts)?;
    mount_binds(root, binds, sources)?;
    apply_root_mount_policy(root, binds, root_filesystem)?;
    pivot_root(root, &old_root).context(PivotRootSnafu)?;

    set_current_dir("/")?;

    for decision in pseudo_mounts {
        apply_pseudo_mount(decision, OLD_PATH)?;
    }

    umount2(OLD_PATH, MntFlags::MNT_DETACH).context(UnmountOldRootSnafu)?;
    if matches!(root_filesystem, RootFilesystemPolicy::ReadWrite) {
        fs::remove_dir(OLD_PATH).context(FsErrSnafu)?;
    }

    umask(Mode::S_IWGRP | Mode::S_IWOTH);

    Ok(())
}
