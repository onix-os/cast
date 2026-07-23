//! Descriptor-bound evidence for a directory on the selected devtmpfs mount.
//!
//! This layer accepts one already-retained directory descriptor and closed
//! mountinfo policy. It validates the policy against the caller's canonical
//! `st_dev` and expected mount ID before observing the descriptor, then uses
//! this exact fail-closed schedule under one caller-owned absolute deadline:
//! opening `fstat`, opening descriptor mount ID, opening `fstatfs`, closing
//! `fstatfs`, closing descriptor mount ID, and closing `fstat`.
//!
//! Linux reports devtmpfs with `TMPFS_MAGIC`, but ordinary tmpfs reports that
//! same magic. The magic alone therefore never proves devtmpfs. The evidence
//! here is meaningful only because it agrees with a separately authenticated
//! exact `devtmpfs` mountinfo policy. Even that composition proves only that
//! this directory is on the selected mount with the expected scalar identity;
//! it does not prove that the descriptor names the exact `/dev` root. A
//! whole-filesystem bind can also retain root `/` and type `devtmpfs` in
//! mountinfo, so whole-root bind provenance remains unprovable here.
//!
//! The result is closed scalar evidence. It owns no descriptor, accepts no
//! caller-authored path, and opens no storage path or device node. The fixed,
//! authenticated current-thread procfs `fd`/`fdinfo` resolution needed for
//! mount-ID observation is the only internal name resolution. This layer grants no
//! storage reopen, device-read, mutation, publication, or durability authority.

use std::{io, mem::zeroed, os::fd::AsRawFd as _, time::Instant};

use thiserror::Error;

use super::{
    descriptor_mount_id_until,
    mountinfo_devtmpfs_policy::{DevtmpfsAccessMode, DevtmpfsFilesystemKind, ValidatedDevtmpfsMountInfoPolicy},
    retry_interrupted,
};

const TMPFS_MAGIC: nix::libc::c_long = 0x0102_1994;
const REQUIRED_OBSERVATIONS: usize = 6;
const PRODUCTION_MAX_WORK: usize = 64;

#[derive(Clone, Copy, Debug)]
struct AuthenticationLimits {
    max_observations: usize,
    max_work: usize,
}

const PRODUCTION_LIMITS: AuthenticationLimits = AuthenticationLimits {
    max_observations: REQUIRED_OBSERVATIONS,
    max_work: PRODUCTION_MAX_WORK,
};

/// Closed interpretation of the Linux filesystem magic admitted here.
///
/// This deliberately names the shared kernel magic family, not devtmpfs.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) enum DevtmpfsDescriptorMagicFamily {
    LinuxTmpfs,
}

/// Scalar-only evidence for one expected directory on the selected mount.
///
/// This value proves neither that the directory is the exact `/dev` mount
/// root nor that the attachment was never installed through a whole-root bind.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) struct ValidatedDevtmpfsSameMountDescriptorEvidence {
    directory_device: u64,
    directory_inode: u64,
    mount_id: u64,
    filesystem: DevtmpfsFilesystemKind,
    access_mode: DevtmpfsAccessMode,
    magic_family: DevtmpfsDescriptorMagicFamily,
}

impl ValidatedDevtmpfsSameMountDescriptorEvidence {
    pub(crate) const fn directory_device(self) -> u64 {
        self.directory_device
    }

    pub(crate) const fn directory_inode(self) -> u64 {
        self.directory_inode
    }

    pub(crate) const fn mount_id(self) -> u64 {
        self.mount_id
    }

    pub(crate) const fn filesystem(self) -> DevtmpfsFilesystemKind {
        self.filesystem
    }

    pub(crate) const fn access_mode(self) -> DevtmpfsAccessMode {
        self.access_mode
    }

