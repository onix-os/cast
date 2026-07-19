use std::os::fd::RawFd;

use nix::errno::Errno;
pub(super) use nix::libc::{PR_CAP_AMBIENT, PR_CAP_AMBIENT_IS_SET, PR_CAPBSET_READ, prctl};
use nix::libc::{PR_CAP_AMBIENT_CLEAR_ALL, PR_CAPBSET_DROP, SYS_capget, SYS_capset, syscall};
use nix::sys::prctl::set_pdeathsig;
use nix::sys::signal::Signal;
use nix::unistd::{close, read};
use snafu::ResultExt;

use super::activation::{descriptor_filesystem_magic, is_cgroup_filesystem};
use super::mounts::setup;
use super::process_runtime::BlockedSignalMask;
use super::{
    Container, ContainerError, DropPayloadCapabilitiesSnafu, InspectPayloadStandardDescriptorSnafu,
    InstallPayloadSeccompSnafu, InvalidContinueMsgSnafu, InvalidPayloadErrorDescriptorSnafu, Message,
    PayloadRetainsCapabilitySnafu, PayloadRetainsRealtimeSchedulingSnafu, ReadContinueMsgSnafu,
    RestoreCloneSignalMaskSnafu, RestrictPayloadSchedulerSnafu, RunSnafu, SanitizePayloadDescriptorsSnafu,
    SetPDeathSigSnafu, UnsafeCgroupStandardDescriptorSnafu, UnsafePayloadStandardDescriptorSnafu, credentials, seccomp,
};

/// Reenter the container
pub(super) fn enter<E>(
    container: &mut Container,
    sync: RawFd,
    mut signal_mask: Option<BlockedSignalMask>,
    mut f: impl FnMut() -> Result<(), E>,
) -> Result<(), ContainerError>
where
    E: std::error::Error + Send + Sync + 'static,
{
    // Ensure process is cleaned up if parent dies
    set_pdeathsig(Signal::SIGKILL).context(SetPDeathSigSnafu)?;

    // Wait for continue message
    let mut message = [0u8; 1];
    let len = read(sync, &mut message).context(ReadContinueMsgSnafu)?;
    if len != 1 || message[0] != Message::Continue as u8 {
        return InvalidContinueMsgSnafu.fail();
    }

    // The parent deliberately leaves setgroups enabled until this point.  A
    // rootless user namespace otherwise freezes the caller's ambient
    // supplementary groups into every build.  Drop them before any mount,
    // process, or package-analysis work can observe them, then prove the
    // namespace-visible identity is the fixed root credential contract.
    isolate_payload_credentials()?;

    setup(container)?;

    // Locators retain setup-only witness descriptors. Clearing the child's
    // copy after pivot leaves the supervising parent's copy-on-write value
    // intact and prevents payload code from inspecting host objects.
    drop(container.root_locator.take());
    container.binds.clear();

    // Descriptor and privilege confinement are container invariants, not
    // optional consequences of selecting a descriptor-backed or read-only
    // root. A pathname container retaining a host directory descriptor can
    // otherwise escape its pivoted root with openat(2), while a writable-root
    // payload can use namespace-root capabilities to recreate setup-only
    // authority such as the auxiliary GID or arbitrary device nodes.
    validate_payload_standard_fds()?;
    sanitize_payload_fds(sync)?;
    restrict_payload_scheduler()?;
    drop_all_payload_capabilities()?;
    seccomp::install_payload_filter().context(InstallPayloadSeccompSnafu)?;

    // Both fork-like activation paths block every catchable signal before
    // their exact task audit. Keep that mask through trusted setup, then
    // restore it only inside the child boundary immediately before arbitrary
    // payload code.
    if let Some(signal_mask) = signal_mask.as_mut() {
        signal_mask.restore().context(RestoreCloneSignalMaskSnafu)?;
    }

    let result = f().boxed().context(RunSnafu);
    if result.is_ok() {
        // Errors retain the write end so the outer clone callback can report
        // them. A successful Rust payload has nothing left to report.
        let _ = close(sync);
    }
    result
}

