use std::io;
use std::os::fd::{AsFd as _, AsRawFd as _, FromRawFd as _, OwnedFd, RawFd};
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

use super::anchored_identity::open_current_namespace_root;
#[cfg(test)]
use super::pseudo_filesystems::detached_owned_nested_proc_fixture;
use super::pseudo_filesystems::{
    PseudoMountDecision, apply_pseudo_mount, apply_root_mount_policy, mount_binds,
    prepare_anchored_pseudo_mount, prepare_anchored_resolver_mount, prepare_pseudo_mount_targets,
    prepare_private_dev, pseudo_mount_decisions, set_mount_access_fd, setup_networking,
};
use super::syscalls::{add_mount, ensure_directory, set_current_dir, setup_localhost};
use super::{Bind, BindSource};
#[cfg(test)]
use crate::OwnedNestedProcFixture;
use crate::private_device_assembly::PreparedPrivateDev;
use crate::private_devices::PrivateDeviceMounts;
use crate::{
    ActivateAnchoredRootSnafu, CloneAnchoredBindSourceSnafu, Container, ContainerError, DevPolicy, FsErrSnafu,
    LoopbackPolicy, MountSnafu, PivotRootSnafu, PseudoFilesystemPolicy, RootFilesystemPolicy, SetHostnameSnafu,
    UnmountOldRootSnafu,
};

// linux/mount.h. nc 0.9 exposes only the source-empty-path flag.
const MOVE_MOUNT_T_EMPTY_PATH: u32 = 0x0000_0040;