    pub(crate) const fn magic_family(self) -> DevtmpfsDescriptorMagicFamily {
        self.magic_family
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum DevtmpfsDescriptorObservationPhase {
    OpeningDirectoryIdentity,
    OpeningDescriptorMountId,
    OpeningFilesystemMagic,
    ClosingFilesystemMagic,
    ClosingDescriptorMountId,
    ClosingDirectoryIdentity,
}

#[derive(Debug, Error)]
pub(crate) enum DevtmpfsDescriptorAuthenticationError {
    #[error("devtmpfs descriptor-authentication limits are zero or exceed production ceilings")]
    InvalidLimits,
    #[error("expected directory has zero st_dev {device}, st_ino {inode}, or mount ID {mount_id}")]
    InvalidExpectedIdentity { device: u64, inode: u64, mount_id: u64 },
    #[error("expected directory st_dev {device} is not a canonical Linux major/minor encoding")]
    InvalidExpectedDeviceEncoding { device: u64 },
    #[error("devtmpfs policy mount ID {policy_mount_id} does not equal expected mount ID {expected_mount_id}")]
    PolicyMountIdMismatch {
        expected_mount_id: u64,
        policy_mount_id: u64,
    },
    #[error(
        "devtmpfs policy device {policy_major}:{policy_minor} does not equal expected st_dev device {expected_major}:{expected_minor}"
    )]
    PolicyDeviceMismatch {
        expected_major: u32,
        expected_minor: u32,
        policy_major: u32,
        policy_minor: u32,
    },
    #[error("devtmpfs descriptor observation limit {limit} was exceeded at {phase:?}")]
    ObservationLimitExceeded {
        limit: usize,
        phase: DevtmpfsDescriptorObservationPhase,
    },
    #[error("devtmpfs descriptor work limit {limit} was exceeded while {action}")]
    WorkLimitExceeded { limit: usize, action: &'static str },
    #[error("devtmpfs descriptor authentication exceeded caller deadline {deadline:?}")]
    DeadlineExceeded { deadline: Instant },
    #[error("devtmpfs descriptor {phase:?} observation failed")]
    ObservationFailed {
        phase: DevtmpfsDescriptorObservationPhase,
        #[source]
        source: io::Error,
    },
    #[error("private devtmpfs descriptor observer returned the wrong value for {phase:?}")]
    ObservationProtocolViolation { phase: DevtmpfsDescriptorObservationPhase },
    #[error("descriptor filesystem magic drifted from {opening:#x} to {closing:#x}")]
    FilesystemMagicDrift {
        opening: nix::libc::c_long,
        closing: nix::libc::c_long,
    },
    #[error("descriptor filesystem magic is not Linux TMPFS_MAGIC: expected {expected:#x}, found {found:#x}")]
    UnsupportedFilesystemMagic {
        expected: nix::libc::c_long,
        found: nix::libc::c_long,
    },
    #[error("descriptor observed zero st_dev {device} or st_ino {inode} at {phase:?}")]
    InvalidObservedIdentity {
        phase: DevtmpfsDescriptorObservationPhase,
        device: u64,
        inode: u64,
    },
    #[error("descriptor observed zero mount ID at {phase:?}")]
    InvalidObservedMountId { phase: DevtmpfsDescriptorObservationPhase },
    #[error("descriptor inode kind drifted from {opening:#o} to {closing:#o}")]
    DirectoryKindDrift { opening: u32, closing: u32 },
    #[error("descriptor is not a directory: expected {expected:#o}, found {found:#o}")]
    UnsupportedDirectoryKind { expected: u32, found: u32 },
    #[error(
        "descriptor identity drifted from st_dev {opening_device}, st_ino {opening_inode} to st_dev {closing_device}, st_ino {closing_inode}"
    )]
    DirectoryIdentityDrift {
        opening_device: u64,
        opening_inode: u64,
        closing_device: u64,
        closing_inode: u64,
    },
    #[error(
        "descriptor identity does not match expectation: expected st_dev {expected_device}, st_ino {expected_inode}, found st_dev {found_device}, st_ino {found_inode}"
    )]
    UnexpectedDirectoryIdentity {
        expected_device: u64,
        expected_inode: u64,
        found_device: u64,
        found_inode: u64,
    },
    #[error("descriptor mount ID drifted from {opening} to {closing}")]
    DescriptorMountIdDrift { opening: u64, closing: u64 },
    #[error("descriptor mount ID {found} does not equal expected mount ID {expected}")]
    UnexpectedDescriptorMountId { expected: u64, found: u64 },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct RawDirectoryIdentity {
    device: u64,
    inode: u64,
    kind: u32,
}