fn sanitize_payload_fds(error_writer: RawFd) -> Result<(), ContainerError> {
    if error_writer < 3 {
        return InvalidPayloadErrorDescriptorSnafu { fd: error_writer }.fail();
    }
    close_range(3, error_writer as u32 - 1)?;
    close_range(error_writer as u32 + 1, u32::MAX)
}

fn validate_payload_standard_fds() -> Result<(), ContainerError> {
    for fd in 0..=2 {
        // A closed standard descriptor carries no authority and is allowed.
        // Any live descriptor is inspected before the rest of the inherited
        // table is closed. Directory and O_PATH descriptors are pathname
        // capabilities rather than ordinary byte streams and must never be
        // handed to untrusted payload code through stdin/stdout/stderr.
        let mut stat: nix::libc::stat = unsafe { std::mem::zeroed() };
        if unsafe { nix::libc::fstat(fd, &mut stat) } == -1 {
            let source = Errno::last();
            if source == Errno::EBADF {
                continue;
            }
            return Err(source).context(InspectPayloadStandardDescriptorSnafu { fd });
        }
        let status_flags = unsafe { nix::libc::fcntl(fd, nix::libc::F_GETFL) };
        if status_flags == -1 {
            return Err(Errno::last()).context(InspectPayloadStandardDescriptorSnafu { fd });
        }
        let filesystem = descriptor_filesystem_magic(fd).context(InspectPayloadStandardDescriptorSnafu { fd })?;
        let kind = stat.st_mode & nix::libc::S_IFMT;
        if standard_descriptor_is_unsafe(kind, status_flags) {
            return UnsafePayloadStandardDescriptorSnafu { fd, kind, status_flags }.fail();
        }
        if is_cgroup_filesystem(filesystem) && status_flags & nix::libc::O_ACCMODE != nix::libc::O_RDONLY {
            return UnsafeCgroupStandardDescriptorSnafu {
                fd,
                filesystem,
                status_flags,
            }
            .fail();
        }
    }
    Ok(())
}

pub(super) fn standard_descriptor_is_unsafe(kind: nix::libc::mode_t, status_flags: nix::libc::c_int) -> bool {
    kind == nix::libc::S_IFDIR || status_flags & nix::libc::O_PATH == nix::libc::O_PATH
}

fn close_range(first: u32, last: u32) -> Result<(), ContainerError> {
    if first > last {
        return Ok(());
    }
    // SAFETY: close_range takes scalar arguments only. This child has a private
    // descriptor table because CLONE_FILES is not requested.
    let result = unsafe { syscall(nix::libc::SYS_close_range, first, last, 0_u32) };
    if result == -1 {
        return Err(Errno::last()).context(SanitizePayloadDescriptorsSnafu);
    }
    Ok(())
}

fn isolate_payload_credentials() -> Result<(), ContainerError> {
    credentials::isolate_payload_credentials().map_err(|failure| match failure {
        credentials::IsolationFailure::ClearSupplementaryGroups(source) => {
            ContainerError::ClearSupplementaryGroups { source }
        }
        credentials::IsolationFailure::NormalizeGroupCredentials(source) => {
            ContainerError::NormalizePayloadGroupCredentials { source }
        }
        credentials::IsolationFailure::NormalizeUserCredentials(source) => {
            ContainerError::NormalizePayloadUserCredentials { source }
        }
        credentials::IsolationFailure::ReadSupplementaryGroups(source) => {
            ContainerError::ReadSupplementaryGroups { source }
        }
        credentials::IsolationFailure::ReadGroupCredentials(source) => {
            ContainerError::ReadPayloadGroupCredentials { source }
        }
        credentials::IsolationFailure::ReadUserCredentials(source) => {
            ContainerError::ReadPayloadUserCredentials { source }
        }
        credentials::IsolationFailure::UnexpectedCredentials(credentials) => {
            ContainerError::UnexpectedPayloadCredentials { credentials }
        }
    })
}

