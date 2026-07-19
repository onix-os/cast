//! Descriptor-bound Linux boot-filesystem evidence.
//!
//! This layer authenticates one already-retained directory descriptor against
//! the caller's exact `st_dev`/`st_ino` expectation. It sandwiches two
//! `fstatfs(2)` observations between two `fstat(2)` observations under one
//! caller-owned absolute deadline. The resulting value contains only closed
//! scalar evidence; it grants no descriptor, path-resolution, reopen, read,
//! write, publication, or durability authority.
//!
//! Linux reports the FAT/MS-DOS filesystem family with `MSDOS_SUPER_MAGIC`.
//! That magic is not an exact filesystem-driver name and does **not** by itself
//! prove that mountinfo reported `vfat`. A future consumer must combine this
//! evidence with the separate exact mountinfo policy before publication.

use std::{io, mem::zeroed, os::fd::AsRawFd as _, time::Instant};

use thiserror::Error;

use super::retry_interrupted;

const MSDOS_SUPER_MAGIC: nix::libc::c_long = 0x4d44;
const REQUIRED_OBSERVATIONS: usize = 4;
const PRODUCTION_MAX_WORK: usize = 32;

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
/// This is deliberately named for the kernel magic family, not for the exact
/// `vfat` mountinfo driver which must be proven independently.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) enum BootFilesystemMagicFamily {
    LinuxMsdos,
}

/// Scalar-only evidence from one retained destination directory.
///
/// The identity fields bind the evidence to the caller's expected directory,
/// but they are descriptive numbers only. This value owns no filesystem or
/// namespace capability and is not an ongoing-currentness guarantee.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) struct ValidatedBootFilesystemDescriptorEvidence {
    destination_device: u64,
    destination_inode: u64,
    magic_family: BootFilesystemMagicFamily,
}

impl ValidatedBootFilesystemDescriptorEvidence {
    pub(crate) const fn destination_device(self) -> u64 {
        self.destination_device
    }

    pub(crate) const fn destination_inode(self) -> u64 {
        self.destination_inode
    }