enum RawObservation {
    DirectoryIdentity(RawDirectoryIdentity),
    DescriptorMountId(u64),
    FilesystemMagic(nix::libc::c_long),
}

/// Authenticate one already-retained directory against closed devtmpfs policy.
///
/// No caller-authored path is accepted and no storage path or device node is
/// opened. Fixed authenticated procfs `fd`/`fdinfo` resolution supplies each
/// mount ID. The descriptor is borrowed, and all outer observations use the original
/// deadline. The deadline bounds checkpoints and interrupted retries but
/// cannot preempt one kernel call already blocked.
pub(crate) fn authenticate_devtmpfs_same_mount_directory_until(
    directory: &std::fs::File,
    expected_device: u64,
    expected_inode: u64,
    expected_mount_id: u64,
    policy: ValidatedDevtmpfsMountInfoPolicy,
    deadline: Instant,
) -> Result<ValidatedDevtmpfsSameMountDescriptorEvidence, DevtmpfsDescriptorAuthenticationError> {
    let mut clock = Instant::now;
    let mut observer = |phase| observe_retained_directory(directory, phase, deadline);
    authenticate_with_observer(
        expected_device,
        expected_inode,
        expected_mount_id,
        policy,
        PRODUCTION_LIMITS,
        deadline,
        &mut clock,
        &mut observer,
    )
    .map(|(evidence, _usage)| evidence)
}

fn observe_retained_directory(
    directory: &std::fs::File,
    phase: DevtmpfsDescriptorObservationPhase,
    deadline: Instant,
) -> io::Result<RawObservation> {
    match phase {
        DevtmpfsDescriptorObservationPhase::OpeningDirectoryIdentity
        | DevtmpfsDescriptorObservationPhase::ClosingDirectoryIdentity => {
            raw_directory_identity(directory, deadline).map(RawObservation::DirectoryIdentity)
        }
        DevtmpfsDescriptorObservationPhase::OpeningDescriptorMountId
        | DevtmpfsDescriptorObservationPhase::ClosingDescriptorMountId => {
            descriptor_mount_id_until(directory, deadline).map(RawObservation::DescriptorMountId)
        }
        DevtmpfsDescriptorObservationPhase::OpeningFilesystemMagic
        | DevtmpfsDescriptorObservationPhase::ClosingFilesystemMagic => {
            raw_filesystem_magic(directory, deadline).map(RawObservation::FilesystemMagic)
        }
    }
}

fn raw_directory_identity(directory: &std::fs::File, deadline: Instant) -> io::Result<RawDirectoryIdentity> {
    // SAFETY: zeroed stat storage is a valid fstat output buffer and the
    // borrowed directory remains retained for the complete bounded call.
    let mut status: nix::libc::stat = unsafe { zeroed() };
    retry_interrupted(Some(deadline), || {
        // SAFETY: status is writable and directory remains a live descriptor.
        if unsafe { nix::libc::fstat(directory.as_raw_fd(), &mut status) } == 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    })?;
    Ok(RawDirectoryIdentity {
        device: status.st_dev,
        inode: status.st_ino,
        kind: status.st_mode & nix::libc::S_IFMT,
    })
}

fn raw_filesystem_magic(directory: &std::fs::File, deadline: Instant) -> io::Result<nix::libc::c_long> {
    // SAFETY: zeroed statfs storage is a valid fstatfs output buffer and the
    // borrowed directory remains retained for the complete bounded call.
    let mut status: nix::libc::statfs = unsafe { zeroed() };
    retry_interrupted(Some(deadline), || {
        // SAFETY: status is writable and directory remains a live descriptor.
        if unsafe { nix::libc::fstatfs(directory.as_raw_fd(), &mut status) } == 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    })?;
    Ok(status.f_type)
}