const LINUX_CAPABILITY_VERSION_3: u32 = 0x2008_0522;
// Version 3 exposes two u32 words. Linux capability numbers are contiguous;
// PR_CAPBSET_READ reports EINVAL at the first unsupported number.
pub(super) const MAX_LINUX_CAPABILITY_NUMBER: u32 = 63;

#[repr(C)]
struct CapabilityHeader {
    version: u32,
    pid: i32,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[repr(C)]
pub(super) struct CapabilityData {
    pub(super) effective: u32,
    pub(super) permitted: u32,
    pub(super) inheritable: u32,
}

/// Remove every Linux capability from the untrusted payload.
///
/// A denylist is not a stable confinement boundary: namespace UID zero can use
/// capabilities such as CAP_SETGID, CAP_CHOWN, CAP_MKNOD,
/// CAP_DAC_READ_SEARCH, or CAP_SYS_CHROOT to recover setup-only authority or
/// bypass pathname policy. Clearing only the live sets is also insufficient,
/// because a later execve can regain capabilities from the bounding set. Drop
/// every supported bounding entry first, clear the ambient and live sets, then
/// verify all three sources of authority.
fn drop_all_payload_capabilities() -> Result<(), ContainerError> {
    let capabilities = supported_capability_numbers().context(DropPayloadCapabilitiesSnafu)?;
    unsafe {
        for &capability in &capabilities {
            let present = checked_prctl_value(prctl(PR_CAPBSET_READ, capability, 0, 0, 0))
                .context(DropPayloadCapabilitiesSnafu)?;
            if present != 0 {
                checked_prctl(prctl(PR_CAPBSET_DROP, capability, 0, 0, 0)).context(DropPayloadCapabilitiesSnafu)?;
            }
        }
        checked_prctl(prctl(PR_CAP_AMBIENT, PR_CAP_AMBIENT_CLEAR_ALL, 0, 0, 0))
            .context(DropPayloadCapabilitiesSnafu)?;
    }

    write_capabilities(&[CapabilityData::default(); 2]).context(DropPayloadCapabilitiesSnafu)?;

    let live = read_capabilities().context(DropPayloadCapabilitiesSnafu)?;
    for capability in capabilities {
        let retained_live = capability_is_set(&live, capability);
        let retained_bounding = unsafe {
            checked_prctl_value(prctl(PR_CAPBSET_READ, capability, 0, 0, 0)).context(DropPayloadCapabilitiesSnafu)? != 0
        };
        let retained_ambient = unsafe {
            checked_prctl_value(prctl(PR_CAP_AMBIENT, PR_CAP_AMBIENT_IS_SET, capability, 0, 0))
                .context(DropPayloadCapabilitiesSnafu)?
                != 0
        };
        if retained_live || retained_bounding || retained_ambient {
            return PayloadRetainsCapabilitySnafu { capability }.fail();
        }
    }
    Ok(())
}

pub(super) fn supported_capability_numbers() -> Result<Vec<u32>, Errno> {
    let mut capabilities = Vec::new();
    let mut reached_kernel_end = false;
    for capability in 0..=MAX_LINUX_CAPABILITY_NUMBER {
        let result = unsafe { prctl(PR_CAPBSET_READ, capability, 0, 0, 0) };
        if result == -1 {
            let source = Errno::last();
            if source == Errno::EINVAL {
                reached_kernel_end = true;
                break;
            }
            return Err(source);
        }
        capabilities.push(capability);
    }
    if capabilities.is_empty() {
        return Err(Errno::EINVAL);
    }
    if !reached_kernel_end {
        // Capability ABI v3 has exactly 64 live-set bits. If a future kernel
        // exposes capability 64, silently clearing only the old range would
        // leave an unknown privilege recoverable after execve.
        let unsupported = MAX_LINUX_CAPABILITY_NUMBER + 1;
        let result = unsafe { prctl(PR_CAPBSET_READ, unsupported, 0, 0, 0) };
        if result != -1 {
            return Err(Errno::EOVERFLOW);
        }
        let source = Errno::last();
        if source != Errno::EINVAL {
            return Err(source);
        }
    }
    Ok(capabilities)
}

/// Make `cpu.max` an enforceable aggregate ceiling for the eventual cgroup
/// boundary. The controller throttles fair-class work; therefore the payload
/// must neither inherit a real-time/deadline policy nor retain a route back to
/// one. All capabilities, including CAP_SYS_NICE, are removed separately by
/// [`drop_all_payload_capabilities`].
fn restrict_payload_scheduler() -> Result<(), ContainerError> {
    let zero = nix::libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    if unsafe { nix::libc::setrlimit(nix::libc::RLIMIT_RTPRIO, &zero) } == -1 {
        return Err(Errno::last()).context(RestrictPayloadSchedulerSnafu);
    }

    let parameter = nix::libc::sched_param { sched_priority: 0 };
    if unsafe { nix::libc::sched_setscheduler(0, nix::libc::SCHED_OTHER, &parameter) } == -1 {
        return Err(Errno::last()).context(RestrictPayloadSchedulerSnafu);
    }

    let policy = unsafe { nix::libc::sched_getscheduler(0) };
    if policy == -1 {
        return Err(Errno::last()).context(RestrictPayloadSchedulerSnafu);
    }
    let mut limit = nix::libc::rlimit {
        rlim_cur: nix::libc::RLIM_INFINITY,
        rlim_max: nix::libc::RLIM_INFINITY,
    };
    if unsafe { nix::libc::getrlimit(nix::libc::RLIMIT_RTPRIO, &mut limit) } == -1 {
        return Err(Errno::last()).context(RestrictPayloadSchedulerSnafu);
    }
    if policy != nix::libc::SCHED_OTHER || limit.rlim_cur != 0 || limit.rlim_max != 0 {
        return PayloadRetainsRealtimeSchedulingSnafu {
            policy,
            soft_limit: limit.rlim_cur,
            hard_limit: limit.rlim_max,
        }
        .fail();
    }
    Ok(())
}

pub(super) fn read_capabilities() -> Result<[CapabilityData; 2], Errno> {
    let mut header = CapabilityHeader {
        version: LINUX_CAPABILITY_VERSION_3,
        pid: 0,
    };
    let mut data = [CapabilityData::default(); 2];
    let result = unsafe { syscall(SYS_capget, &mut header, data.as_mut_ptr()) };
    checked_syscall(result)?;
    Ok(data)
}

fn write_capabilities(data: &[CapabilityData; 2]) -> Result<(), Errno> {
    let mut header = CapabilityHeader {
        version: LINUX_CAPABILITY_VERSION_3,
        pid: 0,
    };
    let result = unsafe { syscall(SYS_capset, &mut header, data.as_ptr()) };
    checked_syscall(result)
}

pub(super) fn capability_is_set(data: &[CapabilityData; 2], capability: u32) -> bool {
    let word = capability as usize / u32::BITS as usize;
    let mask = 1_u32 << (capability % u32::BITS);
    data[word].effective & mask != 0 || data[word].permitted & mask != 0 || data[word].inheritable & mask != 0
}

fn checked_syscall(result: nix::libc::c_long) -> Result<(), Errno> {
    if result == -1 { Err(Errno::last()) } else { Ok(()) }
}

fn checked_prctl(result: nix::libc::c_int) -> Result<(), Errno> {
    checked_prctl_value(result).map(|_| ())
}

pub(super) fn checked_prctl_value(result: nix::libc::c_int) -> Result<nix::libc::c_int, Errno> {
    if result == -1 { Err(Errno::last()) } else { Ok(result) }
}