/// Setup the container
pub(crate) fn setup(
    container: &Container,
    private_devices: Option<PrivateDeviceMounts>,
) -> Result<(), ContainerError> {
    let private_devices = require_private_device_policy(container.pseudo_filesystems.dev, private_devices)?;

    if container.networking && container.root_locator.is_none() {
        setup_networking(&container.root)?;
    }

    if matches!(container.loopback, LoopbackPolicy::HostIpIfAvailable) {
        setup_localhost()?;
    }

    if container.root_locator.is_some() {
        pivot_anchored(container, private_devices)?;
    } else {
        pivot(
            &container.root,
            &container.binds,
            container.pseudo_filesystems,
            container.root_filesystem,
            private_devices,
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
    private_devices: Option<PrivateDeviceMounts>,
) -> Result<(), ContainerError> {
    add_mount(None, "/", None, MsFlags::MS_REC | MsFlags::MS_PRIVATE)?;
    add_mount(Some(root), root, None, MsFlags::MS_BIND)?;

    pivot_mounted_root(root, binds, pseudo_filesystems, root_filesystem, private_devices)
}

/// Reopen every anchored input inside the private child mount namespace, then
/// clone and attach only those child-local descriptors.
fn pivot_anchored(
    container: &Container,
    mut private_devices: Option<PrivateDeviceMounts>,
) -> Result<(), ContainerError> {
    add_mount(None, "/", None, MsFlags::MS_REC | MsFlags::MS_PRIVATE)?;

    // Inherited O_PATH descriptors still carry paths from the supervisor's
    // mount namespace. Open the child namespace root only after propagation is
    // private, then authenticate every locator against it before any mount is
    // cloned or attached.
    let namespace_root = open_current_namespace_root().map_err(|source| ContainerError::ReopenAnchoredRoot {
        path: container.root.clone(),
        source,
    })?;
    let rebound = rebind_anchored_inputs(container, namespace_root.as_raw_fd())?;

    #[cfg(test)]
    attach_owned_nested_proc_fixture(container, &rebound)?;

    // Prepare every source as a detached mount before entering the root. Only
    // child-local rebound descriptors are admitted to open_tree.
    let mut prepared_mounts = prepare_anchored_binds(&rebound.bind_sources)?;
    if container.networking {
        prepared_mounts.push(prepare_anchored_resolver_mount()?);
    }
    for decision in pseudo_mount_decisions(container.pseudo_filesystems) {
        let devices = if matches!(decision, PseudoMountDecision::PrivateMinimalDev) {
            private_devices.take()
        } else {
            None
        };
        prepared_mounts.push(prepare_anchored_pseudo_mount(decision, devices)?);
    }
    require_capability_consumed(private_devices)?;
    validate_anchored_mount_topology(&prepared_mounts)?;

    let root_mount = clone_anchored_root(&container.root, rebound.root.as_raw_fd())?;
    attach_anchored_root(&container.root, rebound.root.as_raw_fd(), &root_mount)?;
    fchdir(root_mount.as_raw_fd()).with_context(|_| ActivateAnchoredRootSnafu {
        label: container.root.clone(),
        operation: "enter attached root mount",
    })?;

    // Pin every target against the untouched cloned root before the first
    // submount is attached. Earlier mounts can therefore never provide a later
    // target, even if a caller accidentally declares overlapping paths.
    let ready_mounts = pin_anchored_mount_targets(root_mount.as_raw_fd(), prepared_mounts)?;

    if matches!(container.root_filesystem, RootFilesystemPolicy::ReadOnly) {
        set_mount_access_fd(root_mount.as_raw_fd(), true, true).with_context(|_| MountSnafu {
            target: container.root.clone(),
        })?;
    }
    for prepared in ready_mounts {
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

#[cfg(test)]
fn attach_owned_nested_proc_fixture(
    container: &Container,
    rebound: &ReboundAnchoredInputs,
) -> Result<(), ContainerError> {
    let Some(fixture) = container.owned_nested_proc_fixture else {
        return Ok(());
    };
    let source = match fixture {
        OwnedNestedProcFixture::Root => rebound.root.as_raw_fd(),
        OwnedNestedProcFixture::FirstAnchoredBind => rebound
            .bind_sources
            .first()
            .expect("owned nested proc bind fixture requires one anchored bind")
            .source
            .as_raw_fd(),
    };
    let target = open_anchored_mount_target(source, Path::new("nested"), AnchoredMountTargetKind::Directory)?;
    let proc_mount = detached_owned_nested_proc_fixture()?;
    move_mount_empty(proc_mount.as_raw_fd(), target.as_raw_fd()).with_context(|_| MountSnafu {
        target: PathBuf::from("nested"),
    })
}

fn clone_anchored_root(label: &Path, anchor: RawFd) -> Result<OwnedFd, ContainerError> {
    // SAFETY: `anchor` is the child-local descriptor reopened from the fresh
    // private-namespace root and an empty path is explicitly admitted by
    // AT_EMPTY_PATH. Deliberately omitting AT_RECURSIVE clones only the
    // authenticated root mount, so undeclared nested mounts cannot enter the
    // frozen root. A successful call returns a fresh detached mount descriptor.
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
    // child-local authenticated O_PATH directory descriptor, and both remain
    // owned until after pivot_root. The flags explicitly admit both empty
    // paths.
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
    source: PreparedAnchoredMountSource,
    pub(crate) target: PathBuf,
    pub(crate) target_kind: AnchoredMountTargetKind,
}

enum PreparedAnchoredMountSource {
    Detached(OwnedFd),
    PrivateDev(PreparedPrivateDev),
}

impl PreparedAnchoredMount {
    pub(crate) fn detached(
        source_mount: OwnedFd,
        target: PathBuf,
        target_kind: AnchoredMountTargetKind,
    ) -> Self {
        Self {
            source: PreparedAnchoredMountSource::Detached(source_mount),
            target,
            target_kind,
        }
    }

    pub(crate) fn private_dev(source: PreparedPrivateDev, target: PathBuf) -> Self {
        Self {
            source: PreparedAnchoredMountSource::PrivateDev(source),
            target,
            target_kind: AnchoredMountTargetKind::Directory,
        }
    }
}

struct ReadyAnchoredMount {
    source: PreparedAnchoredMountSource,
    target: PathBuf,
    target_descriptor: OwnedFd,
}

pub(crate) struct ReboundAnchoredInputs {
    pub(crate) root: OwnedFd,
    pub(crate) bind_sources: Vec<ReboundAnchoredBindSource>,
}

pub(crate) struct ReboundAnchoredBindSource {
    pub(crate) source: OwnedFd,
    pub(crate) source_label: PathBuf,
    pub(crate) target: PathBuf,
    pub(crate) target_kind: AnchoredMountTargetKind,
    pub(crate) read_only: bool,
}

pub(crate) fn validate_anchored_bind_inputs(binds: &[Bind]) -> Result<(), ContainerError> {
    if binds.len() > MAX_ANCHORED_MOUNTS {
        return Err(ContainerError::TooManyAnchoredMounts {
            actual: binds.len(),
            limit: MAX_ANCHORED_MOUNTS,
        });
    }
    for bind in binds {
        if let BindSource::Path(path) = &bind.source {
            return Err(ContainerError::UnpinnedAnchoredMountSource { path: path.clone() });
        }
        normalized_anchored_mount_target(&bind.target)?;
    }
    Ok(())
}

/// Authenticate every anchored input in the supervisor namespace.
///
/// The returned descriptors exist only so pre-clone policy can inspect them;
/// activation drops them before clone and the child reopens every locator
/// again from its own fresh namespace-root descriptor.
pub(crate) fn authenticate_anchored_inputs(
    container: &Container,
) -> Result<Option<ReboundAnchoredInputs>, ContainerError> {
    if container.root_locator.is_none() {
        return Ok(None);
    }
    validate_anchored_bind_inputs(&container.binds)?;
    let namespace_root = open_current_namespace_root().map_err(|source| ContainerError::ReopenAnchoredRoot {
        path: container.root.clone(),
        source,
    })?;
    rebind_anchored_inputs(container, namespace_root.as_raw_fd()).map(Some)
}

fn rebind_anchored_inputs(
    container: &Container,
    namespace_root: RawFd,
) -> Result<ReboundAnchoredInputs, ContainerError> {
    let root_locator = container
        .root_locator
        .as_ref()
        .expect("anchored rebind requires a root locator");
    let root = root_locator
        .reopen_from_namespace_root(namespace_root)
        .map_err(|source| ContainerError::ReopenAnchoredRoot {
            path: container.root.clone(),
            source,
        })?;
    let root_stat = descriptor_stat(root.as_raw_fd()).context(FsErrSnafu)?;
    let root_file_type = root_stat.st_mode & nix::libc::S_IFMT;
    if root_file_type != nix::libc::S_IFDIR {
        return Err(ContainerError::AnchoredRootNotDirectory {
            path: container.root.clone(),
            file_type: root_file_type,
        });
    }

    let bind_sources = container
        .binds
        .iter()
        .map(|bind| {
            let (source, source_label) = match &bind.source {
                BindSource::Path(path) => {
                    return Err(ContainerError::UnpinnedAnchoredMountSource { path: path.clone() });
                }
                BindSource::Anchored(locator) => {
                    let source_label = locator.resolved_absolute_path();
                    let source = locator.reopen_from_namespace_root(namespace_root).map_err(|source| {
                        ContainerError::ReopenAnchoredBindSource {
                            path: source_label.clone(),
                            source,
                        }
                    })?;
                    (source, source_label)
                }
            };
            let target_kind = descriptor_target_kind(source.as_raw_fd(), &source_label)?;
            Ok(ReboundAnchoredBindSource {
                source,
                source_label,
                target: normalized_anchored_mount_target(&bind.target)?,
                target_kind,
                read_only: bind.read_only,
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(ReboundAnchoredInputs { root, bind_sources })
}

fn prepare_anchored_binds(
    bind_sources: &[ReboundAnchoredBindSource],
) -> Result<Vec<PreparedAnchoredMount>, ContainerError> {
    bind_sources
        .iter()
        .map(|bind| {
            // Clone exactly the pinned object. In particular, a directory
            // source must not recursively import mounts that appeared below
            // it on the host; pseudo-filesystem trees are the only explicitly
            // recursive anchored imports.
            let flags = OPEN_TREE_CLONE | OPEN_TREE_CLOEXEC | AT_EMPTY_PATH as u32;
            // SAFETY: source is a live O_PATH descriptor reopened in this
            // child mount namespace, the empty path is admitted explicitly,
            // and successful open_tree returns a detached mount descriptor.
            let descriptor = unsafe { open_tree(bind.source.as_raw_fd(), Path::new(""), flags) }
                .map_err(Errno::from_i32)
                .with_context(|_| CloneAnchoredBindSourceSnafu {
                    path: bind.source_label.clone(),
                })?;
            // SAFETY: successful open_tree returned a fresh owned descriptor.
            let source_mount = unsafe { OwnedFd::from_raw_fd(descriptor) };
            if bind.read_only {
                set_mount_access_fd(source_mount.as_raw_fd(), true, false).with_context(|_| MountSnafu {
                    target: bind.source_label.clone(),
                })?;
            }
            Ok(PreparedAnchoredMount::detached(
                source_mount,
                bind.target.clone(),
                bind.target_kind,
            ))
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
                source: mount.source,
                target: mount.target,
                target_descriptor,
            })
        })
        .collect()
}

fn attach_ready_anchored_mount(prepared: ReadyAnchoredMount) -> Result<(), ContainerError> {
    match prepared.source {
        PreparedAnchoredMountSource::Detached(source_mount) => move_mount_empty(
            source_mount.as_raw_fd(),
            prepared.target_descriptor.as_raw_fd(),
        )
        .with_context(|_| MountSnafu {
            target: prepared.target,
        }),
        PreparedAnchoredMountSource::PrivateDev(private_dev) => private_dev
            .attach_to_authenticated_target(prepared.target_descriptor.as_fd())
            .map_err(|source| ContainerError::FsErr {
                source: io::Error::other(source),
            }),
    }
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
            BindSource::Anchored(locator) => Err(ContainerError::AnchoredBindOnPathContainer {
                path: locator.resolved_absolute_path(),
            }),
        })
        .collect()
}

fn pivot_mounted_root(
    root: &Path,
    binds: &[Bind],
    pseudo_filesystems: PseudoFilesystemPolicy,
    root_filesystem: RootFilesystemPolicy,
    private_devices: Option<PrivateDeviceMounts>,
) -> Result<(), ContainerError> {
    let sources = canonical_bind_sources(binds)?;
    pivot_mounted_root_with_sources(
        root,
        binds,
        &sources,
        pseudo_filesystems,
        root_filesystem,
        private_devices,
    )
}

fn pivot_mounted_root_with_sources(
    root: &Path,
    binds: &[Bind],
    sources: &[PathBuf],
    pseudo_filesystems: PseudoFilesystemPolicy,
    root_filesystem: RootFilesystemPolicy,
    private_devices: Option<PrivateDeviceMounts>,
) -> Result<(), ContainerError> {
    const OLD_PATH: &str = "old_root";

    let old_root = root.join(OLD_PATH);
    let pseudo_mounts = pseudo_mount_decisions(pseudo_filesystems);
    // This child already made mount propagation private in `pivot`. Prepare
    // the bounded parent before entering the root, but populate it only after
    // attachment to the authenticated final /dev target. Any partial result
    // remains disposable trusted-child state and the payload never runs.
    let mut private_dev_mount = private_devices.map(prepare_private_dev).transpose()?;

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
        let dev_mount = if matches!(decision, PseudoMountDecision::PrivateMinimalDev) {
            private_dev_mount.take()
        } else {
            None
        };
        apply_pseudo_mount(decision, OLD_PATH, dev_mount)?;
    }
    require_prepared_mount_consumed(private_dev_mount)?;

    umount2(OLD_PATH, MntFlags::MNT_DETACH).context(UnmountOldRootSnafu)?;
    if matches!(root_filesystem, RootFilesystemPolicy::ReadWrite) {
        fs::remove_dir(OLD_PATH).context(FsErrSnafu)?;
    }

    umask(Mode::S_IWGRP | Mode::S_IWOTH);

    Ok(())
}

fn require_private_device_policy(
    policy: DevPolicy,
    private_devices: Option<PrivateDeviceMounts>,
) -> Result<Option<PrivateDeviceMounts>, ContainerError> {
    match (policy, private_devices) {
        (DevPolicy::Minimal, Some(devices)) => Ok(Some(devices)),
        (DevPolicy::Minimal, None) => Err(private_device_invariant(
            "minimal /dev requires one validated private-device capability",
        )),
        (_, None) => Ok(None),
        (_, Some(_)) => Err(private_device_invariant(
            "private-device capability supplied while minimal /dev is disabled",
        )),
    }
}

fn require_capability_consumed(private_devices: Option<PrivateDeviceMounts>) -> Result<(), ContainerError> {
    if private_devices.is_none() {
        Ok(())
    } else {
        Err(private_device_invariant(
            "private-device capability remained after anchored pseudo-mount preparation",
        ))
    }
}

fn require_prepared_mount_consumed(private_dev_mount: Option<PreparedPrivateDev>) -> Result<(), ContainerError> {
    if private_dev_mount.is_none() {
        Ok(())
    } else {
        Err(private_device_invariant(
            "private minimal /dev mount remained after pathname pseudo-mount attachment",
        ))
    }
}

fn private_device_invariant(message: &'static str) -> ContainerError {
    ContainerError::FsErr {
        source: io::Error::new(io::ErrorKind::InvalidInput, message),
    }
}