#[allow(clippy::too_many_arguments)]
fn authenticate_with_observer(
    expected_device: u64,
    expected_inode: u64,
    expected_mount_id: u64,
    policy: ValidatedDevtmpfsMountInfoPolicy,
    limits: AuthenticationLimits,
    deadline: Instant,
    clock: &mut impl FnMut() -> Instant,
    observer: &mut impl FnMut(DevtmpfsDescriptorObservationPhase) -> io::Result<RawObservation>,
) -> Result<(ValidatedDevtmpfsSameMountDescriptorEvidence, AuthenticationUsage), DevtmpfsDescriptorAuthenticationError>
{
    require_deadline(deadline, clock)?;
    validate_limits(limits)?;
    let mut operation = Operation::new(limits, deadline, clock);
    validate_expected_policy(
        expected_device,
        expected_inode,
        expected_mount_id,
        policy,
        &mut operation,
    )?;

    let opening_identity = require_identity_observation(
        operation.observe(DevtmpfsDescriptorObservationPhase::OpeningDirectoryIdentity, observer)?,
        DevtmpfsDescriptorObservationPhase::OpeningDirectoryIdentity,
    )?;
    let opening_mount_id = require_mount_id_observation(
        operation.observe(DevtmpfsDescriptorObservationPhase::OpeningDescriptorMountId, observer)?,
        DevtmpfsDescriptorObservationPhase::OpeningDescriptorMountId,
    )?;
    let opening_magic = require_magic_observation(
        operation.observe(DevtmpfsDescriptorObservationPhase::OpeningFilesystemMagic, observer)?,
        DevtmpfsDescriptorObservationPhase::OpeningFilesystemMagic,
    )?;
    let closing_magic = require_magic_observation(
        operation.observe(DevtmpfsDescriptorObservationPhase::ClosingFilesystemMagic, observer)?,
        DevtmpfsDescriptorObservationPhase::ClosingFilesystemMagic,
    )?;
    let closing_mount_id = require_mount_id_observation(
        operation.observe(DevtmpfsDescriptorObservationPhase::ClosingDescriptorMountId, observer)?,
        DevtmpfsDescriptorObservationPhase::ClosingDescriptorMountId,
    )?;
    let closing_identity = require_identity_observation(
        operation.observe(DevtmpfsDescriptorObservationPhase::ClosingDirectoryIdentity, observer)?,
        DevtmpfsDescriptorObservationPhase::ClosingDirectoryIdentity,
    )?;

    operation.charge(1, "checking filesystem-magic stability")?;
    if opening_magic != closing_magic {
        return Err(DevtmpfsDescriptorAuthenticationError::FilesystemMagicDrift {
            opening: opening_magic,
            closing: closing_magic,
        });
    }
    operation.charge(1, "checking the Linux filesystem-magic family")?;
    if opening_magic != TMPFS_MAGIC {
        return Err(DevtmpfsDescriptorAuthenticationError::UnsupportedFilesystemMagic {
            expected: TMPFS_MAGIC,
            found: opening_magic,
        });
    }

    require_nonzero_identity(
        opening_identity,
        DevtmpfsDescriptorObservationPhase::OpeningDirectoryIdentity,
        &mut operation,
    )?;
    require_nonzero_identity(
        closing_identity,
        DevtmpfsDescriptorObservationPhase::ClosingDirectoryIdentity,
        &mut operation,
    )?;
    require_nonzero_mount_id(
        opening_mount_id,
        DevtmpfsDescriptorObservationPhase::OpeningDescriptorMountId,
        &mut operation,
    )?;
    require_nonzero_mount_id(
        closing_mount_id,
        DevtmpfsDescriptorObservationPhase::ClosingDescriptorMountId,
        &mut operation,
    )?;

    operation.charge(1, "checking directory inode-kind stability")?;
    if opening_identity.kind != closing_identity.kind {
        return Err(DevtmpfsDescriptorAuthenticationError::DirectoryKindDrift {
            opening: opening_identity.kind,
            closing: closing_identity.kind,
        });
    }
    operation.charge(1, "checking the directory inode kind")?;
    if opening_identity.kind != nix::libc::S_IFDIR {
        return Err(DevtmpfsDescriptorAuthenticationError::UnsupportedDirectoryKind {
            expected: nix::libc::S_IFDIR,
            found: opening_identity.kind,
        });
    }
    operation.charge(1, "checking directory identity stability")?;
    if opening_identity.device != closing_identity.device || opening_identity.inode != closing_identity.inode {
        return Err(DevtmpfsDescriptorAuthenticationError::DirectoryIdentityDrift {
            opening_device: opening_identity.device,
            opening_inode: opening_identity.inode,
            closing_device: closing_identity.device,
            closing_inode: closing_identity.inode,
        });
    }
    operation.charge(1, "matching the expected directory identity")?;
    if opening_identity.device != expected_device || opening_identity.inode != expected_inode {
        return Err(DevtmpfsDescriptorAuthenticationError::UnexpectedDirectoryIdentity {
            expected_device,
            expected_inode,
            found_device: opening_identity.device,
            found_inode: opening_identity.inode,
        });
    }

    operation.charge(1, "checking descriptor mount-ID stability")?;
    if opening_mount_id != closing_mount_id {
        return Err(DevtmpfsDescriptorAuthenticationError::DescriptorMountIdDrift {
            opening: opening_mount_id,
            closing: closing_mount_id,
        });
    }
    operation.charge(1, "matching the expected descriptor mount ID")?;
    if opening_mount_id != expected_mount_id {
        return Err(DevtmpfsDescriptorAuthenticationError::UnexpectedDescriptorMountId {
            expected: expected_mount_id,
            found: opening_mount_id,
        });
    }

    operation.checkpoint()?;
    let usage = operation.usage();
    Ok((
        ValidatedDevtmpfsSameMountDescriptorEvidence {
            directory_device: opening_identity.device,
            directory_inode: opening_identity.inode,
            mount_id: opening_mount_id,
            filesystem: policy.filesystem(),
            access_mode: policy.access_mode(),
            magic_family: DevtmpfsDescriptorMagicFamily::LinuxTmpfs,
        },
        usage,
    ))
}

