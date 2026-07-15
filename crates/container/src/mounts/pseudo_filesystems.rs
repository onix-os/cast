use std::io::{self, Read as _, Write as _};
use std::os::fd::{AsRawFd as _, FromRawFd as _, OwnedFd, RawFd};
use std::os::unix::ffi::OsStrExt as _;
use std::path::{Path, PathBuf};

use fs_err::{self as fs};
use nc::syscalls::syscall5;
use nc::{
    AT_EMPTY_PATH, AT_FDCWD, MOUNT_ATTR_RDONLY, MOVE_MOUNT_F_EMPTY_PATH, OPEN_TREE_CLOEXEC, OPEN_TREE_CLONE,
    SYS_MOUNT_SETATTR, mount_attr_t, move_mount, open_tree,
};
use nix::errno::Errno;
use nix::libc::{AT_RECURSIVE, syscall};
use nix::mount::MsFlags;
use snafu::ResultExt as _;

use super::Bind;
use super::anchored_root::{
    AnchoredMountTargetKind, PreparedAnchoredMount, descriptor_stat, move_mount_empty, openat2_anchored,
};
use super::syscalls::{
    add_mount, add_mount_with_data, bind_mount, ensure_directory, ensure_empty_file, errno_to_io, openat_anchored,
};
use crate::{
    ConfigureAnchoredNetworkingSnafu, ContainerError, DevPolicy, FsErrSnafu, MINIMAL_DEV_IDENTITIES, MountSnafu,
    OpenAnchoredMountTargetSnafu, ProcPolicy, PseudoFilesystemPolicy, RootFilesystemPolicy, SysPolicy, TmpPolicy,
    TmpfsLimits,
};

pub(crate) fn prepare_pseudo_mount_targets(
    root: &Path,
    decisions: &[PseudoMountDecision],
) -> Result<(), ContainerError> {
    for decision in decisions {
        let target = match decision {
            PseudoMountDecision::Proc { .. } => "proc",
            PseudoMountDecision::Tmp { .. } => "tmp",
            PseudoMountDecision::HostSys { .. } => "sys",
            PseudoMountDecision::HostDev { .. } | PseudoMountDecision::MinimalDevReadOnly => "dev",
        };
        ensure_directory(root.join(target))?;
    }
    Ok(())
}

pub(crate) fn mount_binds(root: &Path, binds: &[Bind], sources: &[PathBuf]) -> Result<(), ContainerError> {
    for (bind, source) in binds.iter().zip(sources) {
        let target = root.join(bind.target.strip_prefix("/").unwrap_or(&bind.target));
        bind_mount(source, &target, bind.read_only)?;
    }
    Ok(())
}