    pub(crate) const fn magic_family(self) -> BootFilesystemMagicFamily {
        self.magic_family
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum BootFilesystemObservationPhase {
    OpeningDirectoryIdentity,
    OpeningFilesystemMagic,
    ClosingFilesystemMagic,
    ClosingDirectoryIdentity,
}

#[derive(Debug, Error)]
pub(crate) enum BootFilesystemAuthenticationError {
    #[error("boot-filesystem authentication limits must be nonzero")]
    InvalidLimits,
    #[error("expected boot destination has zero st_dev {device} or st_ino {inode}")]
    InvalidExpectedIdentity { device: u64, inode: u64 },
    #[error("boot-filesystem observation limit {limit} was exceeded at {phase:?}")]
    ObservationLimitExceeded {
        limit: usize,
        phase: BootFilesystemObservationPhase,
    },
    #[error("boot-filesystem work limit {limit} was exceeded while {action}")]
    WorkLimitExceeded { limit: usize, action: &'static str },
    #[error("boot-filesystem authentication exceeded caller deadline {deadline:?}")]
    DeadlineExceeded { deadline: Instant },
    #[error("boot-filesystem {phase:?} observation failed")]
    ObservationFailed {
        phase: BootFilesystemObservationPhase,
        #[source]
        source: io::Error,
    },
    #[error("private boot-filesystem observer returned the wrong value for {phase:?}")]
    ObservationProtocolViolation { phase: BootFilesystemObservationPhase },
    #[error("boot-filesystem magic drifted from {opening:#x} to {closing:#x}")]
    FilesystemMagicDrift {
        opening: nix::libc::c_long,
        closing: nix::libc::c_long,
    },
    #[error("boot-filesystem magic is not Linux MSDOS_SUPER_MAGIC: expected {expected:#x}, found {found:#x}")]
    UnsupportedFilesystemMagic {
        expected: nix::libc::c_long,
        found: nix::libc::c_long,
    },
    #[error("boot destination observed zero st_dev {device} or st_ino {inode} at {phase:?}")]
    InvalidObservedIdentity {
        phase: BootFilesystemObservationPhase,
        device: u64,
        inode: u64,
    },
    #[error("boot destination inode kind drifted from {opening:#o} to {closing:#o}")]
    DirectoryKindDrift { opening: u32, closing: u32 },
    #[error("boot destination is not a directory: expected {expected:#o}, found {found:#o}")]
    UnsupportedDirectoryKind { expected: u32, found: u32 },
    #[error(
        "boot destination identity drifted from st_dev {opening_device}, st_ino {opening_inode} to st_dev {closing_device}, st_ino {closing_inode}"
    )]
    DirectoryIdentityDrift {
        opening_device: u64,
        opening_inode: u64,
        closing_device: u64,
        closing_inode: u64,
    },
    #[error(
        "boot destination identity does not match the caller expectation: expected st_dev {expected_device}, st_ino {expected_inode}, found st_dev {found_device}, st_ino {found_inode}"
    )]
    UnexpectedDirectoryIdentity {
        expected_device: u64,
        expected_inode: u64,
        found_device: u64,
        found_inode: u64,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct RawDirectoryIdentity {
    device: u64,
    inode: u64,
    kind: u32,
}

enum RawObservation {
    DirectoryIdentity(RawDirectoryIdentity),
    FilesystemMagic(nix::libc::c_long),
}

/// Authenticate one already-retained directory under the caller's deadline.
///
/// No path is accepted or opened here. The descriptor is borrowed, never
/// cloned into the result, and all four syscall observations use the same
/// absolute deadline without installing a replacement timeout. As with the
/// other descriptor primitives, that deadline bounds checks and interrupted
/// retries but cannot preempt one kernel syscall which is already blocked.
pub(crate) fn authenticate_boot_filesystem_directory_until(
    directory: &std::fs::File,
    expected_device: u64,
    expected_inode: u64,
    deadline: Instant,
) -> Result<ValidatedBootFilesystemDescriptorEvidence, BootFilesystemAuthenticationError> {
    let mut clock = Instant::now;
    let mut observer = |phase| observe_retained_directory(directory, phase, deadline);
    authenticate_with_observer(
        expected_device,
        expected_inode,
        PRODUCTION_LIMITS,
        deadline,
        &mut clock,
        &mut observer,
    )
    .map(|(evidence, _usage)| evidence)
}

fn observe_retained_directory(
    directory: &std::fs::File,
    phase: BootFilesystemObservationPhase,
    deadline: Instant,
) -> io::Result<RawObservation> {
    match phase {
        BootFilesystemObservationPhase::OpeningDirectoryIdentity
        | BootFilesystemObservationPhase::ClosingDirectoryIdentity => {
            raw_directory_identity(directory, deadline).map(RawObservation::DirectoryIdentity)
        }
        BootFilesystemObservationPhase::OpeningFilesystemMagic
        | BootFilesystemObservationPhase::ClosingFilesystemMagic => {
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

fn authenticate_with_observer(
    expected_device: u64,
    expected_inode: u64,
    limits: AuthenticationLimits,
    deadline: Instant,
    clock: &mut impl FnMut() -> Instant,
    observer: &mut impl FnMut(BootFilesystemObservationPhase) -> io::Result<RawObservation>,
) -> Result<(ValidatedBootFilesystemDescriptorEvidence, AuthenticationUsage), BootFilesystemAuthenticationError> {
    require_deadline(deadline, clock)?;
    if limits.max_observations == 0 || limits.max_work == 0 {
        return Err(BootFilesystemAuthenticationError::InvalidLimits);
    }
    let mut operation = Operation::new(limits, deadline, clock);
    operation.charge(1, "validating the caller's expected destination identity")?;
    if expected_device == 0 || expected_inode == 0 {
        return Err(BootFilesystemAuthenticationError::InvalidExpectedIdentity {
            device: expected_device,
            inode: expected_inode,
        });
    }

    let opening_identity = require_identity_observation(
        operation.observe(BootFilesystemObservationPhase::OpeningDirectoryIdentity, observer)?,
        BootFilesystemObservationPhase::OpeningDirectoryIdentity,
    )?;
    let opening_magic = require_magic_observation(
        operation.observe(BootFilesystemObservationPhase::OpeningFilesystemMagic, observer)?,
        BootFilesystemObservationPhase::OpeningFilesystemMagic,
    )?;
    let closing_magic = require_magic_observation(
        operation.observe(BootFilesystemObservationPhase::ClosingFilesystemMagic, observer)?,
        BootFilesystemObservationPhase::ClosingFilesystemMagic,
    )?;
    let closing_identity = require_identity_observation(
        operation.observe(BootFilesystemObservationPhase::ClosingDirectoryIdentity, observer)?,
        BootFilesystemObservationPhase::ClosingDirectoryIdentity,
    )?;

    operation.charge(1, "checking filesystem-magic stability")?;
    if opening_magic != closing_magic {
        return Err(BootFilesystemAuthenticationError::FilesystemMagicDrift {
            opening: opening_magic,
            closing: closing_magic,
        });
    }
    operation.charge(1, "checking the Linux filesystem-magic family")?;
    if opening_magic != MSDOS_SUPER_MAGIC {
        return Err(BootFilesystemAuthenticationError::UnsupportedFilesystemMagic {
            expected: MSDOS_SUPER_MAGIC,
            found: opening_magic,
        });
    }

    require_nonzero_identity(
        opening_identity,
        BootFilesystemObservationPhase::OpeningDirectoryIdentity,
        &mut operation,
    )?;
    require_nonzero_identity(
        closing_identity,
        BootFilesystemObservationPhase::ClosingDirectoryIdentity,
        &mut operation,
    )?;

    operation.charge(1, "checking destination inode-kind stability")?;
    if opening_identity.kind != closing_identity.kind {
        return Err(BootFilesystemAuthenticationError::DirectoryKindDrift {
            opening: opening_identity.kind,
            closing: closing_identity.kind,
        });
    }
    operation.charge(1, "checking the destination directory inode kind")?;
    if opening_identity.kind != nix::libc::S_IFDIR {
        return Err(BootFilesystemAuthenticationError::UnsupportedDirectoryKind {
            expected: nix::libc::S_IFDIR,
            found: opening_identity.kind,
        });
    }

    operation.charge(1, "checking destination identity stability")?;
    if opening_identity.device != closing_identity.device || opening_identity.inode != closing_identity.inode {
        return Err(BootFilesystemAuthenticationError::DirectoryIdentityDrift {
            opening_device: opening_identity.device,
            opening_inode: opening_identity.inode,
            closing_device: closing_identity.device,
            closing_inode: closing_identity.inode,
        });
    }
    operation.charge(1, "matching the expected destination identity")?;
    if opening_identity.device != expected_device || opening_identity.inode != expected_inode {
        return Err(BootFilesystemAuthenticationError::UnexpectedDirectoryIdentity {
            expected_device,
            expected_inode,
            found_device: opening_identity.device,
            found_inode: opening_identity.inode,
        });
    }

    operation.checkpoint()?;
    let usage = operation.usage();
    Ok((
        ValidatedBootFilesystemDescriptorEvidence {
            destination_device: opening_identity.device,
            destination_inode: opening_identity.inode,
            magic_family: BootFilesystemMagicFamily::LinuxMsdos,
        },
        usage,
    ))
}

fn require_nonzero_identity(
    identity: RawDirectoryIdentity,
    phase: BootFilesystemObservationPhase,
    operation: &mut Operation<'_, impl FnMut() -> Instant>,
) -> Result<(), BootFilesystemAuthenticationError> {
    operation.charge(1, "checking a nonzero observed destination identity")?;
    if identity.device == 0 || identity.inode == 0 {
        Err(BootFilesystemAuthenticationError::InvalidObservedIdentity {
            phase,
            device: identity.device,
            inode: identity.inode,
        })
    } else {
        Ok(())
    }
}

fn require_identity_observation(
    observation: RawObservation,
    phase: BootFilesystemObservationPhase,
) -> Result<RawDirectoryIdentity, BootFilesystemAuthenticationError> {
    match observation {
        RawObservation::DirectoryIdentity(identity) => Ok(identity),
        RawObservation::FilesystemMagic(_) => {
            Err(BootFilesystemAuthenticationError::ObservationProtocolViolation { phase })
        }
    }
}

fn require_magic_observation(
    observation: RawObservation,
    phase: BootFilesystemObservationPhase,
) -> Result<nix::libc::c_long, BootFilesystemAuthenticationError> {
    match observation {
        RawObservation::FilesystemMagic(magic) => Ok(magic),
        RawObservation::DirectoryIdentity(_) => {
            Err(BootFilesystemAuthenticationError::ObservationProtocolViolation { phase })
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
        phase: BootFilesystemObservationPhase,
        observer: &mut impl FnMut(BootFilesystemObservationPhase) -> io::Result<RawObservation>,
    ) -> Result<RawObservation, BootFilesystemAuthenticationError> {
        self.checkpoint()?;
        self.remaining_observations = self.remaining_observations.checked_sub(1).ok_or(
            BootFilesystemAuthenticationError::ObservationLimitExceeded {
                limit: self.max_observations,
                phase,
            },
        )?;
        self.charge(1, "recording one bounded descriptor observation")?;
        let observation =
            observer(phase).map_err(|source| BootFilesystemAuthenticationError::ObservationFailed { phase, source })?;
        self.checkpoint()?;
        Ok(observation)
    }

    fn charge(&mut self, amount: usize, action: &'static str) -> Result<(), BootFilesystemAuthenticationError> {
        self.checkpoint()?;
        self.remaining_work =
            self.remaining_work
                .checked_sub(amount)
                .ok_or(BootFilesystemAuthenticationError::WorkLimitExceeded {
                    limit: self.max_work,
                    action,
                })?;
        self.checkpoint()
    }

    fn checkpoint(&mut self) -> Result<(), BootFilesystemAuthenticationError> {
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
) -> Result<(), BootFilesystemAuthenticationError> {
    if clock() > deadline {
        Err(BootFilesystemAuthenticationError::DeadlineExceeded { deadline })
    } else {
        Ok(())
    }
}

#[cfg(test)]
pub(crate) const FIXTURE_MSDOS_SUPER_MAGIC: nix::libc::c_long = MSDOS_SUPER_MAGIC;

#[cfg(test)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct FixtureBootFilesystemIdentity {
    pub(crate) device: u64,
    pub(crate) inode: u64,
    pub(crate) kind: u32,
}

#[cfg(test)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct FixtureBootFilesystemObservations {
    pub(crate) opening_identity: FixtureBootFilesystemIdentity,
    pub(crate) opening_magic: nix::libc::c_long,
    pub(crate) closing_magic: nix::libc::c_long,
    pub(crate) closing_identity: FixtureBootFilesystemIdentity,
}

#[cfg(test)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct FixtureBootFilesystemLimits {
    pub(crate) max_observations: usize,
    pub(crate) max_work: usize,
}

#[cfg(test)]
impl Default for FixtureBootFilesystemLimits {
    fn default() -> Self {
        Self {
            max_observations: PRODUCTION_LIMITS.max_observations,
            max_work: PRODUCTION_LIMITS.max_work,
        }
    }
}

#[cfg(test)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct FixtureBootFilesystemUsage {
    pub(crate) observations: usize,
    pub(crate) work: usize,
}

#[cfg(test)]
pub(crate) fn validate_fixture_boot_filesystem_authentication(
    observations: FixtureBootFilesystemObservations,
    expected_device: u64,
    expected_inode: u64,
    limits: FixtureBootFilesystemLimits,
    deadline: Instant,
    clock: &mut impl FnMut() -> Instant,
    hook: &mut impl FnMut(BootFilesystemObservationPhase) -> io::Result<()>,
) -> Result<(ValidatedBootFilesystemDescriptorEvidence, FixtureBootFilesystemUsage), BootFilesystemAuthenticationError>
{
    let mut observer = |phase| {
        hook(phase)?;
        Ok(match phase {
            BootFilesystemObservationPhase::OpeningDirectoryIdentity => {
                RawObservation::DirectoryIdentity(observations.opening_identity.into())
            }
            BootFilesystemObservationPhase::OpeningFilesystemMagic => {
                RawObservation::FilesystemMagic(observations.opening_magic)
            }
            BootFilesystemObservationPhase::ClosingFilesystemMagic => {
                RawObservation::FilesystemMagic(observations.closing_magic)
            }
            BootFilesystemObservationPhase::ClosingDirectoryIdentity => {
                RawObservation::DirectoryIdentity(observations.closing_identity.into())
            }
        })
    };
    authenticate_with_observer(
        expected_device,
        expected_inode,
        AuthenticationLimits {
            max_observations: limits.max_observations,
            max_work: limits.max_work,
        },
        deadline,
        clock,
        &mut observer,
    )
    .map(|(evidence, usage)| {
        (
            evidence,
            FixtureBootFilesystemUsage {
                observations: usage.observations,
                work: usage.work,
            },
        )
    })
}

#[cfg(test)]
impl From<FixtureBootFilesystemIdentity> for RawDirectoryIdentity {
    fn from(identity: FixtureBootFilesystemIdentity) -> Self {
        Self {
            device: identity.device,
            inode: identity.inode,
            kind: identity.kind,
        }
    }
}