fn validate_limits(limits: AuthenticationLimits) -> Result<(), DevtmpfsDescriptorAuthenticationError> {
    if limits.max_observations == 0
        || limits.max_observations > PRODUCTION_LIMITS.max_observations
        || limits.max_work == 0
        || limits.max_work > PRODUCTION_LIMITS.max_work
    {
        Err(DevtmpfsDescriptorAuthenticationError::InvalidLimits)
    } else {
        Ok(())
    }
}

fn validate_expected_policy(
    expected_device: u64,
    expected_inode: u64,
    expected_mount_id: u64,
    policy: ValidatedDevtmpfsMountInfoPolicy,
    operation: &mut Operation<'_, impl FnMut() -> Instant>,
) -> Result<(), DevtmpfsDescriptorAuthenticationError> {
    operation.charge(1, "validating the expected descriptor identity")?;
    if expected_device == 0 || expected_inode == 0 || expected_mount_id == 0 {
        return Err(DevtmpfsDescriptorAuthenticationError::InvalidExpectedIdentity {
            device: expected_device,
            inode: expected_inode,
            mount_id: expected_mount_id,
        });
    }
    operation.charge(1, "decoding the canonical expected st_dev")?;
    let (expected_major, expected_minor) = canonical_device_number(expected_device)?;
    operation.charge(1, "matching the devtmpfs policy mount ID")?;
    if policy.mount_id() != expected_mount_id {
        return Err(DevtmpfsDescriptorAuthenticationError::PolicyMountIdMismatch {
            expected_mount_id,
            policy_mount_id: policy.mount_id(),
        });
    }
    operation.charge(1, "matching the devtmpfs policy device number")?;
    if policy.device_major() != expected_major || policy.device_minor() != expected_minor {
        return Err(DevtmpfsDescriptorAuthenticationError::PolicyDeviceMismatch {
            expected_major,
            expected_minor,
            policy_major: policy.device_major(),
            policy_minor: policy.device_minor(),
        });
    }
    Ok(())
}

fn canonical_device_number(device: u64) -> Result<(u32, u32), DevtmpfsDescriptorAuthenticationError> {
    let raw: nix::libc::dev_t = device;
    let major: u32 = nix::libc::major(raw);
    let minor: u32 = nix::libc::minor(raw);
    if nix::libc::makedev(major, minor) != raw {
        Err(DevtmpfsDescriptorAuthenticationError::InvalidExpectedDeviceEncoding { device })
    } else {
        Ok((major, minor))
    }
}

