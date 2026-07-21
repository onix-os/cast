use std::{fs::File, io, os::fd::AsRawFd as _, time::Instant};

#[cfg(test)]
use std::cell::{Cell, RefCell};

use sha2::{Digest as _, Sha256};
use xxhash_rust::xxh3::Xxh3;

use super::{
    error::RetainedBootFilePublicationError,
    model::{RetainedBootFilePublicationLimits, RetainedBootFilePublicationRequest},
};
use crate::linux_fs::descriptor_boot_namespace::BoundRetainedBootFileSource;

const STREAM_BUFFER_BYTES: usize = 4 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum FixtureRetainedBootFilePublicationFault {
    AfterExclusiveCreation,
    MidMultiChunkWrite,
    AfterFinalWriteBeforeSourceValidation,
    BeforePrivateSync,
    RenameReportsErrorAfterApplied,
    BeforeCanonicalSync,
    BeforeParentSync,
    BeforeFilesystemSync,
}

impl FixtureRetainedBootFilePublicationFault {
    const fn label(self) -> &'static str {
        match self {
            Self::AfterExclusiveCreation => "after-exclusive-creation",
            Self::MidMultiChunkWrite => "mid-multi-chunk-write",
            Self::AfterFinalWriteBeforeSourceValidation => "after-final-write-before-source-validation",
            Self::BeforePrivateSync => "before-private-sync",
            Self::RenameReportsErrorAfterApplied => "rename-reports-error-after-applied",
            Self::BeforeCanonicalSync => "before-canonical-sync",
            Self::BeforeParentSync => "before-parent-sync",
            Self::BeforeFilesystemSync => "before-filesystem-sync",
        }
    }
}

#[cfg(test)]
thread_local! {
    static PUBLICATION_FAULT: Cell<Option<FixtureRetainedBootFilePublicationFault>> = const { Cell::new(None) };
    static PRIVATE_NAME_SUBSTITUTION: RefCell<Option<Box<dyn FnOnce()>>> = RefCell::new(None);
}

#[cfg(test)]
pub(crate) fn arm_retained_boot_file_publication_fault(point: FixtureRetainedBootFilePublicationFault) {
    PUBLICATION_FAULT.with(|slot| {
        assert!(slot.replace(Some(point)).is_none(), "boot-file publication fault already armed");
    });
}

/// Install one test-only callback in the final window between authenticating
/// the private name and issuing the sole no-replace rename.
#[cfg(test)]
pub(crate) fn arm_retained_boot_file_private_name_substitution(callback: impl FnOnce() + 'static) {
    PRIVATE_NAME_SUBSTITUTION.with(|slot| {
        assert!(slot.borrow_mut().replace(Box::new(callback)).is_none(), "private-name substitution already armed");
    });
}

pub(super) fn before_private_name_rename() {
    #[cfg(test)]
    {
        let callback = PRIVATE_NAME_SUBSTITUTION.with(|slot| slot.borrow_mut().take());
        if let Some(callback) = callback {
            callback();
        }
    }
}

pub(super) fn fault(point: FixtureRetainedBootFilePublicationFault) -> Result<(), RetainedBootFilePublicationError> {
    #[cfg(test)]
    {
        let armed = PUBLICATION_FAULT.with(|slot| slot.get());
        if armed == Some(point) {
            PUBLICATION_FAULT.with(|slot| slot.set(None));
            return Err(RetainedBootFilePublicationError::InjectedFault { point: point.label() });
        }
    }
    #[cfg(not(test))]
    let _ = point;
    Ok(())
}