pub(crate) fn apply_root_mount_policy(
    root: &Path,
    binds: &[Bind],
    root_filesystem: RootFilesystemPolicy,
) -> Result<(), ContainerError> {
    for decision in root_mount_decisions(root, binds, root_filesystem) {
        match decision {
            RootMountDecision::ReadOnlyRecursive(target) => set_mount_access(&target, true, true)?,
            RootMountDecision::ReadWriteExact(target) => set_mount_access(&target, false, false)?,
        }
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RootMountDecision {
    ReadOnlyRecursive(PathBuf),
    ReadWriteExact(PathBuf),
}

pub(crate) fn root_mount_decisions(
    root: &Path,
    binds: &[Bind],
    policy: RootFilesystemPolicy,
) -> Vec<RootMountDecision> {
    if matches!(policy, RootFilesystemPolicy::ReadWrite) {
        return Vec::new();
    }

    std::iter::once(RootMountDecision::ReadOnlyRecursive(root.to_owned()))
        .chain(binds.iter().filter(|bind| !bind.read_only).map(|bind| {
            RootMountDecision::ReadWriteExact(root.join(bind.target.strip_prefix("/").unwrap_or(&bind.target)))
        }))
        .collect()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PseudoMountDecision {
    Proc { read_only: bool },
    Tmp { limits: Option<TmpfsLimits> },
    HostSys { read_only: bool },
    HostDev { read_only: bool },
    MinimalDevReadOnly,
}

pub(crate) fn pseudo_mount_decisions(policy: PseudoFilesystemPolicy) -> Vec<PseudoMountDecision> {
    let mut decisions = Vec::with_capacity(4);
    match policy.proc {
        ProcPolicy::None => {}
        ProcPolicy::ReadOnly => decisions.push(PseudoMountDecision::Proc { read_only: true }),
        ProcPolicy::ReadWrite => decisions.push(PseudoMountDecision::Proc { read_only: false }),
    }
    match policy.tmp {
        TmpPolicy::Disabled => {}
        TmpPolicy::Empty => decisions.push(PseudoMountDecision::Tmp { limits: None }),
        TmpPolicy::Bounded(limits) => decisions.push(PseudoMountDecision::Tmp { limits: Some(limits) }),
    }
    match policy.sys {
        SysPolicy::None => {}
        SysPolicy::HostReadOnly => decisions.push(PseudoMountDecision::HostSys { read_only: true }),
        SysPolicy::HostReadWrite => decisions.push(PseudoMountDecision::HostSys { read_only: false }),
    }
    match policy.dev {
        DevPolicy::None => {}
        DevPolicy::HostReadOnly => decisions.push(PseudoMountDecision::HostDev { read_only: true }),
        DevPolicy::HostReadWrite => decisions.push(PseudoMountDecision::HostDev { read_only: false }),
        DevPolicy::Minimal => decisions.push(PseudoMountDecision::MinimalDevReadOnly),
    }
    decisions
}

pub(crate) fn apply_pseudo_mount(decision: PseudoMountDecision, old_path: &str) -> Result<(), ContainerError> {
    match decision {
        PseudoMountDecision::Proc { read_only } => add_mount(
            Some(Path::new("proc")),
            Path::new("proc"),
            Some("proc"),
            if read_only {
                MsFlags::MS_RDONLY
            } else {
                MsFlags::empty()
            },
        ),
        PseudoMountDecision::Tmp { limits } => {
            let options = limits.map(TmpfsLimits::mount_options);
            add_mount_with_data(
                Some(Path::new("tmpfs")),
                Path::new("tmp"),
                Some("tmpfs"),
                MsFlags::empty(),
                options.as_deref(),
            )?;
            if let Some(limits) = limits {
                let target = Path::new("tmp");
                let descriptor = openat_anchored(
                    AT_FDCWD,
                    c"tmp",
                    nix::libc::O_PATH | nix::libc::O_DIRECTORY | nix::libc::O_NOFOLLOW | nix::libc::O_CLOEXEC,
                    0,
                )
                .map_err(|source| ContainerError::InspectTmpfs {
                    target: target.to_owned(),
                    source,
                })?;
                verify_tmpfs_limits(descriptor.as_raw_fd(), target, limits)?;
            }
            Ok(())
        }
        PseudoMountDecision::HostSys { read_only } => mount_host_tree(old_path, "sys", read_only),
        PseudoMountDecision::HostDev { read_only } => mount_host_tree(old_path, "dev", read_only),
        PseudoMountDecision::MinimalDevReadOnly => mount_minimal_dev(old_path),
    }
}

/// Prepare a pseudo-filesystem as a detached mount. Target descriptors are
/// opened later, as one batch against the untouched authenticated root.
pub(crate) fn prepare_anchored_pseudo_mount(
    decision: PseudoMountDecision,
) -> Result<PreparedAnchoredMount, ContainerError> {
    let (source_mount, target) = match decision {
        PseudoMountDecision::Proc { read_only } => {
            let source = detached_filesystem_mount(c"proc", read_only, Path::new("proc"), &[])?;
            (source, PathBuf::from("proc"))
        }
        PseudoMountDecision::Tmp { limits } => {
            let source = detached_tmpfs_mount(limits, Path::new("tmp"))?;
            (source, PathBuf::from("tmp"))
        }
        PseudoMountDecision::HostSys { read_only } => {
            let source = detached_host_mount(Path::new("/sys"), read_only)?;
            (source, PathBuf::from("sys"))
        }
        PseudoMountDecision::HostDev { read_only } => {
            let source = detached_host_mount(Path::new("/dev"), read_only)?;
            (source, PathBuf::from("dev"))
        }
        PseudoMountDecision::MinimalDevReadOnly => (prepare_anchored_minimal_dev()?, PathBuf::from("dev")),
    };
    Ok(PreparedAnchoredMount {
        source_mount,
        target,
        target_kind: AnchoredMountTargetKind::Directory,
    })
}

fn detached_filesystem_mount(
    filesystem: &std::ffi::CStr,
    read_only: bool,
    label: &Path,
    parameters: &[(&std::ffi::CStr, &std::ffi::CStr)],
) -> Result<OwnedFd, ContainerError> {
    const FSOPEN_CLOEXEC: nix::libc::c_uint = 0x0000_0001;
    const FSCONFIG_SET_STRING: nix::libc::c_uint = 1;
    const FSCONFIG_CMD_CREATE: nix::libc::c_uint = 6;
    const FSMOUNT_CLOEXEC: nix::libc::c_uint = 0x0000_0001;

    // SAFETY: filesystem is NUL terminated and successful fsopen returns a
    // fresh context descriptor.
    let context = unsafe { syscall(nix::libc::SYS_fsopen, filesystem.as_ptr(), FSOPEN_CLOEXEC) };
    if context == -1 {
        return Err(Errno::last()).context(MountSnafu {
            target: label.to_owned(),
        });
    }
    let context =
        RawFd::try_from(context).map_err(|_| ContainerError::InvalidMountDescriptor { operation: "fsopen" })?;
    // SAFETY: successful fsopen returned a fresh owned descriptor.
    let context = unsafe { OwnedFd::from_raw_fd(context) };

    for &(key, value) in parameters {
        // SAFETY: key and value are both NUL-terminated strings and the live
        // filesystem context borrows them only for this call.
        let configured = unsafe {
            syscall(
                nix::libc::SYS_fsconfig,
                context.as_raw_fd(),
                FSCONFIG_SET_STRING,
                key.as_ptr(),
                value.as_ptr(),
                0,
            )
        };
        if configured == -1 {
            return Err(Errno::last()).context(MountSnafu {
                target: label.to_owned(),
            });
        }
    }

    // SAFETY: CREATE accepts null key/value and borrows only the live context.
    let configured = unsafe {
        syscall(
            nix::libc::SYS_fsconfig,
            context.as_raw_fd(),
            FSCONFIG_CMD_CREATE,
            std::ptr::null::<nix::libc::c_char>(),
            std::ptr::null::<nix::libc::c_void>(),
            0,
        )
    };
    if configured == -1 {
        return Err(Errno::last()).context(MountSnafu {
            target: label.to_owned(),
        });
    }

    // SAFETY: the configured context is live and successful fsmount returns a
    // fresh detached mount descriptor.
    let mount = unsafe { syscall(nix::libc::SYS_fsmount, context.as_raw_fd(), FSMOUNT_CLOEXEC, 0) };
    if mount == -1 {
        return Err(Errno::last()).context(MountSnafu {
            target: label.to_owned(),
        });
    }
    let mount = RawFd::try_from(mount).map_err(|_| ContainerError::InvalidMountDescriptor { operation: "fsmount" })?;
    // SAFETY: successful fsmount returned a fresh owned descriptor.
    let mount = unsafe { OwnedFd::from_raw_fd(mount) };
    if read_only {
        set_mount_access_fd(mount.as_raw_fd(), true, true).with_context(|_| MountSnafu {
            target: label.to_owned(),
        })?;
    }
    Ok(mount)
}

fn detached_tmpfs_mount(limits: Option<TmpfsLimits>, label: &Path) -> Result<OwnedFd, ContainerError> {
    let mount = if let Some(limits) = limits {
        let options = limits.fsconfig_options();
        let parameters = [
            (options[0].0, options[0].1.as_c_str()),
            (options[1].0, options[1].1.as_c_str()),
        ];
        detached_filesystem_mount(c"tmpfs", false, label, &parameters)?
    } else {
        detached_filesystem_mount(c"tmpfs", false, label, &[])?
    };
    if let Some(limits) = limits {
        verify_tmpfs_limits(mount.as_raw_fd(), label, limits)?;
    }
    Ok(mount)
}

pub(crate) const TMPFS_MAGIC: nix::libc::c_long = 0x0102_1994;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct TmpfsLimitReadback {
    pub(crate) filesystem: nix::libc::c_long,
    pub(crate) block_size: nix::libc::c_long,
    pub(crate) blocks: u64,
    pub(crate) inodes: u64,
}

pub(crate) fn verify_tmpfs_limits(fd: RawFd, label: &Path, expected: TmpfsLimits) -> Result<(), ContainerError> {
    // SAFETY: zero is valid initialization for statfs, fd is live, and the
    // output remains exclusively borrowed for the syscall.
    let mut observed: nix::libc::statfs = unsafe { std::mem::zeroed() };
    if unsafe { nix::libc::fstatfs(fd, &mut observed) } == -1 {
        return Err(Errno::last()).context(MountSnafu {
            target: label.to_owned(),
        });
    }
    validate_tmpfs_limit_readback(
        label,
        expected,
        TmpfsLimitReadback {
            filesystem: observed.f_type,
            block_size: observed.f_bsize,
            blocks: observed.f_blocks,
            inodes: observed.f_files,
        },
    )
}

pub(crate) fn validate_tmpfs_limit_readback(
    label: &Path,
    expected: TmpfsLimits,
    observed: TmpfsLimitReadback,
) -> Result<(), ContainerError> {
    if observed.filesystem != TMPFS_MAGIC {
        return Err(ContainerError::UnexpectedTmpfsFilesystem {
            target: label.to_owned(),
            filesystem: observed.filesystem,
        });
    }
    let block_size = u64::try_from(observed.block_size).map_err(|_| ContainerError::InvalidTmpfsLimitReadback {
        target: label.to_owned(),
    })?;
    let size_bytes =
        block_size
            .checked_mul(observed.blocks)
            .ok_or_else(|| ContainerError::InvalidTmpfsLimitReadback {
                target: label.to_owned(),
            })?;
    let inodes = observed.inodes;
    if size_bytes != expected.size_bytes() || inodes != expected.inodes() {
        return Err(ContainerError::TmpfsLimitsNormalized {
            target: label.to_owned(),
            expected_size_bytes: expected.size_bytes(),
            observed_size_bytes: size_bytes,
            expected_inodes: expected.inodes(),
            observed_inodes: inodes,
        });
    }
    Ok(())
}

fn detached_host_mount(source: &Path, read_only: bool) -> Result<OwnedFd, ContainerError> {
    // SAFETY: source remains live for the call and successful open_tree returns
    // a fresh detached recursive mount descriptor.
    let mount = unsafe {
        open_tree(
            AT_FDCWD,
            source,
            OPEN_TREE_CLONE | OPEN_TREE_CLOEXEC | AT_RECURSIVE as u32,
        )
    }
    .map_err(Errno::from_i32)
    .with_context(|_| MountSnafu {
        target: source.to_owned(),
    })?;
    // SAFETY: successful open_tree returned a fresh owned descriptor.
    let mount = unsafe { OwnedFd::from_raw_fd(mount) };
    if read_only {
        set_mount_access_fd(mount.as_raw_fd(), true, true).with_context(|_| MountSnafu {
            target: source.to_owned(),
        })?;
    }
    Ok(mount)
}

fn prepare_anchored_minimal_dev() -> Result<OwnedFd, ContainerError> {
    let dev_mount = detached_filesystem_mount(c"tmpfs", false, Path::new("dev"), &[])?;
    for &(device, expected_major, expected_minor) in MINIMAL_DEV_IDENTITIES {
        let name = std::ffi::CString::new(device).expect("fixed device names contain no NUL");
        let placeholder = openat_anchored(
            dev_mount.as_raw_fd(),
            &name,
            nix::libc::O_WRONLY | nix::libc::O_CREAT | nix::libc::O_EXCL | nix::libc::O_NOFOLLOW | nix::libc::O_CLOEXEC,
            0o600,
        )
        .map_err(|source| ContainerError::OpenAnchoredMountTarget {
            path: PathBuf::from("dev").join(device),
            source,
        })?;
        // SAFETY: placeholder is a live regular file descriptor.
        if unsafe { nix::libc::fchmod(placeholder.as_raw_fd(), 0o600) } == -1 {
            return Err(Errno::last()).context(MountSnafu {
                target: PathBuf::from("dev").join(device),
            });
        }

        let host_device = Path::new("/dev").join(device);
        // SAFETY: host_device remains live and successful open_tree returns a
        // fresh detached bind mount descriptor without opening device data.
        let device_mount = unsafe { open_tree(AT_FDCWD, &host_device, OPEN_TREE_CLONE | OPEN_TREE_CLOEXEC) }
            .map_err(Errno::from_i32)
            .with_context(|_| MountSnafu {
                target: host_device.clone(),
            })?;
        // SAFETY: successful open_tree returned a fresh owned descriptor.
        let device_mount = unsafe { OwnedFd::from_raw_fd(device_mount) };
        validate_minimal_device_source(device_mount.as_raw_fd(), &host_device, expected_major, expected_minor)?;
        set_mount_access_fd(device_mount.as_raw_fd(), true, false).with_context(|_| MountSnafu {
            target: PathBuf::from("dev").join(device),
        })?;
        let target = openat2_anchored(
            dev_mount.as_raw_fd(),
            Path::new(device),
            nix::libc::O_PATH | nix::libc::O_NOFOLLOW | nix::libc::O_CLOEXEC,
            0,
            nix::libc::RESOLVE_BENEATH
                | nix::libc::RESOLVE_NO_XDEV
                | nix::libc::RESOLVE_NO_MAGICLINKS
                | nix::libc::RESOLVE_NO_SYMLINKS,
        )
        .with_context(|_| OpenAnchoredMountTargetSnafu {
            path: PathBuf::from("dev").join(device),
        })?;
        move_mount_empty(device_mount.as_raw_fd(), target.as_raw_fd()).with_context(|_| MountSnafu {
            target: PathBuf::from("dev").join(device),
        })?;
    }
    // The tmpfs is setup scratch, not payload scratch. Seal the complete tree
    // only after all three authenticated device mounts have been attached, so
    // the payload cannot add, remove, or rename entries beneath `/dev`.
    set_mount_access_fd(dev_mount.as_raw_fd(), true, true).with_context(|_| MountSnafu {
        target: PathBuf::from("dev"),
    })?;
    Ok(dev_mount)
}

pub(crate) fn validate_minimal_device_source(
    fd: RawFd,
    label: &Path,
    expected_major: u64,
    expected_minor: u64,
) -> Result<(), ContainerError> {
    let stat = descriptor_stat(fd).context(FsErrSnafu)?;
    let mode = stat.st_mode & nix::libc::S_IFMT;
    if mode != nix::libc::S_IFCHR {
        return Err(ContainerError::UnsupportedAnchoredMountSource {
            path: label.to_owned(),
            mode,
        });
    }
    let actual_major = nix::libc::major(stat.st_rdev) as u64;
    let actual_minor = nix::libc::minor(stat.st_rdev) as u64;
    if (actual_major, actual_minor) != (expected_major, expected_minor) {
        return Err(ContainerError::UnexpectedMinimalDeviceIdentity {
            path: label.to_owned(),
            expected_major,
            expected_minor,
            actual_major,
            actual_minor,
        });
    }
    Ok(())
}

fn mount_host_tree(old_path: &str, name: &str, read_only: bool) -> Result<(), ContainerError> {
    let source = Path::new("/").join(old_path).join(name);
    let target = Path::new(name);
    add_mount(
        Some(source.as_path()),
        target,
        None,
        MsFlags::MS_BIND | MsFlags::MS_REC | MsFlags::MS_SLAVE,
    )?;
    if read_only {
        set_mount_access(target, true, true)?;
    }
    Ok(())
}

fn mount_minimal_dev(old_path: &str) -> Result<(), ContainerError> {
    add_mount(
        Some(Path::new("tmpfs")),
        Path::new("dev"),
        Some("tmpfs"),
        MsFlags::empty(),
    )?;
    for &(device, expected_major, expected_minor) in MINIMAL_DEV_IDENTITIES {
        bind_minimal_device(old_path, device, expected_major, expected_minor)?;
    }
    set_mount_access(Path::new("dev"), true, true)?;
    Ok(())
}

fn bind_minimal_device(
    old_path: &str,
    device: &str,
    expected_major: u64,
    expected_minor: u64,
) -> Result<(), ContainerError> {
    let source = Path::new("/").join(old_path).join("dev").join(device);
    let target = Path::new("dev").join(device);
    let source_name =
        std::ffi::CString::new(source.as_os_str().as_bytes()).expect("constructed device path has no NUL");
    let source_descriptor = openat_anchored(
        AT_FDCWD,
        &source_name,
        nix::libc::O_PATH | nix::libc::O_NOFOLLOW | nix::libc::O_CLOEXEC,
        0,
    )
    .map_err(|source_error| ContainerError::OpenAnchoredMountSource {
        path: source.clone(),
        source: source_error,
    })?;
    validate_minimal_device_source(source_descriptor.as_raw_fd(), &source, expected_major, expected_minor)?;

    ensure_empty_file(&target)?;
    // Clone from the identity-validated descriptor, not the pathname, so a
    // concurrent host replacement cannot change the device that is attached.
    // SAFETY: source_descriptor remains live, AT_EMPTY_PATH admits the empty
    // path, and success returns a fresh detached mount descriptor.
    let device_mount = unsafe {
        open_tree(
            source_descriptor.as_raw_fd(),
            Path::new(""),
            OPEN_TREE_CLONE | OPEN_TREE_CLOEXEC | AT_EMPTY_PATH as u32,
        )
    }
    .map_err(Errno::from_i32)
    .with_context(|_| MountSnafu { target: source.clone() })?;
    // SAFETY: successful open_tree returned a fresh owned descriptor.
    let device_mount = unsafe { OwnedFd::from_raw_fd(device_mount) };
    set_mount_access_fd(device_mount.as_raw_fd(), true, false).with_context(|_| MountSnafu { target: source })?;
    // SAFETY: device_mount is a live detached mount descriptor and target is
    // a controlled placeholder in the fresh minimal-dev tmpfs.
    unsafe {
        move_mount(
            device_mount.as_raw_fd(),
            Path::new(""),
            AT_FDCWD,
            &target,
            MOVE_MOUNT_F_EMPTY_PATH,
        )
    }
    .map_err(Errno::from_i32)
    .with_context(|_| MountSnafu { target })?;
    Ok(())
}

pub(crate) fn set_mount_access(target: &Path, read_only: bool, recursive: bool) -> Result<(), ContainerError> {
    // SAFETY: target remains live for the call and successful open_tree
    // returns a fresh descriptor.
    let fd = unsafe { open_tree(AT_FDCWD, target, OPEN_TREE_CLOEXEC) }
        .map_err(Errno::from_i32)
        .with_context(|_| MountSnafu {
            target: target.to_owned(),
        })?;
    // SAFETY: successful open_tree returned a fresh owned descriptor.
    let fd = unsafe { OwnedFd::from_raw_fd(fd) };
    set_mount_access_fd(fd.as_raw_fd(), read_only, recursive).with_context(|_| MountSnafu {
        target: target.to_owned(),
    })
}

pub(crate) fn set_mount_access_fd(fd: RawFd, read_only: bool, recursive: bool) -> Result<(), Errno> {
    let attr = mount_attr_t {
        attr_set: if read_only { MOUNT_ATTR_RDONLY as u64 } else { 0 },
        attr_clr: if read_only { 0 } else { MOUNT_ATTR_RDONLY as u64 },
        program: 0,
        userns_fd: 0,
    };
    let flags = AT_EMPTY_PATH as usize | if recursive { AT_RECURSIVE as usize } else { 0 };
    // SAFETY: fd is live, empty path is admitted by AT_EMPTY_PATH, and attr is
    // initialized and borrowed only for the syscall.
    unsafe {
        syscall5(
            SYS_MOUNT_SETATTR,
            fd as usize,
            c"".as_ptr() as usize,
            flags,
            &attr as *const mount_attr_t as usize,
            size_of::<mount_attr_t>(),
        )
    }
    .map_err(Errno::from_i32)
    .map(|_| ())
}

pub(crate) fn setup_networking(root: &Path) -> Result<(), ContainerError> {
    ensure_directory(root.join("etc"))?;
    fs::copy("/etc/resolv.conf", root.join("etc/resolv.conf")).context(FsErrSnafu)?;
    Ok(())
}

/// Prepare resolver configuration without consulting the mutable root label.
/// Bounded, stable resolver bytes are copied into a sealed memfd and exposed as
/// a read-only detached file mount. Its target is pinned later together with
/// every other mount target, before the cloned root is modified.
pub(crate) fn prepare_anchored_resolver_mount() -> Result<PreparedAnchoredMount, ContainerError> {
    let resolver = read_host_resolver_bounded()?;
    Ok(PreparedAnchoredMount {
        source_mount: detached_resolver_mount(&resolver)?,
        target: PathBuf::from("etc/resolv.conf"),
        target_kind: AnchoredMountTargetKind::RegularFile,
    })
}

const MAX_RESOLVER_BYTES: usize = 64 * 1024;
const RESOLVER_MODE: nix::libc::mode_t = 0o644;

#[cfg(test)]
pub(crate) fn open_anchored_resolver_target(anchor: RawFd) -> Result<OwnedFd, ContainerError> {
    let path = Path::new("etc/resolv.conf");
    let target = openat2_anchored(
        anchor,
        path,
        nix::libc::O_PATH | nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW,
        0,
        nix::libc::RESOLVE_BENEATH
            | nix::libc::RESOLVE_NO_XDEV
            | nix::libc::RESOLVE_NO_MAGICLINKS
            | nix::libc::RESOLVE_NO_SYMLINKS,
    )
    .map_err(|source| ContainerError::OpenAnchoredMountTarget {
        path: path.to_owned(),
        source,
    })?;
    validate_resolver_target(target.as_raw_fd(), path)?;
    Ok(target)
}

#[cfg(test)]
pub(crate) fn validate_resolver_target(fd: RawFd, path: &Path) -> Result<(), ContainerError> {
    let stat = descriptor_stat(fd).map_err(|source| ContainerError::OpenAnchoredMountTarget {
        path: path.to_owned(),
        source,
    })?;
    let mode = stat.st_mode & nix::libc::S_IFMT;
    if mode != nix::libc::S_IFREG {
        return Err(ContainerError::UnsafeResolverTarget {
            path: path.to_owned(),
            mode,
            links: stat.st_nlink as u64,
        });
    }
    Ok(())
}

const MAX_RESOLVER_STABILITY_ATTEMPTS: usize = 3;

fn read_host_resolver_bounded() -> Result<Vec<u8>, ContainerError> {
    for _ in 0..MAX_RESOLVER_STABILITY_ATTEMPTS {
        match read_host_resolver_bounded_once() {
            Err(ContainerError::ResolverSourceChanged) => {}
            result => return result,
        }
    }
    Err(ContainerError::ResolverSourceChanged)
}

fn read_host_resolver_bounded_once() -> Result<Vec<u8>, ContainerError> {
    // O_PATH pins the object without opening FIFO/device data. Only after its
    // structure is proven to be a regular file do we reopen this exact
    // descriptor through procfs with O_NONBLOCK for bounded reading.
    let pinned = openat_anchored(
        AT_FDCWD,
        c"/etc/resolv.conf",
        nix::libc::O_PATH | nix::libc::O_CLOEXEC,
        0,
    )
    .context(ConfigureAnchoredNetworkingSnafu {
        operation: "pin host resolver source",
    })?;
    let pinned_stat = validate_resolver_source(pinned.as_raw_fd())?;
    let mut reader = reopen_pinned_readonly(pinned.as_raw_fd()).context(ConfigureAnchoredNetworkingSnafu {
        operation: "open pinned host resolver source for bounded reading",
    })?;
    let reader_stat = validate_resolver_source(reader.as_raw_fd())?;
    if reader_stat.st_dev != pinned_stat.st_dev || reader_stat.st_ino != pinned_stat.st_ino {
        return Err(ContainerError::ResolverSourceChanged);
    }

    let mut resolver = Vec::with_capacity((pinned_stat.st_size as usize).min(MAX_RESOLVER_BYTES));
    (&mut reader)
        .take((MAX_RESOLVER_BYTES + 1) as u64)
        .read_to_end(&mut resolver)
        .context(ConfigureAnchoredNetworkingSnafu {
            operation: "read bounded host resolver source",
        })?;
    if resolver.len() > MAX_RESOLVER_BYTES {
        return Err(ContainerError::ResolverSourceTooLarge {
            actual: resolver.len() as u64,
            limit: MAX_RESOLVER_BYTES as u64,
        });
    }
    let final_reader = validate_resolver_source(reader.as_raw_fd())?;
    let final_pinned = validate_resolver_source(pinned.as_raw_fd())?;
    if !resolver_stat_stable(&pinned_stat, &reader_stat)
        || !resolver_stat_stable(&reader_stat, &final_reader)
        || !resolver_stat_stable(&final_reader, &final_pinned)
    {
        return Err(ContainerError::ResolverSourceChanged);
    }
    Ok(resolver)
}

pub(crate) fn resolver_stat_stable(first: &nix::libc::stat, second: &nix::libc::stat) -> bool {
    first.st_dev == second.st_dev
        && first.st_ino == second.st_ino
        && first.st_size == second.st_size
        && first.st_mtime == second.st_mtime
        && first.st_mtime_nsec == second.st_mtime_nsec
        && first.st_ctime == second.st_ctime
        && first.st_ctime_nsec == second.st_ctime_nsec
}

fn validate_resolver_source(fd: RawFd) -> Result<nix::libc::stat, ContainerError> {
    let stat = descriptor_stat(fd).context(ConfigureAnchoredNetworkingSnafu {
        operation: "inspect host resolver source",
    })?;
    let mode = stat.st_mode & nix::libc::S_IFMT;
    let size = u64::try_from(stat.st_size).unwrap_or(u64::MAX);
    if mode != nix::libc::S_IFREG {
        return Err(ContainerError::UnsafeResolverSource {
            mode,
            links: stat.st_nlink as u64,
        });
    }
    if size > MAX_RESOLVER_BYTES as u64 {
        return Err(ContainerError::ResolverSourceTooLarge {
            actual: size,
            limit: MAX_RESOLVER_BYTES as u64,
        });
    }
    Ok(stat)
}

pub(crate) fn reopen_pinned_readonly(fd: RawFd) -> io::Result<fs::File> {
    let diagnostic_path = PathBuf::from(format!("/proc/self/fd/{fd}"));
    let path = std::ffi::CString::new(diagnostic_path.as_os_str().as_bytes())
        .expect("decimal descriptor path cannot contain NUL");
    let descriptor = openat_anchored(
        AT_FDCWD,
        &path,
        nix::libc::O_RDONLY | nix::libc::O_NONBLOCK | nix::libc::O_CLOEXEC,
        0,
    )?;
    Ok(fs::File::from_parts(descriptor.into(), diagnostic_path))
}

fn detached_resolver_mount(resolver: &[u8]) -> Result<OwnedFd, ContainerError> {
    let file = sealed_resolver_file(resolver)?;

    // SAFETY: file is live, AT_EMPTY_PATH admits the empty path, and success
    // returns a fresh detached file-mount descriptor.
    let mount = unsafe {
        open_tree(
            file.as_raw_fd(),
            Path::new(""),
            OPEN_TREE_CLONE | OPEN_TREE_CLOEXEC | AT_EMPTY_PATH as u32,
        )
    }
    .map_err(Errno::from_i32)
    .map_err(errno_to_io)
    .context(ConfigureAnchoredNetworkingSnafu {
        operation: "clone sealed resolver mount",
    })?;
    // SAFETY: successful open_tree returned a fresh owned descriptor.
    let mount = unsafe { OwnedFd::from_raw_fd(mount) };
    set_mount_access_fd(mount.as_raw_fd(), true, false)
        .map_err(errno_to_io)
        .context(ConfigureAnchoredNetworkingSnafu {
            operation: "make sealed resolver mount read-only",
        })?;
    Ok(mount)
}

pub(crate) fn sealed_resolver_file(resolver: &[u8]) -> Result<fs::File, ContainerError> {
    if resolver.len() > MAX_RESOLVER_BYTES {
        return Err(ContainerError::ResolverSourceTooLarge {
            actual: resolver.len() as u64,
            limit: MAX_RESOLVER_BYTES as u64,
        });
    }
    // SAFETY: the name is static and NUL terminated; success returns a fresh
    // descriptor transferred exactly once to OwnedFd.
    let descriptor = unsafe {
        nix::libc::memfd_create(
            c"container-resolv.conf".as_ptr(),
            nix::libc::MFD_ALLOW_SEALING | nix::libc::MFD_CLOEXEC,
        )
    };
    if descriptor == -1 {
        return Err(io::Error::last_os_error()).context(ConfigureAnchoredNetworkingSnafu {
            operation: "create sealed resolver file",
        });
    }
    // SAFETY: memfd_create returned a fresh owned descriptor.
    let descriptor = unsafe { OwnedFd::from_raw_fd(descriptor) };
    let mut file = fs::File::from_parts(descriptor.into(), "sealed resolv.conf");
    file.write_all(resolver).context(ConfigureAnchoredNetworkingSnafu {
        operation: "write sealed resolver file",
    })?;
    // SAFETY: file is a live memfd and mode contains only permission bits.
    if unsafe { nix::libc::fchmod(file.as_raw_fd(), RESOLVER_MODE) } == -1 {
        return Err(io::Error::last_os_error()).context(ConfigureAnchoredNetworkingSnafu {
            operation: "set deterministic resolver mode",
        });
    }
    file.sync_all().context(ConfigureAnchoredNetworkingSnafu {
        operation: "sync sealed resolver file",
    })?;
    let required_seals =
        nix::libc::F_SEAL_WRITE | nix::libc::F_SEAL_GROW | nix::libc::F_SEAL_SHRINK | nix::libc::F_SEAL_SEAL;
    // SAFETY: file is a live sealable memfd and the variadic argument is the
    // documented seal bitmask.
    if unsafe { nix::libc::fcntl(file.as_raw_fd(), nix::libc::F_ADD_SEALS, required_seals) } == -1 {
        return Err(io::Error::last_os_error()).context(ConfigureAnchoredNetworkingSnafu {
            operation: "seal resolver file",
        });
    }
    // SAFETY: file remains live and F_GET_SEALS has no variadic argument.
    let actual_seals = unsafe { nix::libc::fcntl(file.as_raw_fd(), nix::libc::F_GET_SEALS) };
    if actual_seals == -1 {
        return Err(io::Error::last_os_error()).context(ConfigureAnchoredNetworkingSnafu {
            operation: "verify resolver file seals",
        });
    }
    let stat = descriptor_stat(file.as_raw_fd()).context(ConfigureAnchoredNetworkingSnafu {
        operation: "verify sealed resolver file metadata",
    })?;
    let kind = stat.st_mode & nix::libc::S_IFMT;
    let mode = stat.st_mode & 0o777;
    let size = u64::try_from(stat.st_size).unwrap_or(u64::MAX);
    if kind != nix::libc::S_IFREG
        || mode != RESOLVER_MODE
        || size != resolver.len() as u64
        || actual_seals & required_seals != required_seals
    {
        return Err(ContainerError::InvalidSealedResolver {
            kind,
            mode,
            size,
            expected_size: resolver.len() as u64,
            seals: actual_seals,
        });
    }
    Ok(file)
}