fn require_nonzero_identity(
    identity: RawDirectoryIdentity,
    phase: DevtmpfsDescriptorObservationPhase,
    operation: &mut Operation<'_, impl FnMut() -> Instant>,
) -> Result<(), DevtmpfsDescriptorAuthenticationError> {
    operation.charge(1, "checking a nonzero observed directory identity")?;
    if identity.device == 0 || identity.inode == 0 {
        Err(DevtmpfsDescriptorAuthenticationError::InvalidObservedIdentity {
            phase,
            device: identity.device,
            inode: identity.inode,
        })
    } else {
        Ok(())
    }
}

fn require_nonzero_mount_id(
    mount_id: u64,
    phase: DevtmpfsDescriptorObservationPhase,
    operation: &mut Operation<'_, impl FnMut() -> Instant>,
) -> Result<(), DevtmpfsDescriptorAuthenticationError> {
    operation.charge(1, "checking a nonzero observed descriptor mount ID")?;
    if mount_id == 0 {
        Err(DevtmpfsDescriptorAuthenticationError::InvalidObservedMountId { phase })
    } else {
        Ok(())
    }
}

fn require_identity_observation(
    observation: RawObservation,
    phase: DevtmpfsDescriptorObservationPhase,
) -> Result<RawDirectoryIdentity, DevtmpfsDescriptorAuthenticationError> {
    match observation {
        RawObservation::DirectoryIdentity(identity) => Ok(identity),
        RawObservation::DescriptorMountId(_) | RawObservation::FilesystemMagic(_) => {
            Err(DevtmpfsDescriptorAuthenticationError::ObservationProtocolViolation { phase })
        }
    }
}

fn require_mount_id_observation(
    observation: RawObservation,
    phase: DevtmpfsDescriptorObservationPhase,
) -> Result<u64, DevtmpfsDescriptorAuthenticationError> {
    match observation {
        RawObservation::DescriptorMountId(mount_id) => Ok(mount_id),
        RawObservation::DirectoryIdentity(_) | RawObservation::FilesystemMagic(_) => {
            Err(DevtmpfsDescriptorAuthenticationError::ObservationProtocolViolation { phase })
        }
    }
}