pub(super) fn stream_expected_source(
    source: &mut BoundRetainedBootFileSource<'_, '_, '_>,
    destination: &File,
    request: RetainedBootFilePublicationRequest<'_>,
    limits: RetainedBootFilePublicationLimits,
    deadline: Instant,
) -> Result<(), RetainedBootFilePublicationError> {
    checkpoint(deadline)?;
    let mut xxh3 = Xxh3::new();
    let mut sha256 = Sha256::new();
    let mut buffer = [0u8; STREAM_BUFFER_BYTES];
    let mut offset = 0u64;
    let mut write_calls = 0usize;
    let mut written_bytes = 0u64;

    while offset < request.expected_length() {
        checkpoint(deadline)?;
        source.checkpoint().map_err(|source| RetainedBootFilePublicationError::Source { source })?;
        let offered = usize::try_from((request.expected_length() - offset).min(STREAM_BUFFER_BYTES as u64))
            .expect("fixed boot-file stream buffer fits usize");
        let found = source
            .read_at(offset, &mut buffer[..offered])
            .map_err(|source| RetainedBootFilePublicationError::Source { source })?;
        if found == 0 || found > offered {
            return Err(RetainedBootFilePublicationError::ContentIdentityMismatch {
                field: "source length",
            });
        }
        xxh3.update(&buffer[..found]);
        sha256.update(&buffer[..found]);
        let mut written = 0usize;
        while written < found {
            checkpoint(deadline)?;
            write_calls = write_calls.checked_add(1).ok_or(RetainedBootFilePublicationError::InvalidLimit {
                field: "write calls",
            })?;
            if write_calls > limits.max_write_calls {
                return Err(RetainedBootFilePublicationError::InvalidLimit {
                    field: "write calls",
                });
            }
            let write_offset = offset.checked_add(written as u64).ok_or(
                RetainedBootFilePublicationError::ContentIdentityMismatch {
                    field: "write offset",
                },
            )?;
            let count = pwrite_once(destination, write_offset, &buffer[written..found]).map_err(|source| {
                RetainedBootFilePublicationError::Filesystem {
                    action: "writing one private boot-file chunk",
                    source,
                }
            })?;
            if count == 0 || count > found - written {
                return Err(RetainedBootFilePublicationError::ContentIdentityMismatch {
                    field: "private write progress",
                });
            }
            written += count;
            written_bytes = written_bytes.checked_add(count as u64).ok_or(
                RetainedBootFilePublicationError::ContentIdentityMismatch {
                    field: "written byte count",
                },
            )?;
            if written_bytes > limits.max_write_bytes {
                return Err(RetainedBootFilePublicationError::LengthLimitExceeded {
                    length: written_bytes,
                    limit: limits.max_write_bytes,
                });
            }
        }
        offset = offset.checked_add(found as u64).ok_or(
            RetainedBootFilePublicationError::ContentIdentityMismatch {
                field: "source offset",
            },
        )?;
        if offset < request.expected_length() {
            fault(FixtureRetainedBootFilePublicationFault::MidMultiChunkWrite)?;
        }
    }

    fault(FixtureRetainedBootFilePublicationFault::AfterFinalWriteBeforeSourceValidation)?;
    let mut probe = [0u8; 1];
    if source
        .read_at(request.expected_length(), &mut probe)
        .map_err(|source| RetainedBootFilePublicationError::Source { source })?
        != 0
    {
        return Err(RetainedBootFilePublicationError::ContentIdentityMismatch {
            field: "source terminal length",
        });
    }
    if offset != request.expected_length() || written_bytes != request.expected_length() {
        return Err(RetainedBootFilePublicationError::ContentIdentityMismatch {
            field: "source length",
        });
    }
    if xxh3.digest128() != request.expected_xxh3() {
        return Err(RetainedBootFilePublicationError::ContentIdentityMismatch { field: "source XXH3" });
    }
    let actual_sha256: [u8; 32] = sha256.finalize().into();
    if actual_sha256 != request.expected_sha256() {
        return Err(RetainedBootFilePublicationError::ContentIdentityMismatch {
            field: "source SHA-256",
        });
    }
    source
        .terminally_revalidate()
        .map_err(|source| RetainedBootFilePublicationError::Source { source })?;
    checkpoint(deadline)
}

fn pwrite_once(file: &File, offset: u64, bytes: &[u8]) -> io::Result<usize> {
    let offset = nix::libc::off_t::try_from(offset)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "boot-file write offset exceeds off_t"))?;
    // SAFETY: bytes and the retained writable descriptor remain live for the
    // one positional write. The syscall retains neither argument.
    let found = unsafe { nix::libc::pwrite(file.as_raw_fd(), bytes.as_ptr().cast(), bytes.len(), offset) };
    if found < 0 {
        Err(io::Error::last_os_error())
    } else {
        usize::try_from(found).map_err(|_| io::Error::other("pwrite returned an oversized byte count"))
    }
}

pub(super) fn checkpoint(deadline: Instant) -> Result<(), RetainedBootFilePublicationError> {
    if Instant::now() > deadline {
        Err(RetainedBootFilePublicationError::DeadlineExceeded { deadline })
    } else {
        Ok(())
    }
}