fn require_magic_observation(
    observation: RawObservation,
    phase: DevtmpfsDescriptorObservationPhase,
) -> Result<nix::libc::c_long, DevtmpfsDescriptorAuthenticationError> {
    match observation {
        RawObservation::FilesystemMagic(magic) => Ok(magic),
        RawObservation::DirectoryIdentity(_) | RawObservation::DescriptorMountId(_) => {
            Err(DevtmpfsDescriptorAuthenticationError::ObservationProtocolViolation { phase })
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct AuthenticationUsage {
    observations: usize,
    work: usize,
}

struct Operation<'a, Clock> {
    deadline: Instant,
    max_observations: usize,
    remaining_observations: usize,
    max_work: usize,
    remaining_work: usize,
    clock: &'a mut Clock,
}

impl<'a, Clock: FnMut() -> Instant> Operation<'a, Clock> {
    fn new(limits: AuthenticationLimits, deadline: Instant, clock: &'a mut Clock) -> Self {
        Self {
            deadline,
            max_observations: limits.max_observations,
            remaining_observations: limits.max_observations,
            max_work: limits.max_work,
            remaining_work: limits.max_work,
            clock,
        }
    }

    fn observe(
        &mut self,
        phase: DevtmpfsDescriptorObservationPhase,
        observer: &mut impl FnMut(DevtmpfsDescriptorObservationPhase) -> io::Result<RawObservation>,
    ) -> Result<RawObservation, DevtmpfsDescriptorAuthenticationError> {
        self.checkpoint()?;
        self.remaining_observations = self.remaining_observations.checked_sub(1).ok_or(
            DevtmpfsDescriptorAuthenticationError::ObservationLimitExceeded {
                limit: self.max_observations,
                phase,
            },
        )?;
        self.charge(1, "recording one bounded descriptor observation")?;
        let observation = observer(phase)
            .map_err(|source| DevtmpfsDescriptorAuthenticationError::ObservationFailed { phase, source })?;
        self.checkpoint()?;
        Ok(observation)
    }

    fn charge(&mut self, amount: usize, action: &'static str) -> Result<(), DevtmpfsDescriptorAuthenticationError> {
        self.checkpoint()?;
        self.remaining_work = self.remaining_work.checked_sub(amount).ok_or(
            DevtmpfsDescriptorAuthenticationError::WorkLimitExceeded {
                limit: self.max_work,
                action,
            },
        )?;
        self.checkpoint()
    }

    fn checkpoint(&mut self) -> Result<(), DevtmpfsDescriptorAuthenticationError> {
        require_deadline(self.deadline, self.clock)
    }

    const fn usage(&self) -> AuthenticationUsage {
        AuthenticationUsage {
            observations: self.max_observations - self.remaining_observations,
            work: self.max_work - self.remaining_work,
        }
    }
}

fn require_deadline(
    deadline: Instant,
    clock: &mut impl FnMut() -> Instant,
) -> Result<(), DevtmpfsDescriptorAuthenticationError> {
    if clock() > deadline {
        Err(DevtmpfsDescriptorAuthenticationError::DeadlineExceeded { deadline })
    } else {
        Ok(())
    }
}

#[cfg(test)]
pub(crate) const FIXTURE_TMPFS_MAGIC: nix::libc::c_long = TMPFS_MAGIC;

#[cfg(test)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct FixtureDevtmpfsDescriptorIdentity {
    pub(crate) device: u64,
    pub(crate) inode: u64,
    pub(crate) kind: u32,
}

#[cfg(test)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct FixtureDevtmpfsDescriptorObservations {
    pub(crate) opening_identity: FixtureDevtmpfsDescriptorIdentity,
    pub(crate) opening_mount_id: u64,
    pub(crate) opening_magic: nix::libc::c_long,
    pub(crate) closing_magic: nix::libc::c_long,
    pub(crate) closing_mount_id: u64,
    pub(crate) closing_identity: FixtureDevtmpfsDescriptorIdentity,
}

#[cfg(test)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct FixtureDevtmpfsDescriptorLimits {
    pub(crate) max_observations: usize,
    pub(crate) max_work: usize,
}

#[cfg(test)]
impl Default for FixtureDevtmpfsDescriptorLimits {
    fn default() -> Self {
        Self {
            max_observations: PRODUCTION_LIMITS.max_observations,
            max_work: PRODUCTION_LIMITS.max_work,
        }
    }
}

#[cfg(test)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct FixtureDevtmpfsDescriptorUsage {
    pub(crate) observations: usize,
    pub(crate) work: usize,
}

#[cfg(test)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum FixtureDevtmpfsRawObservation {
    DirectoryIdentity(FixtureDevtmpfsDescriptorIdentity),
    DescriptorMountId(u64),
    FilesystemMagic(nix::libc::c_long),
}

#[cfg(test)]
#[allow(clippy::too_many_arguments)]
pub(crate) fn validate_fixture_devtmpfs_descriptor_authentication(
    observations: FixtureDevtmpfsDescriptorObservations,
    expected_device: u64,
    expected_inode: u64,
    expected_mount_id: u64,
    policy: ValidatedDevtmpfsMountInfoPolicy,
    limits: FixtureDevtmpfsDescriptorLimits,
    deadline: Instant,
    clock: &mut impl FnMut() -> Instant,
    hook: &mut impl FnMut(DevtmpfsDescriptorObservationPhase) -> io::Result<()>,
) -> Result<
    (
        ValidatedDevtmpfsSameMountDescriptorEvidence,
        FixtureDevtmpfsDescriptorUsage,
    ),
    DevtmpfsDescriptorAuthenticationError,
> {
    let mut observer = |phase| {
        hook(phase)?;
        Ok(match phase {
            DevtmpfsDescriptorObservationPhase::OpeningDirectoryIdentity => {
                RawObservation::DirectoryIdentity(observations.opening_identity.into())
            }
            DevtmpfsDescriptorObservationPhase::OpeningDescriptorMountId => {
                RawObservation::DescriptorMountId(observations.opening_mount_id)
            }
            DevtmpfsDescriptorObservationPhase::OpeningFilesystemMagic => {
                RawObservation::FilesystemMagic(observations.opening_magic)
            }
            DevtmpfsDescriptorObservationPhase::ClosingFilesystemMagic => {
                RawObservation::FilesystemMagic(observations.closing_magic)
            }
            DevtmpfsDescriptorObservationPhase::ClosingDescriptorMountId => {
                RawObservation::DescriptorMountId(observations.closing_mount_id)
            }
            DevtmpfsDescriptorObservationPhase::ClosingDirectoryIdentity => {
                RawObservation::DirectoryIdentity(observations.closing_identity.into())
            }
        })
    };
    authenticate_fixture_with_observer(
        expected_device,
        expected_inode,
        expected_mount_id,
        policy,
        limits,
        deadline,
        clock,
        &mut observer,
    )
}

#[cfg(test)]
#[allow(clippy::too_many_arguments)]
pub(crate) fn validate_fixture_devtmpfs_descriptor_protocol(
    expected_device: u64,
    expected_inode: u64,
    expected_mount_id: u64,
    policy: ValidatedDevtmpfsMountInfoPolicy,
    limits: FixtureDevtmpfsDescriptorLimits,
    deadline: Instant,
    clock: &mut impl FnMut() -> Instant,
    fixture_observer: &mut impl FnMut(DevtmpfsDescriptorObservationPhase) -> io::Result<FixtureDevtmpfsRawObservation>,
) -> Result<
    (
        ValidatedDevtmpfsSameMountDescriptorEvidence,
        FixtureDevtmpfsDescriptorUsage,
    ),
    DevtmpfsDescriptorAuthenticationError,
> {
    let mut observer = |phase| {
        fixture_observer(phase).map(|observation| match observation {
            FixtureDevtmpfsRawObservation::DirectoryIdentity(identity) => {
                RawObservation::DirectoryIdentity(identity.into())
            }
            FixtureDevtmpfsRawObservation::DescriptorMountId(mount_id) => RawObservation::DescriptorMountId(mount_id),
            FixtureDevtmpfsRawObservation::FilesystemMagic(magic) => RawObservation::FilesystemMagic(magic),
        })
    };
    authenticate_fixture_with_observer(
        expected_device,
        expected_inode,
        expected_mount_id,
        policy,
        limits,
        deadline,
        clock,
        &mut observer,
    )
}

#[cfg(test)]
#[allow(clippy::too_many_arguments)]
fn authenticate_fixture_with_observer(
    expected_device: u64,
    expected_inode: u64,
    expected_mount_id: u64,
    policy: ValidatedDevtmpfsMountInfoPolicy,
    limits: FixtureDevtmpfsDescriptorLimits,
    deadline: Instant,
    clock: &mut impl FnMut() -> Instant,
    observer: &mut impl FnMut(DevtmpfsDescriptorObservationPhase) -> io::Result<RawObservation>,
) -> Result<
    (
        ValidatedDevtmpfsSameMountDescriptorEvidence,
        FixtureDevtmpfsDescriptorUsage,
    ),
    DevtmpfsDescriptorAuthenticationError,
> {
    authenticate_with_observer(
        expected_device,
        expected_inode,
        expected_mount_id,
        policy,
        AuthenticationLimits {
            max_observations: limits.max_observations,
            max_work: limits.max_work,
        },
        deadline,
        clock,
        observer,
    )
    .map(|(evidence, usage)| {
        (
            evidence,
            FixtureDevtmpfsDescriptorUsage {
                observations: usage.observations,
                work: usage.work,
            },
        )
    })
}

#[cfg(test)]
impl From<FixtureDevtmpfsDescriptorIdentity> for RawDirectoryIdentity {
    fn from(identity: FixtureDevtmpfsDescriptorIdentity) -> Self {
        Self {
            device: identity.device,
            inode: identity.inode,
            kind: identity.kind,
        }
    }
}
